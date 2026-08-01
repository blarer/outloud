//! Tier 3: synthetic keystrokes. Stubs.
//!
//! Typing the text key by key works in anything that takes keyboard focus,
//! which is why every dictation tool ships it. It is also the worst tier:
//! insert-only, layout-dependent (a synthetic `KeyA` produces whatever the
//! active layout maps it to), and slow enough per event that long insertions
//! visibly stream. Characters with no key on the current layout need a
//! per-platform unicode path, noted per target below.

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

/// How synthetic keystrokes should be paced for a given destination.
///
/// The distinction exists because the two kinds of destination consume key
/// events through entirely different machinery:
///
/// - A GUI text field receives the event's attached unicode *string* and
///   inserts all of it, so a multi-character payload arrives intact and a
///   whole sentence costs a handful of events (~1ms instead of ~40ms).
/// - A terminal's input path is a tty line discipline reading from a pty.
///   It samples the key event rather than reading the whole attached
///   buffer: measured against `cat > file` in Terminal.app, a 20-unit
///   payload delivered "hello from cgevent" as "bat". Terminals therefore
///   need one character per event, paced so the tty keeps up.
///
/// Getting this wrong in the fast direction corrupts text (the "bat" case);
/// getting it wrong in the slow direction merely wastes 40ms. The policy
/// below errs slow only for destinations that look terminal-like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingStrategy {
    /// Multi-character unicode payloads, no inter-event pacing.
    Batched,
    /// One character per event with pacing, for tty-backed input.
    PerCharPaced,
}

/// Applications whose focused "text field" is a tty behind a pty, where a
/// batched unicode payload is dropped or mangled (see [`TypingStrategy`]).
///
/// Matched against the accessibility title of the frontmost application.
/// The list errs toward inclusion: an unnecessary entry only costs speed in
/// that one app, while a missing terminal corrupts what the user dictated.
const TTY_BACKED_APPS: &[&str] = &[
    "terminal", // Terminal.app
    "iterm",    // iTerm2 reports "iTerm2" or "iTerm"
    "wezterm",
    "kitty",
    "alacritty",
    "ghostty",
    "warp",
    "hyper",
    "tabby",
    "rio",
    "zellij",
];

/// Decide how to type into the destination, as a pure function so the rule
/// is unit-testable without a display (the same discipline as
/// [`crate::detect::Env`]).
///
/// `field_reads_but_refuses_writes` is the accessibility signature of a
/// terminal scrollback: a readable `AXTextArea` that refuses both AXValue
/// and AXSelectedText writes (the Terminal.app case measured in M0). Any
/// destination showing it is treated as tty-backed even when its name is
/// not on the list, because that signature is how an unknown terminal
/// emulator presents.
///
/// Deliberately keyed on the DESTINATION application, never on whether this
/// process has a tty: a daemon launched from a shell always has one while
/// the user dictates into a browser, and that exact confusion was a real
/// bug in tier selection once already (see `Env::destination_is_terminal`).
pub fn typing_strategy_for(
    destination_app: Option<&str>,
    field_reads_but_refuses_writes: bool,
) -> TypingStrategy {
    if field_reads_but_refuses_writes {
        return TypingStrategy::PerCharPaced;
    }
    let Some(app) = destination_app else {
        // Unknown destination: the slow path is the one that cannot corrupt.
        return TypingStrategy::PerCharPaced;
    };
    let app = app.to_ascii_lowercase();
    if TTY_BACKED_APPS.iter().any(|t| app.contains(t)) {
        TypingStrategy::PerCharPaced
    } else {
        TypingStrategy::Batched
    }
}

/// Destinations whose accessibility `AXValue` write is accepted but ignored.
///
/// Electron apps built on a React-controlled contenteditable are the case
/// this exists for. `AXUIElementSetAttributeValue` on `AXValue` returns
/// success and the text visibly lands, but the write goes around the app's
/// own editor state: React's model still holds the previous value, so the
/// next keystroke reconciles against stale state. In Discord the observed
/// result is text merged with whatever was there before, the caret parked at
/// offset zero, and Enter inserting a newline instead of sending, because
/// the component never learned a message exists.
///
/// The accessibility API gives no way to ask "will this write reach your
/// model", so the only honest signal is the destination's identity. These
/// apps skip the AXValue tier entirely and go to synthesized typing, which
/// enters through the same path a human's keyboard does and therefore
/// updates the editor exactly like typing.
///
/// Matched as a lowercase substring, so "Discord Canary" and "Discord PTB"
/// are covered by "discord".
const AX_VALUE_IGNORED_APPS: &[&str] = &[
    "discord", "slack", "notion", "obsidian", "linear", "figma", "spotify", "signal", "element",
    "teams",
];

/// Destinations that also discard synthetic keystrokes shortly after they
/// land, leaving clipboard paste as the only transport that reaches them.
///
/// Measured on Discord (docs/compat-matrix.md). Polling the focused field
/// once a second during one dictation:
///
/// ```text
/// t+1s   " The dog is brown and has a lot of fun running through the yard..."
/// t+2s   "\u{feff}\n"
/// ```
///
/// The text arrives complete and correct, then the field returns to the
/// app's empty state about a second later. Nothing in the injection path
/// sends Return, so this is the app discarding the content, not an
/// accidental submit: its editor reconciles against a model that never
/// recorded the synthetic events and rewrites the DOM back to it.
///
/// This is a STRICT SUBSET of [`AX_VALUE_IGNORED_APPS`] and deliberately
/// not merged with it. The two lists state different facts, and most apps
/// that ignore AXValue writes accept typing perfectly well: Slack, Notion
/// and the rest are on that list and are not known to discard keystrokes.
/// Promoting an app here costs it the undo-preserving path and clobbers the
/// user's clipboard for a moment, so it should be earned by measurement.
const TYPING_DISCARDED_APPS: &[&str] = &["discord"];

/// What a destination will actually accept, as one answer instead of two
/// booleans every caller has to remember to combine.
///
/// Five separate bypasses of these lists shipped, each a path that consulted
/// one list, the wrong list, or neither: streaming twice, the
/// `deliver_without_ax` fallbacks, the edit path, and the AX-ignored branch.
/// Every one had the facts available and simply did not ask. Two exported
/// predicates make "did not ask" the easy default, so the shape of the API
/// was the bug.
///
/// Call [`accepts`] once per destination and match on the answer. A new
/// transport cannot silently inherit the wrong default, because there is no
/// default: the match must name every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceptance {
    /// Ordinary destination: accessibility writes stick and typing works.
    /// Both the AX tier and streaming are available.
    AxAndTyping,
    /// The editor accepts an `AXValue` write and ignores it, but takes
    /// keystrokes normally (Slack, Notion, and the rest of
    /// [`AX_VALUE_IGNORED_APPS`]). Typing is the transport; streaming must
    /// decline, because streaming's revisions ARE accessibility writes.
    TypingOnly,
    /// The app discards synthetic keystrokes shortly after they land AND
    /// ignores accessibility writes (see [`TYPING_DISCARDED_APPS`]).
    /// Clipboard paste is the only transport that reaches it.
    ClipboardOnly,
}

/// The executable name of the foreground window's process, lowercased and
/// without the `.exe` suffix, for [`accepts`].
///
/// macOS reads the app's accessibility title; Windows has no equivalent, so
/// this uses the process name, which is what `AX_VALUE_IGNORED_APPS` entries
/// like "discord" and "slack" already match against.
///
/// `None` when the window or process cannot be identified, which `accepts`
/// treats as an ordinary destination rather than assuming the worst.
#[cfg(all(target_os = "windows", feature = "display"))]
pub fn foreground_process_name() -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        // LIMITED_INFORMATION rather than QUERY_INFORMATION: it is the
        // narrowest right that answers "what is this process called", and it
        // works against elevated processes where the broader right does not.
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(
            path.rsplit(['\\', '/'])
                .next()?
                .trim_end_matches(".exe")
                .trim_end_matches(".EXE")
                .to_ascii_lowercase(),
        )
    }
}

/// What transports `app` will actually accept.
///
/// `None` means the destination is unknown, which is treated as an ordinary
/// one: assuming the worst would push every unrecognised app onto the
/// clipboard and clobber the user's pasteboard on ordinary dictation.
pub fn accepts(app: Option<&str>) -> Acceptance {
    let Some(app) = app else {
        return Acceptance::AxAndTyping;
    };
    // Narrowest fact first: the discard list is a strict subset of the
    // AX-ignored list, so checking it second would never fire.
    if discards_synthetic_typing(app) {
        Acceptance::ClipboardOnly
    } else if ignores_ax_value_writes(app) {
        Acceptance::TypingOnly
    } else {
        Acceptance::AxAndTyping
    }
}

/// Whether `app` throws away synthetic keystrokes after accepting them (see
/// [`TYPING_DISCARDED_APPS`]).
pub fn discards_synthetic_typing(app: &str) -> bool {
    let app = app.to_ascii_lowercase();
    TYPING_DISCARDED_APPS.iter().any(|a| app.contains(a))
}

/// Whether `app`'s editor ignores an `AXValue` write (see
/// [`AX_VALUE_IGNORED_APPS`]).
///
/// Public because the injection layer decides the tier while this crate owns
/// the list: one list, one consumer per decision, no drift.
pub fn ignores_ax_value_writes(app: &str) -> bool {
    let app = app.to_ascii_lowercase();
    AX_VALUE_IGNORED_APPS.iter().any(|a| app.contains(a))
}

/// Whether `app` names a terminal emulator (a tty-backed destination).
///
/// Public because the injection layer needs the same fact for a different
/// decision: a terminal destination is where the shell bridge, not typing,
/// is the right transport for an edit command. One list, two consumers,
/// zero drift.
pub fn destination_is_tty_backed(app: &str) -> bool {
    let app = app.to_ascii_lowercase();
    TTY_BACKED_APPS.iter().any(|t| app.contains(t))
}

/// Split `text` into chunks of at most `max_units` UTF-16 code units,
/// never splitting a `char` (a surrogate pair must ride one event: half a
/// pair is not a character and renders as a replacement glyph).
///
/// Pure so the chunking rule is asserted on in tests rather than buried in
/// the FFI call. `max_units` exists because very long payloads on a single
/// CGEvent have historically been truncated by some consumers; 20 units per
/// event is the conservative, widely-used bound and still turns a sentence
/// into a handful of events instead of one per character.
pub fn unicode_event_chunks(text: &str, max_units: usize) -> Vec<Vec<u16>> {
    let max_units = max_units.max(2); // a lone astral char needs 2 units
    let mut chunks: Vec<Vec<u16>> = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    let mut buf = [0u16; 2];
    for ch in text.chars() {
        let units = ch.encode_utf16(&mut buf);
        if current.len() + units.len() > max_units {
            chunks.push(std::mem::take(&mut current));
        }
        current.extend_from_slice(units);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// macOS CGEvent keyboard synthesis, batched.
///
/// Uses `CGEventCreateKeyboardEvent` plus `CGEventKeyboardSetUnicodeString`,
/// which sidesteps layouts entirely by attaching a literal UTF-16 string to
/// each event pair, and needs the same Accessibility trust the AX tier
/// needs. Unlike the per-character path in `ax_edit::synth` (which exists
/// for tty-backed destinations, see [`TypingStrategy`]), this target sends
/// multi-character payloads with no pacing, so a whole sentence costs a few
/// events rather than one pair per character: ~1ms instead of ~40ms.
pub struct CgEventTarget;

/// Same event-tap constants as `ax_edit::synth`, and for the same reasons:
/// posting at the session tap keeps our own hotkey CGEventTap from seeing
/// (and reentrantly mangling) our synthetic events, and a private source
/// prevents a physically-held hotkey modifier from combining with the
/// payload into chords.
#[cfg(all(target_os = "macos", feature = "display"))]
mod cgevent {
    use std::ffi::c_void;

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

    /// `kCGSessionEventTap`: below the HID insertion point, so our own
    /// listen-only hotkey tap never sees these events (see ax-edit::synth
    /// for the reentrancy incident this prevents).
    const SESSION_TAP: u32 = 1;
    /// `kCGEventSourceStatePrivate`: do not inherit held modifiers.
    const PRIVATE_SOURCE: u32 = -1i32 as u32;

    struct Event(CGEventRef);
    impl Drop for Event {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    /// Post every chunk as a down+up event pair carrying the chunk as its
    /// unicode payload. No pacing: GUI event queues are ordered and
    /// buffered, and pacing is exactly what made the per-character path
    /// cost 40ms per sentence.
    pub(super) fn post_chunks(chunks: &[Vec<u16>]) -> Result<(), super::TargetError> {
        let source = unsafe { CGEventSourceCreate(PRIVATE_SOURCE) };
        // A null source is legal ("no source state"): degraded, not fatal.
        let release_source = scopeguard(source);
        for chunk in chunks {
            for &down in &[true, false] {
                let ev = Event(unsafe { CGEventCreateKeyboardEvent(source, 0, down) });
                if ev.0.is_null() {
                    return Err(super::TargetError::Transport(
                        "CGEventCreateKeyboardEvent returned null".into(),
                    ));
                }
                unsafe {
                    // Belt and braces with the private source: a stray
                    // Command flag turns dictated text into menu shortcuts.
                    CGEventSetFlags(ev.0, 0);
                    CGEventKeyboardSetUnicodeString(ev.0, chunk.len() as u32, chunk.as_ptr());
                    CGEventPost(SESSION_TAP, ev.0);
                }
            }
        }
        drop(release_source);
        Ok(())
    }

    /// Minimal drop guard for the event source (a full scopeguard dep is
    /// not worth one release call).
    struct SourceGuard(CGEventSourceRef);
    impl Drop for SourceGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }
    fn scopeguard(source: CGEventSourceRef) -> SourceGuard {
        SourceGuard(source)
    }
}

/// UTF-16 units per event. See [`unicode_event_chunks`] for why 20.
///
/// Gated with its only caller: the CGEvent path below is macOS-and-display
/// only, so on every other target this is a dead constant and clippy's
/// `-D warnings` turns that into a build failure.
#[cfg(all(target_os = "macos", feature = "display"))]
const CGEVENT_CHUNK_UNITS: usize = 20;

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

    #[cfg(all(target_os = "macos", feature = "display"))]
    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        if text.is_empty() {
            return Ok(());
        }
        // Without trust CGEventPost silently does nothing, which would look
        // like a successful write that delivered no text: check and refuse.
        if !ax_edit::is_trusted(false) {
            return Err(TargetError::Unsupported(
                "CGEvent synthesis needs Accessibility trust",
            ));
        }
        cgevent::post_chunks(&unicode_event_chunks(text, CGEVENT_CHUNK_UNITS))
    }

    #[cfg(not(all(target_os = "macos", feature = "display")))]
    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "CGEvent synthesis exists only on macOS display builds",
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
mod strategy_tests {
    use super::*;

    /// The iMessage regression class: a native GUI app whose AX write was
    /// refused must get BATCHED typing, not the per-character tty pacing
    /// that made a sentence take 40ms and visibly stream.
    #[test]
    fn gui_apps_get_batched_typing() {
        for app in [
            "Messages",
            "Mail",
            "Notes",
            "Slack",
            "Safari",
            "Google Chrome",
        ] {
            assert_eq!(
                typing_strategy_for(Some(app), false),
                TypingStrategy::Batched,
                "{app} is a GUI app and must not be typed character by character"
            );
        }
    }

    /// The corruption direction: a tty samples key events instead of
    /// reading the attached string (a 20-unit payload rendered "hello from
    /// cgevent" as "bat" in Terminal.app), so terminals must stay paced.
    #[test]
    fn terminals_get_paced_typing() {
        for app in [
            "Terminal",
            "iTerm2",
            "WezTerm",
            "kitty",
            "Alacritty",
            "Ghostty",
            "Warp",
        ] {
            assert_eq!(
                typing_strategy_for(Some(app), false),
                TypingStrategy::PerCharPaced,
                "{app} is tty-backed and a batched payload would be mangled"
            );
        }
    }

    /// A readable-but-unwritable field is how an UNKNOWN terminal emulator
    /// presents (the Terminal.app scrollback signature), so that signal
    /// forces pacing even for an app whose name says nothing.
    #[test]
    fn read_only_field_forces_pacing_regardless_of_name() {
        assert_eq!(
            typing_strategy_for(Some("SomeNewTerm"), true),
            TypingStrategy::PerCharPaced
        );
        assert_eq!(
            typing_strategy_for(Some("Messages"), true),
            TypingStrategy::PerCharPaced,
            "the field signature outranks the app name: wrong-fast corrupts"
        );
    }

    /// No app name at all: err toward the path that cannot corrupt.
    #[test]
    fn unknown_destination_stays_paced() {
        assert_eq!(
            typing_strategy_for(None, false),
            TypingStrategy::PerCharPaced
        );
    }

    /// The prior real bug in this area was keying on OUR process's tty
    /// rather than the destination. The signature of this function makes
    /// that impossible to reintroduce silently: it takes only destination
    /// facts, and matching is case-insensitive so "terminal" and
    /// "Terminal" agree.
    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(
            typing_strategy_for(Some("TERMINAL"), false),
            TypingStrategy::PerCharPaced
        );
    }

    fn rejoin(chunks: &[Vec<u16>]) -> String {
        let units: Vec<u16> = chunks.iter().flatten().copied().collect();
        String::from_utf16(&units).unwrap()
    }

    /// The strongest chunking property: what is posted must decode back to
    /// exactly the transcript, for any chunk bound.
    #[test]
    fn chunks_round_trip_to_the_original() {
        for s in [
            "",
            "hello",
            "The quick brown fox jumps over the lazy dog.",
            "émoji 🎤👍🏽 日本語",
        ] {
            for max in [2, 3, 20, 1000] {
                assert_eq!(
                    rejoin(&unicode_event_chunks(s, max)),
                    s,
                    "max={max} s={s:?}"
                );
            }
        }
    }

    /// A surrogate pair must never straddle a chunk boundary: half a pair
    /// on its own event renders as a replacement glyph.
    #[test]
    fn surrogate_pairs_never_split_across_chunks() {
        // max=3 forces awkward boundaries around every 2-unit char.
        for chunk in unicode_event_chunks("a🎤b🎤c🎤", 3) {
            assert!(chunk.len() <= 3);
            // A chunk must not END with an unmatched high surrogate.
            if let Some(&last) = chunk.last() {
                assert!(
                    !(0xD800..0xDC00).contains(&last),
                    "chunk ends with a lone high surrogate: {chunk:?}"
                );
            }
        }
    }

    #[test]
    fn empty_text_produces_no_chunks() {
        assert!(unicode_event_chunks("", 20).is_empty());
    }

    /// Chunk bound is respected: a 44-char sentence at 20 units per event
    /// is 3 events, which is the entire speedup over 44 paced events.
    #[test]
    fn chunk_bound_is_respected() {
        let chunks = unicode_event_chunks("The quick brown fox jumps over the lazy dog.", 20);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.len() <= 20));
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

    /// Discord is the destination this list was built for.
    ///
    /// Reported symptom: text pasted but Enter broke the line instead of
    /// sending, and the message could not be sent at all. Cause: Discord
    /// accepts an AXValue write and its React editor ignores it, so the
    /// component's model never learns a message exists.
    #[test]
    fn discord_is_known_to_ignore_ax_value_writes() {
        assert!(ignores_ax_value_writes("Discord"));
        // Beta channels ship under their own names.
        assert!(ignores_ax_value_writes("Discord Canary"));
        assert!(ignores_ax_value_writes("Discord PTB"));
    }

    /// Native apps must NOT be diverted: the AX path preserves their undo,
    /// which typing does not, so diverting them would be a real regression.
    #[test]
    fn native_apps_keep_the_accessibility_path() {
        for app in ["TextEdit", "Notes", "Mail", "Safari", "Pages"] {
            assert!(
                !ignores_ax_value_writes(app),
                "{app} handles AXValue correctly and must keep the AX tier"
            );
        }
    }

    /// Discord fails BOTH earlier tiers, so it must be on both lists, and the
    /// narrower one has to be checked first.
    ///
    /// Measured: an AXValue write is accepted and ignored, and synthetic
    /// keystrokes land and are then discarded about a second later. Only a
    /// real paste survives.
    #[test]
    fn discord_is_on_both_lists() {
        assert!(ignores_ax_value_writes("Discord"));
        assert!(discards_synthetic_typing("Discord"));
        // Same substring matching as the other list, so the beta channels
        // are covered without being enumerated.
        assert!(discards_synthetic_typing("Discord Canary"));
        assert!(discards_synthetic_typing("Discord PTB"));
    }

    /// The typing-discard list is a STRICT subset, and keeping it that way
    /// is the point.
    ///
    /// Everything on it loses the undo-preserving path and briefly clobbers
    /// the user's clipboard, so an app belongs here only once measured. The
    /// other Electron apps ignore AXValue writes but are not known to throw
    /// away keystrokes, and assuming they do would degrade all of them on
    /// one app's evidence.
    #[test]
    fn typing_discard_is_measured_not_assumed() {
        for app in ["Slack", "Notion", "Obsidian", "Linear", "Figma", "Teams"] {
            assert!(
                ignores_ax_value_writes(app),
                "{app} should still skip the AXValue tier"
            );
            assert!(
                !discards_synthetic_typing(app),
                "{app} has not been measured as discarding typing; do not \
                 promote it without the measurement"
            );
        }
        // A native app is on neither list.
        assert!(!ignores_ax_value_writes("TextEdit"));
        assert!(!discards_synthetic_typing("TextEdit"));
    }

    /// Ignoring an AXValue write must NOT drag an app onto the slow typing
    /// path. They are different facts about different mechanisms.
    ///
    /// Regression: the `ignores_ax_value_writes` branch in inject.rs passed
    /// `true` for `field_reads_but_refuses_writes`, which short-circuits this
    /// function to PerCharPaced before it ever looks at the app. At
    /// 700us/char (ax_edit::synth::KEY_INTERVAL) that is a ~73ms floor on a
    /// 104-character sentence, paid by nine apps that are not terminals.
    ///
    /// `field_reads_but_refuses_writes` means "the field reads back but
    /// refuses every write", which is the accessibility signature of a
    /// terminal scrollback. That is the only thing that should force pacing
    /// regardless of app name.
    #[test]
    fn ax_ignoring_apps_still_get_the_fast_typing_path() {
        // Every app on the AX-ignored list is a GUI app that accepts
        // keystrokes normally. None is tty-backed.
        for app in AX_VALUE_IGNORED_APPS {
            assert_eq!(
                typing_strategy_for(Some(app), false),
                TypingStrategy::Batched,
                "{app} ignores AXValue writes but is not a terminal, so it \
                 must not be forced onto the paced path"
            );
        }

        // The flag still forces pacing when it is genuinely set, which is
        // what protects an unknown terminal emulator.
        assert_eq!(
            typing_strategy_for(Some("Slack"), true),
            TypingStrategy::PerCharPaced,
            "a field that refuses every write is the terminal signature"
        );

        // And real terminals stay paced on name alone.
        assert_eq!(
            typing_strategy_for(Some("Terminal"), false),
            TypingStrategy::PerCharPaced
        );
        // Unknown destination keeps the safe slow path.
        assert_eq!(
            typing_strategy_for(None, false),
            TypingStrategy::PerCharPaced
        );
    }

    /// One answer per destination, so a new transport cannot inherit a wrong
    /// default by forgetting to consult a list.
    ///
    /// Five bypasses shipped before this existed, each a path that asked one
    /// list, the wrong list, or neither.
    #[test]
    fn acceptance_names_the_transport_for_every_destination() {
        // Discord is on BOTH lists, and the clipboard answer must win: it is
        // the narrower fact and the only transport that reaches it.
        assert_eq!(accepts(Some("Discord")), Acceptance::ClipboardOnly);

        // AX-ignored but types fine. Streaming must decline for these (its
        // revisions are AX writes) while typing still works.
        for app in ["Slack", "Notion", "Linear", "Figma", "Signal", "Teams"] {
            assert_eq!(
                accepts(Some(app)),
                Acceptance::TypingOnly,
                "{app} ignores AX writes but accepts keystrokes"
            );
        }

        // Ordinary apps keep the fast path.
        for app in ["TextEdit", "Xcode", "Safari"] {
            assert_eq!(accepts(Some(app)), Acceptance::AxAndTyping, "{app}");
        }

        // Unknown destination is treated as ordinary ON PURPOSE: assuming the
        // worst would clobber the pasteboard on every unrecognised app.
        assert_eq!(accepts(None), Acceptance::AxAndTyping);

        // Every app on the discard list must resolve to ClipboardOnly, or a
        // future addition would silently get typed into.
        for app in TYPING_DISCARDED_APPS {
            assert_eq!(accepts(Some(app)), Acceptance::ClipboardOnly, "{app}");
        }
    }
}
