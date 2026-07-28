//! Windows backend: a low-level keyboard hook (`WH_KEYBOARD_LL`) on a
//! dedicated message-pump thread.
//!
//! ## Why a hook and not RegisterHotKey
//!
//! `RegisterHotKey` is the polite API and has genuinely better conflict
//! detection than anything on macOS: registration FAILS with
//! `ERROR_HOTKEY_ALREADY_REGISTERED` if any other process holds the chord.
//! But it is unusable as the primary mechanism for push-to-talk, for two
//! hard reasons:
//!
//! 1. It only delivers `WM_HOTKEY` on the DOWN edge. There is no release
//!    notification at all, and PTT is *defined* by the release ("release to
//!    commit"). Polling `GetAsyncKeyState` to synthesize the up edge adds
//!    latency and a busy loop, and misses fast taps entirely.
//! 2. It cannot bind a bare modifier (right Alt as PTT, the nearest
//!    equivalent of the product's right-Option default): the API requires a
//!    non-modifier virtual key.
//!
//! The low-level hook sees every transition system-wide, both edges, with
//! side-specific modifier VKs. Its costs are known and handled:
//!
//! - **The timeout unhook.** If the hook callback ever exceeds
//!   `LowLevelHooksTimeout` (default 300ms, HKCU Control Panel\Desktop),
//!   Windows silently removes the hook. No notification exists. Mitigation
//!   one: the callback is allocation-free and never blocks (state is under
//!   a try_lock; a miss drops one observation instead of stalling input).
//!   Mitigation two, follow-up work noted in docs/hotkeys.md: a periodic
//!   liveness self-check that re-installs the hook if the OS dropped it and
//!   resets the matcher/state machine, because a swallowed key-up would
//!   otherwise leave the mic hot (the worst trust failure available).
//! - **UIPI.** When an *elevated* (admin) window has focus, a non-elevated
//!   process's hook does not see its keystrokes at all: User Interface
//!   Privilege Isolation blocks input observation and injection across
//!   integrity levels. The hotkey simply does not fire while an elevated
//!   app is focused. This is documented user-facing behaviour
//!   (docs/hotkeys.md), not a bug we can code around; the only fixes are
//!   running elevated ourselves (bad default) or the uiAccess manifest
//!   route (requires signing + installation under Program Files).
//!
//! ## Conflict detection
//!
//! At bind time, for keyed chords, we do a probe: `RegisterHotKey`, note
//! success/failure, `UnregisterHotKey` immediately. Failure means some
//! other app holds the chord and the user should be warned (advisory, per
//! the UX doc; the hook still sees the events either way). Bare-modifier
//! chords skip the probe because RegisterHotKey cannot express them.

use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Instant;

use crate::matcher::Matcher;
use crate::taphold::TapHold;
use crate::winmatch::WinMatcher;
use crate::{HotkeyError, HotkeyEvent};

#[allow(clippy::upper_case_acronyms)]
mod ffi {
    // Hand-written bindings for the handful of user32 calls this backend
    // needs, mirroring how the macOS backend binds CoreGraphics: a small
    // extern block keeps the supply-chain surface where it already was
    // instead of adding the full `windows` crate for six functions.
    use std::ffi::c_void;

    pub type HHOOK = *mut c_void;
    pub type HINSTANCE = *mut c_void;
    pub type HWND = *mut c_void;
    pub type WPARAM = usize;
    pub type LPARAM = isize;
    pub type LRESULT = isize;

    pub const WH_KEYBOARD_LL: i32 = 13;
    pub const WM_KEYDOWN: usize = 0x0100;
    pub const WM_KEYUP: usize = 0x0101;
    pub const WM_SYSKEYDOWN: usize = 0x0104;
    pub const WM_SYSKEYUP: usize = 0x0105;

    /// KBDLLHOOKSTRUCT (winuser.h). Layout is ABI, not a guess.
    #[repr(C)]
    pub struct KBDLLHOOKSTRUCT {
        pub vk_code: u32,
        pub scan_code: u32,
        pub flags: u32,
        pub time: u32,
        pub extra_info: usize,
    }

    #[repr(C)]
    pub struct POINT {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    pub struct MSG {
        pub hwnd: HWND,
        pub message: u32,
        pub w_param: WPARAM,
        pub l_param: LPARAM,
        pub time: u32,
        pub pt: POINT,
    }

    pub type HookProc =
        unsafe extern "system" fn(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT;

    #[link(name = "user32")]
    extern "system" {
        pub fn SetWindowsHookExW(
            id_hook: i32,
            lpfn: HookProc,
            hmod: HINSTANCE,
            thread_id: u32,
        ) -> HHOOK;
        pub fn UnhookWindowsHookEx(hhk: HHOOK) -> i32;
        pub fn CallNextHookEx(hhk: HHOOK, code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT;
        pub fn GetMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32) -> i32;
        pub fn RegisterHotKey(hwnd: HWND, id: i32, modifiers: u32, vk: u32) -> i32;
        pub fn UnregisterHotKey(hwnd: HWND, id: i32) -> i32;
        pub fn GetLastError() -> u32;
    }
}

/// Everything the hook callback needs. A process-global because the Win32
/// hook procedure is a bare function pointer with no user-data argument;
/// one binding per process is the same constraint the manager already
/// documents ("one manager per binding").
struct HookState {
    matcher: WinMatcher,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
}

static STATE: Mutex<Option<HookState>> = Mutex::new(None);

/// The hook procedure. Runs on the message-pump thread, inside the OS input
/// dispatch path: everything here must be fast (see module docs on the
/// LowLevelHooksTimeout unhook). The mutex is uncontended in steady state,
/// as the only other lockers are bind and the recovery path.
unsafe extern "system" fn hook_proc(
    code: i32,
    w_param: ffi::WPARAM,
    l_param: ffi::LPARAM,
) -> ffi::LRESULT {
    if code >= 0 {
        let kb = &*(l_param as *const ffi::KBDLLHOOKSTRUCT);
        let is_down = matches!(w_param, ffi::WM_KEYDOWN | ffi::WM_SYSKEYDOWN);
        let is_up = matches!(w_param, ffi::WM_KEYUP | ffi::WM_SYSKEYUP);
        if is_down || is_up {
            if let Ok(mut guard) = STATE.try_lock() {
                if let Some(state) = guard.as_mut() {
                    if let Some(edge) = state.matcher.feed(kb.vk_code, is_down) {
                        let now = Instant::now();
                        let events = match edge {
                            crate::matcher::Edge::Down => state.machine.on_key_down(now),
                            crate::matcher::Edge::Up => state.machine.on_key_up(now),
                        };
                        for e in events {
                            // A disconnected receiver means the manager was
                            // dropped; nothing useful to do from inside the
                            // hook, so events are discarded, matching the
                            // documented drop semantics.
                            let _ = state.sender.send(e);
                        }
                    }
                }
            }
            // try_lock failure means bind/recovery holds the lock this
            // instant; dropping one observation is better than blocking the
            // input path and getting unhooked for good.
        }
    }
    // Always pass the event on: this hook observes, never swallows.
    ffi::CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param)
}

/// Probe RegisterHotKey purely for conflict intelligence: if another app
/// owns the chord, registration fails with ERROR_HOTKEY_ALREADY_REGISTERED
/// (1409). The registration is undone immediately; the hook is the real
/// mechanism. Returns whether the chord was already claimed.
fn probe_conflict(chord_vk: u32, chord_mods: u32) -> bool {
    const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;
    // Arbitrary app-local id; ids only need to be unique within our thread.
    const PROBE_ID: i32 = 0x4151; // "AQ"
    unsafe {
        if ffi::RegisterHotKey(std::ptr::null_mut(), PROBE_ID, chord_mods, chord_vk) != 0 {
            ffi::UnregisterHotKey(std::ptr::null_mut(), PROBE_ID);
            false
        } else {
            ffi::GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED
        }
    }
}

/// MOD_* flags for RegisterHotKey (winuser.h), for the conflict probe only.
fn probe_mods(chord: &crate::chord::Chord) -> u32 {
    use crate::chord::Modifier;
    let mut m = 0;
    for md in &chord.mods {
        m |= match md {
            Modifier::Option => 0x0001,  // MOD_ALT
            Modifier::Control => 0x0002, // MOD_CONTROL
            Modifier::Shift => 0x0004,   // MOD_SHIFT
            Modifier::Command => 0x0008, // MOD_WIN
            Modifier::Fn => 0,           // unreachable: WinMatcher::new refused it
        };
    }
    m
}

pub fn spawn(
    chord: &crate::chord::Chord,
    matcher: Matcher,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    // The pre-compiled Matcher speaks macOS event vocabulary (CGEvent
    // types and NX flag bits) and is unused here; the Windows matcher is
    // compiled fresh from the chord.
    drop(matcher);

    let win_matcher = WinMatcher::new(chord).map_err(|e| HotkeyError::BadChord(e.to_string()))?;

    // Advisory conflict probe (keyed chords only; RegisterHotKey cannot
    // express bare modifiers). The result is currently only logged: wiring
    // it into HotkeyManager::conflicts requires plumbing a return value
    // through spawn and is noted in docs/hotkeys.md as follow-up.
    if let Some(k) = chord.key {
        if !k.is_bare_modifier() {
            if let Some(vk) = crate::winmatch::vk_for_key(k) {
                if probe_conflict(vk, probe_mods(chord)) {
                    eprintln!(
                        "hotkey: warning: '{chord}' is already registered by another \
                         application (RegisterHotKey probe); both will act on it"
                    );
                }
            }
        }
    }

    *STATE.lock().unwrap() = Some(HookState {
        matcher: win_matcher,
        machine,
        sender,
    });

    // Install the hook from the thread that will pump messages: a
    // WH_KEYBOARD_LL callback is delivered via the message queue of the
    // thread that installed it, so installing from a thread without a pump
    // yields a hook that never fires. The channel confirms installation so
    // `spawn` keeps its "Ok means live" contract.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("hotkey-hook".into())
        .spawn(move || {
            let hook = unsafe {
                ffi::SetWindowsHookExW(ffi::WH_KEYBOARD_LL, hook_proc, std::ptr::null_mut(), 0)
            };
            if hook.is_null() {
                let err = unsafe { ffi::GetLastError() };
                let _ = ready_tx.send(Err(format!("SetWindowsHookExW failed (error {err})")));
                return;
            }
            let _ = ready_tx.send(Ok(()));

            // Classic message pump. GetMessageW blocks; the hook callback
            // is dispatched to this thread while it waits. The loop runs
            // for the process lifetime, matching the manager's documented
            // drop semantics (backend thread outlives the manager).
            let mut msg: ffi::MSG = unsafe { std::mem::zeroed() };
            loop {
                let rc = unsafe { ffi::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
                if rc <= 0 {
                    break; // WM_QUIT or error: tear down.
                }
            }
            unsafe { ffi::UnhookWindowsHookEx(hook) };
        })
        .map_err(|e| HotkeyError::Backend(format!("failed to spawn hook thread: {e}")))?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => Err(HotkeyError::Backend(msg)),
        Err(_) => Err(HotkeyError::Backend(
            "hook thread died before confirming installation".into(),
        )),
    }
}
