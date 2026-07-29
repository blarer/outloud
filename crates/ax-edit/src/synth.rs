//! Synthesized keyboard input, for destinations the accessibility API
//! cannot write.
//!
//! Terminals are the reason this exists. A terminal's "text field" is a
//! character grid owned by whatever program is running inside it, so it
//! exposes no writable `AXValue`: the AX tier reads and writes nothing, and
//! dictation into a shell prompt had no working path at all.
//!
//! The obvious fallback, clipboard-plus-paste, was already implemented and
//! is broken in exactly this case for a non-obvious reason: it synthesizes
//! Cmd+V by shelling out to `osascript`, and System Events keystroke
//! synthesis is itself a TCC-gated operation attributed to *osascript*, not
//! to us. So a correctly granted Aqua still gets:
//!
//! ```text
//! System Events got an error: osascript is not allowed to send keystrokes. (1002)
//! ```
//!
//! Granting osascript that permission would be asking the user to hand
//! blanket keystroke synthesis to a general-purpose scripting interpreter,
//! which is a materially worse security posture than the one grant Aqua
//! needs for itself. We already hold that grant, so we post the events
//! ourselves.
//!
//! [`type_text`] uses `CGEventKeyboardSetUnicodeString`, which attaches a
//! literal UTF-16 string to a key event instead of naming a virtual keycode.
//! That sidesteps keyboard layouts entirely (a synthetic `kVK_ANSI_A` types
//! whatever the active layout maps it to; a unicode payload types what it
//! says) and handles characters with no key at all. It is also why this is
//! preferred over pasting into a terminal even where paste works: it leaves
//! the user's clipboard untouched.

#![cfg(target_os = "macos")]

use std::ffi::c_void;

use crate::AxError;

type CGEventRef = *const c_void;
type CGEventSourceRef = *const c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(state_id: u32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        keydown: bool,
    ) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: u32, string: *const u16);
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap_location: u32, event: CGEventRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// `kCGSessionEventTap`: post into the login session, *below* the
/// `kCGHIDEventTap` insertion point.
///
/// This is not the obvious choice and the difference is load-bearing. Aqua
/// runs a listen-only `CGEventTap` at the HID level for its push-to-talk
/// hotkey (see `crates/hotkey`). Posting to the HID tap from the same
/// process routes every synthetic keystroke back through our own tap
/// callback on our own run loop, and the resulting reentrancy corrupted the
/// output: dictating "Hello Terminal." into a shell produced a row of block
/// glyphs, and injection slowed from 55ms to 360ms. The same code posting
/// from a *separate* process, with the daemon running and tapping, was
/// perfect, which is what identified the self-tap as the cause.
///
/// Session level is also more correct on the merits: these are synthetic
/// events belonging to this login session, not HID-derived input, and a
/// hotkey tap has no business seeing keystrokes the app itself generated.
const SESSION_TAP: u32 = 1;

/// `kCGEventSourceStatePrivate`: a private source does not inherit the
/// user's currently-held modifiers. Without this, a hotkey the user is
/// still physically holding (right-option, say) is combined with our
/// synthetic keystrokes and turns plain text into option-chords.
const PRIVATE_SOURCE: u32 = -1i32 as u32;
/// `kVK_ANSI_V`, needed only for the Cmd+V path.
const VK_V: u16 = 9;
/// `kCGEventFlagMaskCommand`.
const FLAG_COMMAND: u64 = 0x0010_0000;

/// Owned `CGEventRef` that releases on drop, so an early return cannot leak
/// an event.
struct Event(CGEventRef);

impl Drop for Event {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Owned event source.
struct Source(CGEventSourceRef);

impl Drop for Source {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

impl Source {
    fn new() -> Source {
        // A null source is legal and means "no source state"; it is a
        // degraded but working fallback rather than an error.
        Source(unsafe { CGEventSourceCreate(PRIVATE_SOURCE) })
    }
}

/// Pause between key events.
///
/// Two failure modes share one cause, and this is the fix for both.
///
/// `CGEventKeyboardSetUnicodeString` accepts multi-character payloads and
/// desktop text fields honour them, but a terminal does not: measured
/// against `cat > file` in Terminal.app, a 20-unit payload delivered
/// "hello from cgevent" as "bat". A terminal's input path is a tty line
/// discipline reading from a pty, not a text field taking a string, so it
/// samples the key event rather than reading the whole attached buffer.
/// [`type_text`] therefore posts one *character* per event, splitting on
/// `char` rather than on a fixed count of UTF-16 units so a surrogate pair
/// is never cut in half.
///
/// Posting those events back to back loses characters for the same reason:
/// events arriving faster than the tty consumes them are dropped, and
/// `CGEventPost` has no backpressure to report it.
///
/// The pause is expressed in *nanoseconds and slept with a spin*, not with
/// `thread::sleep`, because the OS rounds any sleep up to roughly a
/// scheduler tick: asking for 1.2ms measured out at ~15ms per character,
/// which turned a 36-character utterance into 5.6 seconds of visible
/// typing. Spinning costs one core for the duration of an utterance
/// (tens of ms) and is the difference between "instant" and "watching a
/// ghost type".
const KEY_INTERVAL: std::time::Duration = std::time::Duration::from_micros(700);

/// Sleep for `d` without paying the scheduler's tick granularity.
///
/// `thread::sleep` is the wrong primitive at this scale: it guarantees *at
/// least* the requested duration and in practice rounds up to the next
/// timer tick, which is an order of magnitude more than we want here.
fn spin_for(d: std::time::Duration) {
    let deadline = std::time::Instant::now() + d;
    while std::time::Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

/// Type `text` into whatever has keyboard focus, as unicode key events.
///
/// Requires the same Accessibility trust the AX tier needs; without it
/// `CGEventPost` silently does nothing, which is why callers must check
/// [`crate::is_trusted`] and report the permission rather than reporting a
/// mysterious no-op.
///
/// Insert-only by nature: this appends at the caret and cannot address text
/// that is already there. A destination that supports selection will replace
/// its selection, because that is what typing does.
pub fn type_text(text: &str) -> Result<(), AxError> {
    if text.is_empty() {
        return Ok(());
    }
    if !crate::is_trusted(false) {
        return Err(AxError::NotTrusted);
    }

    let source = Source::new();
    let mut buf = [0u16; 2];

    for ch in text.chars() {
        let units: &[u16] = ch.encode_utf16(&mut buf);
        // Keycode 0 with a unicode payload: the payload wins, and the
        // keycode is ignored by every consumer that reads the string.
        for &down in &[true, false] {
            let ev = Event(unsafe { CGEventCreateKeyboardEvent(source.0, 0, down) });
            if ev.0.is_null() {
                return Err(AxError::Unsupported);
            }
            unsafe {
                // Clear inherited modifiers explicitly as well as using a
                // private source: belt and braces, because a stray Command
                // flag here would turn dictated text into menu shortcuts.
                CGEventSetFlags(ev.0, 0);
                CGEventKeyboardSetUnicodeString(ev.0, units.len() as u32, units.as_ptr());
                CGEventPost(SESSION_TAP, ev.0);
            }
        }
        spin_for(KEY_INTERVAL);
    }
    Ok(())
}

/// Synthesize Cmd+V.
///
/// Kept for destinations where pasting is genuinely better than typing:
/// pasting is atomic, so a very long transcript arrives in one operation
/// rather than as a stream of events an application might reflow between.
/// Terminals prefer [`type_text`], which does not touch the clipboard.
pub fn press_cmd_v() -> Result<(), AxError> {
    if !crate::is_trusted(false) {
        return Err(AxError::NotTrusted);
    }
    let source = Source::new();
    for &down in &[true, false] {
        let ev = Event(unsafe { CGEventCreateKeyboardEvent(source.0, VK_V, down) });
        if ev.0.is_null() {
            return Err(AxError::Unsupported);
        }
        unsafe {
            CGEventSetFlags(ev.0, FLAG_COMMAND);
            CGEventPost(SESSION_TAP, ev.0);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty input must not post events or require permission: the daemon
    /// calls this on the empty-transcript path.
    #[test]
    fn empty_text_is_a_noop() {
        assert!(type_text("").is_ok());
    }

    /// Splitting on `char` must preserve the string exactly, including
    /// characters outside the BMP (two UTF-16 units, which must ride the
    /// same event) and combining marks.
    #[test]
    fn per_char_encoding_preserves_every_code_unit() {
        for s in [
            "hello world",
            "The rain in Spain falls mainly on the plain.",
            "émoji: 🎤 and 👍🏽 plus CJK 日本語テキスト",
            &"x".repeat(97),
        ] {
            let mut buf = [0u16; 2];
            let rejoined: Vec<u16> = s
                .chars()
                .flat_map(|c| c.encode_utf16(&mut buf).to_vec())
                .collect();
            assert_eq!(rejoined, s.encode_utf16().collect::<Vec<_>>());
            assert_eq!(String::from_utf16(&rejoined).unwrap(), s);
        }
    }

    /// Astral-plane characters must occupy one event, not two: half a
    /// surrogate pair is not a character and would corrupt the output.
    #[test]
    fn astral_characters_ride_a_single_event() {
        let mut buf = [0u16; 2];
        assert_eq!('🎤'.encode_utf16(&mut buf).len(), 2);
        assert_eq!('a'.encode_utf16(&mut buf).len(), 1);
    }

    /// Types into whatever is focused, so it is only meaningful by hand.
    /// Run with: cargo test -p ax-edit -- --ignored synth
    #[test]
    #[ignore = "posts real keystrokes into the focused application"]
    fn synth_types_into_the_focused_app() {
        type_text("outloud synth test\n").expect("typing must succeed when trusted");
    }
}
