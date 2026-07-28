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

/// One synthetic key transition: a UTF-16 code unit and which edge it is.
///
/// Exists so the encoding decision is *data* that can be asserted on, rather
/// than being buried in an FFI call nobody can run on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnicodeKeyStep {
    /// The UTF-16 code unit, delivered in `wScan` (never `wVk`).
    pub unit: u16,
    /// False for the down edge, true for the matching up edge.
    pub key_up: bool,
}

/// The exact sequence of synthetic key transitions for `text`.
///
/// Two events per UTF-16 code unit, down then up: some applications
/// (notably ones translating back through `ToUnicode`) drop unicode events
/// that have no up transition, so both edges are always sent.
///
/// The subtle part is **surrogate pairs**. Anything outside the BMP (emoji,
/// and CJK extension characters real users type) encodes as TWO UTF-16
/// units, and `KEYEVENTF_UNICODE` requires both to be sent as their own
/// events, in order, in the SAME `SendInput` batch. Iterating over `chars`
/// and casting to u16 (the obvious-looking bug) would truncate every
/// astral character; splitting the batch between the two halves lets a real
/// keystroke interleave and can produce a lone surrogate, which renders as
/// a replacement glyph. Encoding through `encode_utf16` keeps both halves
/// adjacent and ordered by construction.
///
/// Pure and compiled on every platform, so the property is tested on macOS
/// CI rather than only on Windows hardware.
pub fn unicode_key_plan(text: &str) -> Vec<UnicodeKeyStep> {
    let mut out = Vec::with_capacity(text.encode_utf16().count() * 2);
    for unit in text.encode_utf16() {
        out.push(UnicodeKeyStep {
            unit,
            key_up: false,
        });
        out.push(UnicodeKeyStep { unit, key_up: true });
    }
    out
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

        let plan = unicode_key_plan(text);
        let mut inputs: Vec<INPUT> = Vec::with_capacity(plan.len());
        for step in &plan {
            let mut flags = KEYEVENTF_UNICODE;
            if step.key_up {
                flags |= KEYEVENTF_KEYUP;
            }
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        // wVk must be zero for KEYEVENTF_UNICODE; the
                        // code unit travels in wScan.
                        wVk: VIRTUAL_KEY(0),
                        wScan: step.unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
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

#[cfg(test)]
mod sendinput_tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        unicode_key_plan(text)
            .iter()
            .filter(|s| !s.key_up)
            .map(|s| s.unit)
            .collect()
    }

    #[test]
    fn every_unit_gets_a_down_then_an_up() {
        let plan = unicode_key_plan("hi");
        assert_eq!(plan.len(), 4, "two units, two edges each");
        assert_eq!(
            plan[0],
            UnicodeKeyStep {
                unit: b'h' as u16,
                key_up: false
            }
        );
        assert_eq!(
            plan[1],
            UnicodeKeyStep {
                unit: b'h' as u16,
                key_up: true
            }
        );
        assert_eq!(
            plan[2],
            UnicodeKeyStep {
                unit: b'i' as u16,
                key_up: false
            }
        );
        assert_eq!(
            plan[3],
            UnicodeKeyStep {
                unit: b'i' as u16,
                key_up: true
            }
        );
    }

    #[test]
    fn astral_characters_are_sent_as_both_surrogate_halves() {
        // The bug this pins: iterating chars and casting to u16 truncates
        // every non-BMP character. An emoji must become TWO code units.
        let plan = unicode_key_plan("\u{1F600}"); // grinning face
        assert_eq!(plan.len(), 4, "one char, two surrogates, two edges each");
        let sent = units("\u{1F600}");
        assert_eq!(
            sent,
            vec![0xD83D, 0xDE00],
            "high surrogate then low, in order"
        );
    }

    #[test]
    fn surrogate_halves_stay_adjacent_and_ordered() {
        // Both halves must be adjacent in ONE batch: splitting them lets a
        // real keystroke interleave and can strand a lone surrogate, which
        // renders as a replacement glyph.
        let sent = units("a\u{1F600}b");
        assert_eq!(sent, vec![b'a' as u16, 0xD83D, 0xDE00, b'b' as u16]);
        let hi = sent.iter().position(|&u| u == 0xD83D).unwrap();
        assert_eq!(
            sent[hi + 1],
            0xDE00,
            "the low surrogate must immediately follow"
        );
    }

    #[test]
    fn non_ascii_bmp_text_survives_intact() {
        // Layout independence is the whole reason for KEYEVENTF_UNICODE:
        // these must pass through as their own code units, not as keys
        // looked up in whatever layout happens to be active.
        assert_eq!(units("é"), vec![0x00E9]);
        assert_eq!(units("日本"), vec![0x65E5, 0x672C]);
    }

    #[test]
    fn round_trips_back_to_the_original_string() {
        // The strongest property: whatever we plan to type must decode back
        // to exactly what the recognizer produced.
        for s in [
            "",
            "hello",
            "café",
            "日本語",
            "🎉 done",
            "a\u{1F600}b\u{4E2D}",
        ] {
            let sent = units(s);
            assert_eq!(
                String::from_utf16(&sent).unwrap(),
                s,
                "round trip failed for {s:?}"
            );
        }
    }

    #[test]
    fn empty_text_plans_nothing() {
        // The insert path short-circuits on an empty plan rather than
        // calling SendInput with a zero-length array.
        assert!(unicode_key_plan("").is_empty());
    }
}
