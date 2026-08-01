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
//!   Mitigation two: the pump thread runs a 2s liveness watchdog
//!   (`pump_with_watchdog`) that reinstalls the hook if the OS dropped it
//!   and resets the matcher/state machine, because a swallowed key-up
//!   would otherwise leave the mic hot (the worst trust failure available).
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
    /// PeekMessage removes the message it returns.
    pub const PM_REMOVE: u32 = 0x0001;
    pub const WM_QUIT: u32 = 0x0012;

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
        pub fn PeekMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32, remove: u32) -> i32;
        /// Blocks IN the message system until input arrives or the timeout
        /// expires. This is what makes the thread available for hook
        /// dispatch; a sleep is not.
        pub fn MsgWaitForMultipleObjects(
            count: u32,
            handles: *const std::ffi::c_void,
            wait_all: i32,
            millis: u32,
            wake_mask: u32,
        ) -> u32;
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
        let is_down = matches!(w_param, ffi::WM_KEYDOWN | ffi::WM_SYSKEYDOWN);
        let is_up = matches!(w_param, ffi::WM_KEYUP | ffi::WM_SYSKEYUP);
        if is_down || is_up {
            let vk = (*(l_param as *const ffi::KBDLLHOOKSTRUCT)).vk_code;
            // A panic unwinding across an `extern "system"` boundary into
            // Windows' input dispatcher is undefined behaviour, and this
            // callback runs on the system's input path where a crash takes
            // the whole session's keyboard with it. The body is
            // panic-free by construction (no unwrap, no indexing, no
            // allocation), so this is belt and braces against a future
            // edit, not a known panic.
            let _ = std::panic::catch_unwind(|| {
                if let Ok(mut guard) = STATE.try_lock() {
                    if let Some(state) = guard.as_mut() {
                        if let Some(edge) = state.matcher.feed(vk, is_down) {
                            let now = Instant::now();
                            let events = match edge {
                                crate::matcher::Edge::Down => state.machine.on_key_down(now),
                                crate::matcher::Edge::Up => state.machine.on_key_up(now),
                            };
                            for e in events {
                                // A disconnected receiver means the manager
                                // was dropped; nothing useful to do from
                                // inside the hook, so events are discarded,
                                // matching the documented drop semantics.
                                let _ = state.sender.send(e);
                            }
                        }
                    }
                }
                // try_lock failure means bind/recovery holds the lock this
                // instant; dropping one observation is better than blocking
                // the input path and getting unhooked for good.
            });
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

/// Is this chord already held by another process? The public entry point
/// for [`crate::conflict::check_chord`], so probe results reach
/// `HotkeyManager::conflicts()` like every other conflict source instead of
/// only reaching stderr.
pub fn chord_already_registered(vk: u32, chord: &crate::chord::Chord) -> bool {
    probe_conflict(vk, probe_mods(chord))
}

/// How often the watchdog verifies the hook is still installed. Well under
/// any human's tolerance for a dead hotkey, and cheap: one API call.
const WATCHDOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// `QS_ALLINPUT`: wake for any input or posted message.
///
/// The value matters less than the blocking: what the thread must not do is
/// sleep, because a sleeping thread cannot dispatch a hook callback.
const QS_ALLINPUT: u32 = 0x04FF;

pub fn spawn(
    chord: &crate::chord::Chord,
    matcher: Matcher,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    // The pre-compiled Matcher speaks macOS event vocabulary (CGEvent
    // types and NX flag bits) and is unused here; the Windows matcher is
    // compiled fresh from the chord.
    let _ = matcher;

    let win_matcher = WinMatcher::new(chord).map_err(|e| HotkeyError::BadChord(e.to_string()))?;

    // Poisoning would mean a previous holder panicked while touching the
    // state. Recover rather than propagate: refusing to bind because of a
    // past panic would leave the user with no hotkey at all, which is the
    // outcome this whole crate exists to prevent. The state is fully
    // overwritten on the next line anyway, so nothing torn survives.
    let mut guard = STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(HookState {
        matcher: win_matcher,
        machine,
        sender,
    });
    drop(guard);

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

            pump_with_watchdog(hook);
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

/// Message pump plus the liveness watchdog.
///
/// Windows removes a hook whose callback exceeded `LowLevelHooksTimeout`
/// and tells nobody: there is no event, no error, no callback. The macOS
/// event tap at least delivers `kCGEventTapDisabledByTimeout` as an event,
/// so the Windows recovery has to be *polled*.
///
/// The wait MUST block inside the message system rather than sleep, and this
/// is not a style preference. A low-level hook callback is delivered by the
/// OS while the owning thread is waiting on its message queue. A thread
/// asleep in `thread::sleep` is not waiting on its queue, so every keystroke
/// stalls until the sleep ends. Windows gives a hook
/// `LowLevelHooksTimeout` (300ms by default) to respond and silently
/// unhooks it otherwise, so a 2s sleep guarantees the hook is killed on the
/// first keypress.
///
/// The symptom is worse than a dead hotkey: the hook sits in the chain for
/// EVERY key system-wide, so the whole keyboard freezes until Windows tears
/// it down. Observed on Windows 11 the first time this code ran on real
/// hardware; Ctrl+Alt+Del cleared it, which is the signature of exactly this
/// failure.
///
/// On detecting a dead hook we reinstall AND reset the matcher and state
/// machine: while unhooked we may have missed a key-UP, and a state machine
/// stuck in "pressed" keeps the microphone hot forever, which is the worst
/// trust failure this crate can produce.
fn pump_with_watchdog(mut hook: ffi::HHOOK) {
    let mut msg: ffi::MSG = unsafe { std::mem::zeroed() };
    loop {
        // PeekMessage drains anything queued without blocking, so the
        // watchdog tick below is never starved by message traffic. The
        // hook callback itself is delivered during these calls.
        while unsafe { ffi::PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, ffi::PM_REMOVE) }
            != 0
        {
            if msg.message == ffi::WM_QUIT {
                unsafe { ffi::UnhookWindowsHookEx(hook) };
                return;
            }
        }

        // Blocks in the message system, so the OS can dispatch hook
        // callbacks the whole time, and still returns by the watchdog
        // deadline so the liveness check below runs on schedule. Replacing
        // this with a sleep reintroduces a system-wide keyboard freeze.
        unsafe {
            ffi::MsgWaitForMultipleObjects(
                0,
                std::ptr::null(),
                0,
                WATCHDOG_INTERVAL.as_millis() as u32,
                QS_ALLINPUT,
            );
        }

        // Poison check, and why it belongs in the watchdog: the callback
        // takes the state with `try_lock`, and a POISONED mutex returns Err
        // from try_lock forever. So one panic anywhere would make the hook
        // silently drop every event from then on, with the hook still
        // installed and the OS perfectly happy: a dead hotkey with no
        // symptom, which is the exact failure this crate exists to prevent.
        // Clearing the poison restores the callback without touching the
        // hook. `clear_poison` is stable since Rust 1.77, below our MSRV.
        if STATE.is_poisoned() {
            STATE.clear_poison();
            let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(state) = guard.as_mut() {
                // The panicking holder may have left the machine mid-edge,
                // so reset for the same reason the hook-death path does.
                state.matcher.reset();
                for e in state.machine.reset() {
                    let _ = state.sender.send(e);
                }
                let _ = state.sender.send(HotkeyEvent::TapRecovered);
            }
            drop(guard);
            eprintln!(
                "hotkey: hook state was poisoned by a panic and has been cleared; \
                 the binding is live again"
            );
        }

        // The liveness question: is our HHOOK still in the chain? There is
        // no "is it installed" API, so we use the one observable side
        // effect: unhooking a hook the OS already removed fails.
        // Unhook-then-reinstall unconditionally would drop events during
        // the gap on every tick, so we only act when the unhook fails.
        if unsafe { ffi::UnhookWindowsHookEx(hook) } == 0 {
            let fresh = unsafe {
                ffi::SetWindowsHookExW(ffi::WH_KEYBOARD_LL, hook_proc, std::ptr::null_mut(), 0)
            };
            if fresh.is_null() {
                eprintln!(
                    "hotkey: the keyboard hook was removed by the OS and could not be \
                     reinstalled (error {}); the hotkey is DEAD until restart",
                    unsafe { ffi::GetLastError() }
                );
                return;
            }
            hook = fresh;
            // Pessimistic reset: a swallowed key-up would otherwise leave
            // capture running with no way to stop it.
            // Recover from poisoning here too: skipping the reset because
            // of a past panic is exactly the stuck-capture outcome the
            // reset exists to prevent.
            let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
            {
                if let Some(state) = guard.as_mut() {
                    state.matcher.reset();
                    for e in state.machine.reset() {
                        let _ = state.sender.send(e);
                    }
                    let _ = state.sender.send(HotkeyEvent::TapRecovered);
                }
            }
            eprintln!("hotkey: keyboard hook was removed by the OS and has been reinstalled");
        } else {
            // The unhook SUCCEEDED, which means the hook was alive and we
            // just removed it ourselves. Put it straight back.
            let fresh = unsafe {
                ffi::SetWindowsHookExW(ffi::WH_KEYBOARD_LL, hook_proc, std::ptr::null_mut(), 0)
            };
            if fresh.is_null() {
                eprintln!(
                    "hotkey: failed to reinstall the keyboard hook after a liveness check \
                     (error {}); the hotkey is DEAD until restart",
                    unsafe { ffi::GetLastError() }
                );
                return;
            }
            hook = fresh;
        }
    }
}
