//! Tier 3: synthetic keystrokes. Stubs.
//!
//! Typing the text key by key works in anything that takes keyboard focus,
//! which is why every dictation tool ships it. It is also the worst tier:
//! insert-only, layout-dependent (a synthetic `KeyA` produces whatever the
//! active layout maps it to), and slow enough per event that long insertions
//! visibly stream. Characters with no key on the current layout need a
//! per-platform unicode path, noted per target below.

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

/// macOS CGEvent keyboard synthesis. Stub.
///
/// Needs: `CGEventCreateKeyboardEvent` plus
/// `CGEventKeyboardSetUnicodeString`, which sidesteps layouts entirely by
/// attaching the literal string to a single event pair, and the same
/// Accessibility trust the AX tier needs. When AX trust exists the AX tier
/// is strictly better, so on macOS this only matters for apps that take
/// keys but expose no AX field, which is exactly the secure-input and
/// game-window cases where synthesis is often blocked too.
pub struct CgEventTarget;

impl TextTarget for CgEventTarget {
    fn name(&self) -> &'static str {
        "macos-cgevent"
    }

    fn tier(&self) -> Tier {
        Tier::SyntheticKeys
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::insert_only(false)
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::NotReadable("keystroke synthesis cannot read"))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "CGEvent keystroke synthesis not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "keystroke synthesis cannot address existing text",
        ))
    }
}

/// Windows `SendInput` synthesis with `KEYEVENTF_UNICODE`.
///
/// The one platform where the unicode path is first-class: each UTF-16 code
/// unit rides a KEYBDINPUT with the UNICODE flag, so arbitrary text lands
/// without layout translation (the layout-dependence trap in the module
/// docs simply does not apply). Whole strings go in ONE SendInput call:
/// the batch is atomic with respect to other input injection, which
/// prevents interleaving with real user keystrokes mid-utterance.
///
/// Known blockers, both by design of the OS:
/// - **UIPI**: injection into a window of higher integrity (an elevated
///   app) is silently discarded; SendInput reports success. Documented in
///   docs/compat-matrix.md rather than detected, because there is no
///   supported way to ask "did the target accept it".
/// - Anti-cheat and secure-desktop (UAC prompt, login screen) input paths
///   ignore injected input entirely.
pub struct SendInputTarget;

impl TextTarget for SendInputTarget {
    fn name(&self) -> &'static str {
        "windows-sendinput"
    }

    fn tier(&self) -> Tier {
        Tier::SyntheticKeys
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::insert_only(false)
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::NotReadable("keystroke synthesis cannot read"))
    }

    #[cfg(all(target_os = "windows", feature = "display"))]
    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE, VIRTUAL_KEY,
        };

        // Two INPUTs per UTF-16 unit: down then up. Some applications
        // (notably ones translating back through ToUnicode) drop unicode
        // events that have no up transition, so both edges are sent even
        // though the down alone usually suffices.
        let mut inputs: Vec<INPUT> = Vec::with_capacity(text.encode_utf16().count() * 2);
        for unit in text.encode_utf16() {
            for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            // wVk must be zero for KEYEVENTF_UNICODE; the
                            // code unit travels in wScan.
                            wVk: VIRTUAL_KEY(0),
                            wScan: unit,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
        }
        if inputs.is_empty() {
            return Ok(());
        }
        // SAFETY: `inputs` is a valid, correctly-sized INPUT array and
        // SendInput does not retain the pointer past the call.
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            // Partial sends happen when input is blocked (UIPI, BlockInput,
            // secure desktop). Partial TEXT is worse than none for the
            // caller's retry logic, but there is no way to unsend; report
            // honestly.
            return Err(TargetError::Transport(format!(
                "SendInput delivered {}/{} events (input blocked by UIPI or secure desktop?)",
                sent,
                inputs.len()
            )));
        }
        Ok(())
    }

    #[cfg(not(all(target_os = "windows", feature = "display")))]
    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "SendInput exists only on Windows display builds",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "keystroke synthesis cannot address existing text",
        ))
    }
}

/// Linux uinput virtual keyboard (what `ydotool` wraps). Stub.
///
/// Needs: write access to `/dev/uinput` (root or a udev rule), and a
/// layout-matching keymap because uinput emits scancodes, not characters,
/// the exact problem `wtype` solves on Wayland by going through the
/// virtual-keyboard protocol with a custom keymap per unusual character.
/// Works on X11, Wayland, and even the raw console, which no other
/// graphical tier does.
pub struct UinputTarget;

impl TextTarget for UinputTarget {
    fn name(&self) -> &'static str {
        "linux-uinput"
    }

    fn tier(&self) -> Tier {
        Tier::SyntheticKeys
    }

    fn capabilities(&self) -> Capabilities {
        // Console works without a display server, hence headless-capable.
        Capabilities::insert_only(true)
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::NotReadable("keystroke synthesis cannot read"))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "uinput keystroke synthesis not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "keystroke synthesis cannot address existing text",
        ))
    }
}
