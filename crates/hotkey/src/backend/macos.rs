//! The macOS backend: a CGEventTap watching keyboard events system-wide.
//!
//! WHY CGEventTap and not NSEvent.addGlobalMonitorForEvents:
//!
//! Push-to-talk needs BOTH edges (down and up) of whatever key is bound,
//! anywhere in the system. The global NSEvent monitor delivers keyDown/keyUp
//! for ordinary keys, but the Fn/Globe key never produces those: it only
//! surfaces as a `flagsChanged` with `NSEvent.ModifierFlags.function`, and
//! the global monitor's view of flagsChanged is filtered enough (and offers
//! no device-level left/right bits in some paths) that bare-modifier
//! bindings become unreliable. A `kCGHIDEventTap`-level CGEventTap sees the
//! raw HID-derived stream: every keyDown/keyUp/flagsChanged, with the full
//! CGEventFlags including the device-specific left/right bits the matcher
//! needs. The cost is Accessibility permission for a listen-only tap, which
//! this product ALREADY requires for text editing (see
//! docs/macos-permissions.md), so the tap is free here. We create the tap as
//! `kCGEventTapOptionListenOnly`: we never consume or modify events, which
//! keeps us out of the latency path of every keystroke the user types and
//! means a stall in this process can at worst lose OUR hotkey, never wedge
//! the system's typing. (Suppressing the bound key from reaching other apps
//! is a future concern that would require an active tap; not needed while
//! the defaults are bare modifiers that type nothing.)
//!
//! Threading model: the tap runs on a dedicated thread with its own
//! CFRunLoop. The callback does only three cheap things: read two integers
//! off the event, run the pure matcher + tap/hold state machine, and push
//! resulting events into an unbounded channel. No allocation-heavy work, no
//! locks shared with slow code, no IO. This is a hard rule: the callback
//! executes on the window server's event-dispatch path, and if it exceeds
//! the tap timeout macOS disables the tap, i.e. being slow here doesn't
//! just stutter input, it kills our own hotkey.
//!
//! The disable trap: macOS sends `kCGEventTapDisabledByTimeout` INTO the
//! callback when it has disabled the tap for slowness (system sleep/wake and
//! heavy load can trigger it without any fault of ours), and
//! `kCGEventTapDisabledByUserInput` when e.g. secure input toggles. We
//! re-enable immediately in the callback and reset the matcher + state
//! machine, because a key-up may have been swallowed while dead and a
//! machine stuck in "pressed" would keep the mic hot forever.

#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::matcher::{
    Edge, Matcher, EVENT_FLAGS_CHANGED, EVENT_KEY_DOWN, EVENT_KEY_UP,
    EVENT_TAP_DISABLED_BY_TIMEOUT, EVENT_TAP_DISABLED_BY_USER_INPUT,
};
use crate::taphold::TapHold;
use crate::{HotkeyError, HotkeyEvent};

// --- CoreGraphics FFI ------------------------------------------------------
// Hand-written extern block instead of the `core-graphics` crate: we need
// five functions, and every new dependency is a supply-chain review burden
// (deny.toml checks eight targets). Types follow CGEventTypes.h.

type CGEventTapProxy = *const c_void;
type CGEventRef = *const c_void;
type CFMachPortRef = *const c_void;
type CFRunLoopSourceRef = *const c_void;
type CFRunLoopRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFIndex = isize;
type CFStringRef = *const c_void;

// kCGHIDEventTap: earliest insertion point, sees events as the HID system
// posts them, before app-level remapping.
const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
// Listen-only per the module doc.
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
// kCGKeyboardEventKeycode field id.
const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

/// `kIOHIDRequestTypeListenEvent`: may this process observe HID input?
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
/// `kIOHIDAccessTypeGranted`.
const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
}

/// Whether this process may observe keyboard input, i.e. whether the user
/// has granted **Input Monitoring**.
///
/// This is a DIFFERENT permission from Accessibility, and confusing the two
/// costs hours. `kCGHIDEventTap` sits at the HID layer and needs Input
/// Monitoring; Accessibility is what lets us read and write text in other
/// applications. A daemon with Accessibility but not Input Monitoring binds
/// nothing and reports itself healthy, which is exactly the failure a user
/// hit: the tray said ready, the hotkey did nothing, and no surface named
/// the missing grant.
///
/// Never prompts. `IOHIDRequestAccess` would show a dialog, which is wrong
/// on a poll; the caller deep-links to the pane instead.
pub fn has_input_monitoring() -> bool {
    // Safety: a pure query with no arguments beyond a constant.
    unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) == K_IOHID_ACCESS_TYPE_GRANTED }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRelease(cf: *const c_void);
}

// --- The tap thread --------------------------------------------------------

/// Everything the callback needs, boxed and leaked to the tap thread for the
/// life of the process. The manager owns bindings for its whole lifetime, so
/// a deliberate leak is simpler and safer than reference counting across an
/// FFI boundary that outlives Rust scopes.
struct TapState {
    matcher: Matcher,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
    tap: CFMachPortRef,
}

// The raw pointer fields keep TapState from being auto-Send; the tap port is
// only ever touched from the tap thread after creation hands it over.
unsafe impl Send for TapState {}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    // Safety: user_info is the Box::into_raw'd TapState owned by this tap,
    // freed never (process-lifetime binding).
    let state = unsafe { &mut *(user_info as *mut TapState) };

    match event_type {
        EVENT_TAP_DISABLED_BY_TIMEOUT | EVENT_TAP_DISABLED_BY_USER_INPUT => {
            // THE trap: without this the hotkey silently dies after any
            // stall. Re-enable, then reset state because edges were lost.
            unsafe { CGEventTapEnable(state.tap, true) };
            state.matcher.reset();
            for ev in state.machine.reset() {
                let _ = state.sender.send(ev);
            }
            let _ = state.sender.send(HotkeyEvent::TapRecovered);
            return event;
        }
        EVENT_KEY_DOWN | EVENT_KEY_UP | EVENT_FLAGS_CHANGED => {}
        _ => return event,
    }

    let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };
    let flags = unsafe { CGEventGetFlags(event) };

    // Debug tracing gated by a once-read env var: the check must not do an
    // environment lookup per keystroke on the event-dispatch thread.
    static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *DEBUG.get_or_init(|| std::env::var_os("HOTKEY_DEBUG").is_some()) {
        eprintln!("tap: type={event_type} keycode={keycode} flags={flags:#x}");
    }

    if let Some(edge) = state.matcher.feed(event_type, keycode, flags) {
        let now = Instant::now();
        let evs = match edge {
            Edge::Down => state.machine.on_key_down(now),
            Edge::Up => state.machine.on_key_up(now),
        };
        for ev in evs {
            // send() on an unbounded channel never blocks; a disconnected
            // receiver just drops the event, which is fine (manager gone).
            let _ = state.sender.send(ev);
        }
    }
    event
}

/// Create the tap on a fresh thread and run its loop forever.
/// Returns once the tap is confirmed created (or failed).
pub fn spawn(
    matcher: Matcher,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), HotkeyError>>();

    std::thread::Builder::new()
        .name("hotkey-event-tap".into())
        .spawn(move || {
            let state = Box::into_raw(Box::new(TapState {
                matcher,
                machine,
                sender,
                tap: std::ptr::null(),
            }));

            let mask =
                (1u64 << EVENT_KEY_DOWN) | (1u64 << EVENT_KEY_UP) | (1u64 << EVENT_FLAGS_CHANGED);

            // Safety: callback + user_info stay valid for the process
            // lifetime (leaked box, never-exiting thread).
            let tap = unsafe {
                CGEventTapCreate(
                    K_CG_HID_EVENT_TAP,
                    K_CG_HEAD_INSERT_EVENT_TAP,
                    K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                    mask,
                    tap_callback,
                    state as *mut c_void,
                )
            };
            if tap.is_null() {
                // Null means the OS refused: virtually always missing
                // Input Monitoring (this tap's actual requirement) or
                // Accessibility trust for the responsible process (see
                // docs/macos-permissions.md for why "the responsible
                // process" may be your terminal).
                let _ = ready_tx.send(Err(HotkeyError::PermissionDenied));
                // Reclaim the state we leaked for a tap that never existed.
                drop(unsafe { Box::from_raw(state) });
                return;
            }
            unsafe { (*state).tap = tap };

            unsafe {
                let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                if source.is_null() {
                    let _ = ready_tx.send(Err(HotkeyError::Backend(
                        "CFMachPortCreateRunLoopSource returned null".into(),
                    )));
                    CFRelease(tap);
                    drop(Box::from_raw(state));
                    return;
                }
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, true);
                let _ = ready_tx.send(Ok(()));
                // Runs forever; the thread is the binding's lifetime.
                CFRunLoopRun();
            }
        })
        .map_err(|e| HotkeyError::Backend(format!("failed to spawn tap thread: {e}")))?;

    ready_rx
        .recv()
        .map_err(|_| HotkeyError::Backend("tap thread died before reporting readiness".into()))?
}
