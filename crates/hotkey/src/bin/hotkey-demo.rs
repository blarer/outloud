//! Live verification harness: bind a chord, print every event with a
//! timestamp. Exists because the event tap CANNOT be exercised by unit
//! tests (it needs a real window server, real permission, and real fingers),
//! and an untested primary interaction is not done.
//!
//! Usage:
//!   hotkey-demo [chord] [tap-threshold-ms]
//!   hotkey-demo right-option
//!   hotkey-demo fn
//!   hotkey-demo cmd+shift+space 250
//!   hotkey-demo --selftest    # macOS: post synthetic HID events through
//!                             # the real tap and assert the event sequence
//!
//! Note the permission trap: run from a terminal, the responsible process
//! is the TERMINAL, so the terminal needs Accessibility trust
//! (docs/macos-permissions.md). This is fine for a dev harness.

use std::time::{Duration, Instant};

use hotkey::{Chord, HotkeyEvent, HotkeyManager, Timing};

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next();

    #[cfg(target_os = "macos")]
    if first.as_deref() == Some("--selftest") {
        std::process::exit(selftest::run());
    }

    // Everywhere else the selftest would need that platform's synthetic
    // input API (SendInput on Windows), and injecting keys to test the hook
    // that observes them is a loop worth building deliberately rather than
    // as a demo side effect. Say so instead of silently binding the literal
    // chord "--selftest" and looking broken.
    #[cfg(not(target_os = "macos"))]
    if first.as_deref() == Some("--selftest") {
        eprintln!(
            "hotkey-demo: --selftest is macOS-only (it posts synthetic HID events through \
             the real tap). On Windows, hold the chord by hand and watch the events below."
        );
        std::process::exit(2);
    }

    let chord: Chord = match first {
        Some(s) => match s.parse() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("bad chord '{s}': {e}");
                std::process::exit(2);
            }
        },
        None => Chord::right_option(),
    };
    let timing = match args.next() {
        Some(ms) => match ms.parse::<u64>() {
            Ok(ms) => Timing {
                tap_threshold: Duration::from_millis(ms),
            },
            Err(_) => {
                eprintln!("bad threshold '{ms}' (want milliseconds)");
                std::process::exit(2);
            }
        },
        None => Timing::default(),
    };

    println!(
        "binding '{chord}' (tap threshold {}ms)...",
        timing.tap_threshold.as_millis()
    );

    let manager = match HotkeyManager::bind(chord, timing) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("bind failed: {e}");
            std::process::exit(1);
        }
    };

    if manager.conflicts().is_empty() {
        println!("no conflicts detected for '{}'", manager.chord());
    } else {
        for c in manager.conflicts() {
            println!("WARNING: '{}' is {c}", manager.chord());
        }
    }
    println!("listening. tap = latch, hold = push-to-talk. ctrl-c to quit.");

    let start = Instant::now();
    for event in manager.events() {
        let t = start.elapsed();
        let label = match event {
            HotkeyEvent::Pressed => "PRESSED   (capture start)",
            HotkeyEvent::Released => "RELEASED  (PTT commit)",
            HotkeyEvent::Latched => "LATCHED   (capture stays live)",
            HotkeyEvent::Unlatched => "UNLATCHED (latch commit)",
            HotkeyEvent::TapRecovered => "TAP RECOVERED (OS disabled it; re-enabled)",
        };
        println!("[{:>8.3}s] {label}", t.as_secs_f64());
    }
}

/// Automated live verification: posts synthetic HID keyboard events through
/// the real window server, so the real CGEventTap, matcher, and tap/hold
/// machine all run exactly as they do for physical keys. F13 is used because
/// no macOS default binds it, so the synthetic presses trigger nothing else.
///
/// This is a supplement to, not a replacement for, pressing real keys: it
/// cannot exercise the Fn key (CGEventPost cannot synthesize the fn flags
/// change from a plain process) or verify what a physical keyboard's HID
/// driver emits.
#[cfg(target_os = "macos")]
mod selftest {
    use super::*;
    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            keycode: u16,
            keydown: bool,
        ) -> *const c_void;
        fn CGEventPost(tap_location: u32, event: *const c_void);
        fn CFRelease(cf: *const c_void);
    }

    const HID_TAP: u32 = 0; // kCGHIDEventTap: same insertion point we listen at.
    const F13: u16 = 105;

    fn press(down: bool) {
        unsafe {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null(), F13, down);
            assert!(!ev.is_null(), "CGEventCreateKeyboardEvent failed");
            CGEventPost(HID_TAP, ev);
            CFRelease(ev);
        }
    }

    fn drain(manager: &HotkeyManager, wait: Duration) -> Vec<HotkeyEvent> {
        let deadline = Instant::now() + wait;
        let mut out = Vec::new();
        while let Ok(ev) = manager
            .events()
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            out.push(ev);
        }
        out
    }

    pub fn run() -> i32 {
        let chord: Chord = "f13".parse().expect("f13 parses");
        let manager = match HotkeyManager::bind(chord, Timing::default()) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("selftest: bind failed: {e}");
                return 1;
            }
        };
        println!(
            "selftest: bound '{}', posting synthetic F13 events",
            manager.chord()
        );

        // Scenario 1: hold 500ms (past the 300ms threshold) -> Pressed, Released.
        press(true);
        std::thread::sleep(Duration::from_millis(500));
        press(false);
        let got = drain(&manager, Duration::from_millis(400));
        let want = vec![HotkeyEvent::Pressed, HotkeyEvent::Released];
        println!("selftest: hold      -> {got:?}");
        if got != want {
            eprintln!("selftest FAIL: hold expected {want:?}");
            return 1;
        }

        // Scenario 2: tap 100ms -> Pressed, Latched; second tap -> Unlatched.
        press(true);
        std::thread::sleep(Duration::from_millis(100));
        press(false);
        let got = drain(&manager, Duration::from_millis(400));
        let want = vec![HotkeyEvent::Pressed, HotkeyEvent::Latched];
        println!("selftest: tap       -> {got:?}");
        if got != want {
            eprintln!("selftest FAIL: tap expected {want:?}");
            return 1;
        }
        press(true);
        std::thread::sleep(Duration::from_millis(50));
        press(false);
        let got = drain(&manager, Duration::from_millis(400));
        let want = vec![HotkeyEvent::Unlatched];
        println!("selftest: untap     -> {got:?}");
        if got != want {
            eprintln!("selftest FAIL: untap expected {want:?}");
            return 1;
        }

        println!("selftest: PASS (real tap, real window server, synthetic keys)");
        0
    }
}
