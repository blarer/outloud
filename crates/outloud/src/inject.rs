//! The write half: turn a final transcript into text on the user's screen.
//!
//! Two paths, decided by what was selected at *key-down* time (deliverables
//! 2 and 3):
//!
//! - **Dictation** (no selection): the transcript is inserted at the caret.
//! - **Edit** (selection present): the transcript is parsed as an
//!   [`EditIntent`] and applied to the selected text, and the rewritten
//!   selection is written back in place. Freeform intents have no local LLM
//!   yet, so they are *reported*, loudly and with the heard instruction,
//!   instead of silently doing nothing.
//!
//! Every failure names its next action (the diag crate's philosophy): the
//! outcome enum carries a user-facing situation -> action string, and the
//! caller maps it onto the Error overlay state.

// `AxError` is only named by the macOS splice/fallback paths below; on other
// platforms delivery goes through the text-target tier ladder and never
// mentions it, so an unconditional import is an unused-import error there.
#[cfg(target_os = "macos")]
use ax_edit::AxError;
use ax_edit::TextSnapshot;
use edit_intent::EditIntent;

use crate::freeform::{classify, FreeformDisposition};

/// What the snapshot taken at key-down tells us about the coming utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Insert the transcript at the caret.
    Dictate,
    /// Apply the transcript as an edit command to this selected text.
    Edit { selected: String },
}

/// Decide the mode from a key-down snapshot. `None` snapshot (no focused
/// text field, AX refused, etc.) still means dictation: the clipboard-paste
/// fallback can insert into fields AX cannot even read.
pub fn mode_from_snapshot(snap: Option<&TextSnapshot>) -> Mode {
    match snap {
        Some(s) if s.is_selection_edit() => Mode::Edit {
            selected: s
                .selected_text
                .clone()
                .expect("is_selection_edit implies selected_text"),
        },
        _ => Mode::Dictate,
    }
}

/// Read the focused destination at key-down and decide dictate-vs-edit,
/// using whichever accessibility API this platform has.
///
/// Exists so the pipeline has ONE call for "what is selected right now"
/// instead of a macOS-shaped call the other platforms cannot answer. On
/// macOS this is the AX snapshot the M0 spike measured at ~134us warm; on
/// Windows it is UI Automation's `TextPattern::GetSelection`. Everywhere
/// else, and on any failure, the answer is dictation, which is the safe
/// degradation: inserting text is never destructive, whereas guessing at
/// an edit would rewrite something the user did not select.
pub fn mode_at_keydown() -> Mode {
    #[cfg(target_os = "macos")]
    {
        mode_from_snapshot(ax_edit::snapshot_focused().ok().as_ref())
    }

    #[cfg(all(target_os = "windows", feature = "display"))]
    {
        use text_target::targets::ax::UiaTarget;
        // A fresh client per key-down: UIA connection setup is cheap
        // relative to an utterance, and holding COM state across the
        // pipeline's threads would need apartment marshalling for no gain.
        match UiaTarget::new().and_then(|mut t| t.selected_text()) {
            Ok(Some(selected)) => Mode::Edit { selected },
            Ok(None) => Mode::Dictate,
            Err(e) => {
                // Logged, not silent: an elevated window in focus (UIPI)
                // looks exactly like "nothing selected" otherwise, and the
                // user deserves to know why edit-by-voice went quiet.
                eprintln!("outloud: could not read the focused element ({e}); assuming dictation");
                Mode::Dictate
            }
        }
    }

    #[cfg(not(any(target_os = "macos", all(target_os = "windows", feature = "display"))))]
    Mode::Dictate
}

/// Key-down read that also hands back the snapshot itself, so the streaming
/// path can probe the same field the mode decision was made from. One AX
/// read, two consumers: re-snapshotting for the probe would race a focus
/// change between two reads that must describe the same element.
pub fn snapshot_and_mode_at_keydown() -> (Mode, Option<TextSnapshot>) {
    #[cfg(target_os = "macos")]
    {
        let snap = ax_edit::snapshot_focused().ok();
        (mode_from_snapshot(snap.as_ref()), snap)
    }
    #[cfg(not(target_os = "macos"))]
    {
        (mode_at_keydown(), None)
    }
}

/// The profile-matching identity of the app a snapshot came from.
///
/// Built from the snapshot rather than looked up separately so the app
/// that profiles resolve against is the same one whose text was read.
///
/// `window_class` stays `None` on macOS: it is an X11/Wayland concept and
/// inventing a value for it would make `match.window-class` fire on the
/// wrong platform. `process_name` carries the accessibility title, which
/// is the closest honest analogue available without spawning anything.
#[cfg(target_os = "macos")]
pub fn app_identity(snap: Option<&TextSnapshot>) -> Option<config::AppIdentity> {
    let snap = snap?;
    if snap.bundle_id.is_none() && snap.app.is_none() {
        return None;
    }
    Some(config::AppIdentity {
        bundle_id: snap.bundle_id.clone(),
        process_name: snap.app.clone(),
        window_class: None,
    })
}

/// How one utterance ended, for the overlay and the log.
#[derive(Debug)]
pub enum Outcome {
    /// Text landed. `via` names the transport/strategy for diagnostics.
    Wrote { text: String, via: String },
    /// Recognizer heard nothing: return quietly to Idle per the state table.
    EmptyTranscript,
    /// A freeform edit instruction with no local LLM to run it (deliverable
    /// 3): reported, never silently dropped.
    FreeformUnsupported { instruction: String },
    /// The edit command's search text was not in the selection.
    EditNoMatch { command: String },
    /// A deterministic edit command spoken at a terminal, staged as an
    /// INTENT on the shell bridge. Nothing was typed: the shell applies the
    /// rewrite itself when the user presses the plugin's key (^X^A), which
    /// is the only in-place, undo-preserving edit path a terminal has.
    StagedShellIntent { command: String },
    /// Delivery was suppressed by OUTLOUD_NO_INJECT. Carries the text that
    /// would have been written, so a measurement run still reports what the
    /// recognizer produced.
    Suppressed { text: String },
    /// Everything failed. `situation_action` is the one-line
    /// "situation -> next action" string for the Error overlay state.
    Failed { situation_action: String },
}

/// Deliver `transcript` according to `mode`.
///
/// Blocking (AX writes are ~13ms synchronous IPC); the supervisor calls it
/// via `spawn_blocking` so a hung target app cannot stall the event loop.
pub fn deliver(mode: &Mode, transcript: &str) -> Outcome {
    let text = transcript.trim();
    if text.is_empty() {
        return Outcome::EmptyTranscript;
    }

    // OUTLOUD_NO_INJECT=1: measure everything, type nothing.
    //
    // Not a nicety. A `--once --wav` run delivers into whatever window
    // happens to be focused, so benchmarking against a recording while the
    // user is working types the test sentence into their chat window. That
    // happened. An automated run must be able to exercise the whole
    // pipeline without touching a UI it does not own.
    if std::env::var_os("OUTLOUD_NO_INJECT").is_some_and(|v| v == "1") {
        return Outcome::Suppressed {
            text: text.to_string(),
        };
    }

    // A terminal destination inverts the transport decision for edit
    // commands: a terminal's line buffer is unreachable by AX and typing
    // the words "change x to y" into a prompt is not an edit, it is
    // corruption one Enter away from running. When a bridge is serving,
    // stage the utterance as an INTENT instead and let the shell pull it.
    #[cfg(all(target_os = "macos", feature = "display"))]
    if let Some(outcome) = stage_terminal_edit(text) {
        return outcome;
    }

    // Non-macOS platforms have no ax-edit, so they take the platform-tier
    // path below rather than the AX-specific splice logic. Keeping the two
    // separate (instead of abstracting ax-edit away) means the macOS path,
    // the only one measured end to end, is byte-for-byte what M0 proved.
    #[cfg(not(target_os = "macos"))]
    {
        deliver_via_tiers(mode, text)
    }

    #[cfg(target_os = "macos")]
    match mode {
        Mode::Dictate => insert_with_fallback(text),
        Mode::Edit { selected } => {
            // Recognizers punctuate ("Change quick to slow."), but spoken
            // edit commands are imperatives whose trailing punctuation was
            // never said: strip it so "to slow." does not write "slow.".
            let command = text.trim_end_matches(['.', '!', '?', ',']);
            let intent = edit_intent::parse(command);

            // A selection means an edit is POSSIBLE, not that one was
            // intended, and the two readings of an unparsed phrase have
            // opposite correct behaviours. Text is selected far more often
            // than people realise (a terminal keeps the last drag
            // selected, editors highlight the current word, browsers hold
            // a selection long after the click), so refusing every
            // unparsed phrase turned ordinary dictation into "the app
            // stopped transcribing". But inserting every unparsed phrase
            // meant "tighten this up" REPLACED the selected sentence with
            // the words "Tighten this up.", destroying it silently.
            //
            // `freeform::classify` is the rule that separates them, and it
            // is biased: a wrong refusal costs one retry, a wrong
            // overwrite costs a paragraph. See that module for the signals
            // and the escape hatch ("type: ...").
            if let EditIntent::Freeform { .. } = &intent {
                return match classify(text, selected) {
                    // Dictation that happened while something was
                    // selected. Insert it, exactly as before.
                    FreeformDisposition::Dictate { text } => insert_with_fallback(&text),
                    // Recognisably an instruction ABOUT the selection that
                    // nothing here can carry out. Write NOTHING and say so
                    // through the Error overlay: the user's selected text
                    // is the one thing that must survive.
                    FreeformDisposition::RewriteRequest { instruction } => {
                        Outcome::FreeformUnsupported { instruction }
                    }
                };
            }

            match edit_intent::apply(selected, &intent) {
                Some(rewritten) => replace_selection(&rewritten),
                // The command parsed as an edit but matched nothing in the
                // selection. That IS worth reporting: the user said
                // "change X to Y" and meant it, so silently inserting the
                // sentence would be the wrong guess.
                None => Outcome::EditNoMatch {
                    command: text.to_string(),
                },
            }
        }
    }
}

/// Delivery for platforms whose write path is a `text-target` tier rather
/// than `ax-edit`: Windows today (UIA, then SendInput, then clipboard),
/// and the shape any future Linux backend slots into.
///
/// The tier ladder is walked explicitly here instead of through
/// `text_target::detect` because delivery needs the *fallback* behaviour on
/// failure, and detect answers a different question ("what is best right
/// now") with no retry semantics.
/// What the tier ladder should actually write, or the outcome that ends the
/// utterance before any transport is touched.
///
/// Split out and compiled on EVERY platform so the decision is unit-tested
/// on macOS CI, the same reason `winmatch` and `detect_display_on` are pure.
/// Otherwise this logic would only ever be exercised on Windows hardware
/// nobody has run yet.
pub fn payload_for(mode: &Mode, text: &str) -> Result<String, Outcome> {
    match mode {
        Mode::Dictate => Ok(text.to_string()),
        Mode::Edit { selected } => {
            // Recognizers punctuate; spoken imperatives do not. Same
            // stripping as the macOS path, for the same reason: "to slow."
            // must not write "slow.".
            let command = text.trim_end_matches(['.', '!', '?', ',']);
            let intent = edit_intent::parse(command);
            if let EditIntent::Freeform { .. } = &intent {
                // Same split as the macOS path, through the same rule:
                // an instruction about the selection is refused (nothing
                // is written), while dictation that merely coincided with
                // a selection is written verbatim. The caller demotes the
                // MODE too, so the payload cannot reach a replace-shaped
                // transport; see `deliver_via_tiers`.
                return match classify(text, selected) {
                    FreeformDisposition::Dictate { text } => Ok(text),
                    FreeformDisposition::RewriteRequest { instruction } => {
                        Err(Outcome::FreeformUnsupported { instruction })
                    }
                };
            }
            match edit_intent::apply(selected, &intent) {
                Some(rewritten) => Ok(rewritten),
                None => Err(Outcome::EditNoMatch {
                    command: text.to_string(),
                }),
            }
        }
    }
}

/// May this mode fall back to an INSERT-ONLY transport (SendInput, typing)?
///
/// No, for edits. An insert-only tier cannot address existing text, so an
/// edit reaching one would append the rewritten text NEXT TO the original
/// instead of replacing it: "the quick fox" plus a rewrite yields "the
/// quick fox the slow fox". That is corruption, not degradation, and it is
/// silent. Edits therefore skip straight to the clipboard, which replaces a
/// selection natively through the app's own paste handling.
pub fn may_use_insert_only_tier(mode: &Mode) -> bool {
    matches!(mode, Mode::Dictate)
}

/// The pure half of the terminal edit-by-voice decision: given the focused
/// destination and the transcript, the shell-bridge command to stage, or
/// `None` when this utterance is ordinary dictation.
///
/// Stages only DETERMINISTIC edit commands. A freeform phrase spoken at a
/// terminal is someone dictating text into their shell (a commit message,
/// a grep pattern), and hijacking it into a bridge intent would make plain
/// dictation silently stop typing.
///
/// Pure and compiled wherever the keys tier exists, so the decision is
/// unit-tested without a bridge, a terminal, or an accessibility grant.
#[cfg(feature = "display")]
pub fn shell_bridge_command(destination_app: Option<&str>, transcript: &str) -> Option<String> {
    let app = destination_app?;
    if !text_target::targets::keys::destination_is_tty_backed(app) {
        return None;
    }
    // Recognizers punctuate; spoken imperatives do not. Same stripping as
    // the GUI edit path, so "to staging-web." stages "to staging-web".
    let command = transcript.trim_end_matches(['.', '!', '?', ',']);
    match edit_intent::parse(command) {
        EditIntent::Freeform { .. } => None,
        _ => Some(command.to_string()),
    }
}

/// Stage an edit command on the shell bridge when the focused destination
/// is a terminal and a bridge is serving. `None` means "not this path":
/// the caller falls through to the ordinary delivery ladder.
#[cfg(all(target_os = "macos", feature = "display"))]
fn stage_terminal_edit(text: &str) -> Option<Outcome> {
    let app = ax_edit::frontmost_app()?;
    let command = shell_bridge_command(Some(&app), text)?;
    let socket = shell_bridge::server::default_socket_path();
    // No bridge serving: fall through to typing. Refusing outright would
    // break dictating literal words at a prompt for everyone who never
    // installed the shell integration.
    if !socket.exists() {
        return None;
    }
    match shell_bridge::server::stage_intent(&socket, &command) {
        Ok(_) => Some(Outcome::StagedShellIntent { command }),
        Err(e) => {
            // A dead socket file from a crashed bridge: say so, then fall
            // through to typing rather than swallowing the utterance.
            eprintln!("outloud: bridge socket present but staging failed ({e}); typing instead");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn deliver_via_tiers(mode: &Mode, text: &str) -> Outcome {
    // Same rule the macOS path applies, for the same shipped-broken
    // reason: a selection means an edit is POSSIBLE, not INTENDED. An
    // unparsed phrase that `freeform::classify` reads as ordinary
    // dictation is demoted to `Mode::Dictate`, which both inserts it and
    // keeps it away from replace-shaped transports that would otherwise
    // overwrite a selection the user never aimed at.
    //
    // A phrase classified as an instruction about the selection is NOT
    // demoted: it stays in `Mode::Edit` so `payload_for` refuses it and
    // no transport is touched at all.
    let mode = match mode {
        Mode::Edit { selected }
            if matches!(
                edit_intent::parse(text.trim_end_matches(['.', '!', '?', ','])),
                EditIntent::Freeform { .. }
            ) && matches!(
                classify(text, selected),
                FreeformDisposition::Dictate { .. }
            ) =>
        {
            &Mode::Dictate
        }
        m => m,
    };
    let payload = match payload_for(mode, text) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };

    #[cfg(all(target_os = "windows", feature = "display"))]
    {
        use text_target::targets::ax::UiaTarget;
        use text_target::TextTarget;

        // Tier 1: UI Automation. For an edit this is a true in-place
        // rewrite of the field's value; for dictation it appends at the
        // field's end, which is where a dictated sentence belongs.
        let uia_err = match UiaTarget::new() {
            Ok(mut t) => {
                let res = match mode {
                    Mode::Dictate => t.insert(&payload),
                    Mode::Edit { .. } => t.replace(&payload),
                };
                match res {
                    Ok(()) => {
                        return Outcome::Wrote {
                            text: payload,
                            via: "windows-uia".into(),
                        }
                    }
                    Err(e) => e.to_string(),
                }
            }
            Err(e) => e.to_string(),
        };
        eprintln!("outloud: UI Automation write refused ({uia_err}); falling back");

        // Tier 3: SendInput. Insert-only, so an edit that reaches here
        // would APPEND the rewritten text next to the original rather than
        // replacing it. That is a corruption, not a degradation, so edits
        // stop at the clipboard (where the user pastes over their own
        // selection deliberately) and only dictation types.
        if may_use_insert_only_tier(mode) {
            use text_target::targets::keys::SendInputTarget;
            let mut keys = SendInputTarget;
            match keys.insert(&payload) {
                Ok(()) => {
                    return Outcome::Wrote {
                        text: payload,
                        via: "windows-sendinput".into(),
                    }
                }
                Err(e) => eprintln!("outloud: SendInput refused ({e}); falling back to clipboard"),
            }
        }

        // Tier 4: clipboard + Ctrl+V, which replaces a selection natively
        // and so is the correct last resort for edits too.
        use text_target::targets::clipboard::ClipboardTarget;
        match ClipboardTarget::new() {
            Ok(mut clip) => match clip.insert(&payload) {
                Ok(()) => {
                    // Let the target consume the paste before handing the
                    // user's own clipboard back.
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let _ = clip.restore();
                    Outcome::Wrote {
                        text: payload,
                        via: "clipboard-paste".into(),
                    }
                }
                Err(e) => Outcome::Failed {
                    situation_action: format!(
                        "every write tier refused ({e}) -> your text is on the clipboard, \
                         press Ctrl+V (an elevated window in focus blocks us: see UIPI in \
                         docs/compat-matrix.md)"
                    ),
                },
            },
            Err(e) => Outcome::Failed {
                situation_action: format!(
                    "every write tier refused and no clipboard ({e}) -> focus a normal \
                     (non-elevated) text field and try again"
                ),
            },
        }
    }

    #[cfg(not(all(target_os = "windows", feature = "display")))]
    {
        Outcome::Failed {
            situation_action: format!(
                "no write transport on this platform/build for \"{payload}\" \
                 -> use the terminal-native transports via spike-cli"
            ),
        }
    }
}

/// Insert at the caret.
///
/// Trap this function exists to defuse: with no selection,
/// `ax_edit::replace_focused` writes the whole `AXValue`, which would
/// REPLACE the user's entire document with the transcript. So dictation
/// re-reads the field at commit time and splices the transcript into the
/// existing value at the caret (AX reports the caret as a zero-length
/// selection in UTF-16 units), then writes the spliced whole value. When the
/// field cannot be read or the caret cannot be mapped, we fall back to
/// clipboard paste, which inserts natively and cannot destroy anything.
#[cfg(target_os = "macos")]
fn insert_with_fallback(text: &str) -> Outcome {
    match ax_edit::snapshot_focused() {
        Ok(snap) => {
            // A read-only field is not a text destination we can write, and
            // trying anyway is actively destructive rather than merely
            // useless. Terminal.app is the case that matters: it exposes its
            // *scrollback* as an AXTextArea with a caret, so the code below
            // happily spliced a transcript into 1300 characters of scrollback
            // and wrote the result back. Dictating into a terminal produced a
            // screenful of mangled history instead of the sentence.
            //
            // Checked before the selection branch as well as the caret one,
            // because AXSelectedText is refused on the same elements.
            // Some Electron editors ACCEPT an AXValue write and then ignore
            // it. Discord is the case this was found in: the text lands
            // visibly, but React's model still holds the old value, so the
            // caret sits at offset zero, the new text is merged with
            // whatever was there before, and Enter inserts a newline
            // instead of sending, because the component never learned a
            // message exists. The write reports success, which makes this
            // strictly worse than a refusal: the fallback never runs.
            //
            // There is no accessibility query for "will this reach your
            // model", so the destination's identity is the only honest
            // signal. Treat it as a refusal and type instead, which enters
            // through the same path a human's keyboard does.
            if snap
                .app
                .as_deref()
                .is_some_and(text_target::targets::keys::ignores_ax_value_writes)
            {
                return deliver_without_ax(
                    text,
                    &AxError::NotSettable,
                    typing_strategy(snap.app.as_deref(), true),
                    char_before_caret(&snap),
                );
            }

            if is_read_only(&snap) {
                // A readable-but-unwritable field is the accessibility
                // signature of a terminal scrollback, so the typing
                // strategy is forced to the paced per-character path here
                // regardless of the app's name (an unknown terminal
                // emulator presents exactly this way).
                return deliver_without_ax(
                    text,
                    &AxError::NotSettable,
                    typing_strategy(snap.app.as_deref(), true),
                    char_before_caret(&snap),
                );
            }
            // A non-empty selection at commit time: typing replaces it, so
            // dictation does too, through the undo-preserving path only.
            if snap.is_selection_edit() {
                return write_over_selection(&snap, text);
            }
            match spliced_at_caret(&snap, text) {
                Some(new_value) => {
                    write_focused(&new_value, typing_strategy(snap.app.as_deref(), false))
                }
                // Field readable but caret unknown/unmappable: paste inserts
                // at the caret without us knowing where it is.
                // The field was readable but its caret was not mappable, so
                // the snapshot can still say what precedes the insertion
                // even though the splice could not be computed.
                None => deliver_without_ax(
                    text,
                    &AxError::NoTextValue,
                    typing_strategy(snap.app.as_deref(), false),
                    char_before_caret(&snap),
                ),
            }
        }
        // No snapshot at all, so the snapshot cannot name the destination.
        // Ask for the frontmost application separately: knowing the app is
        // what allows the fast batched typing path, and one extra AX call
        // is cheap next to the 40ms the per-character path would cost.
        // No snapshot at all, so nothing is known about the caret. Passing
        // None means no space is added, which is the safe direction: a
        // stray leading space would be visible on every utterance, while a
        // missing one only shows when appending to existing text.
        Err(e) => deliver_without_ax(
            text,
            &e,
            typing_strategy(ax_edit::frontmost_app().as_deref(), false),
            None,
        ),
    }
}

/// The typing-strategy decision, in one place so both fallback entry points
/// agree. Delegates to the pure, unit-tested rule in `text-target`; this
/// wrapper only exists because a headless build has no `keys` module.
#[cfg(all(target_os = "macos", feature = "display"))]
fn typing_strategy(
    app: Option<&str>,
    field_reads_but_refuses_writes: bool,
) -> text_target::targets::keys::TypingStrategy {
    text_target::targets::keys::typing_strategy_for(app, field_reads_but_refuses_writes)
}

/// Headless builds have no synthetic-keys tier to choose a strategy for.
#[cfg(all(target_os = "macos", not(feature = "display")))]
fn typing_strategy(_app: Option<&str>, _field_reads_but_refuses_writes: bool) -> TypingChoice {
    NoTypingStrategy
}

/// Whether the focused element refuses every write the AX tier has.
///
/// Split out from [`insert_with_fallback`] so the rule is testable without
/// a live accessibility tree, because the case it guards is destructive
/// rather than merely unhelpful: Terminal.app exposes its scrollback as a
/// readable `AXTextArea` with a caret, and writing a spliced value back
/// replaces the visible history with mangled text.
///
/// `any(..., test)` rather than macOS-only: the unit tests below encode the
/// Terminal.app regression and must keep running on every platform's test
/// suite, not just where the live caller compiles.
#[cfg(any(target_os = "macos", test))]
fn is_read_only(snap: &TextSnapshot) -> bool {
    !snap.value_settable && !snap.selected_text_settable
}

/// The spliced whole-field value for inserting `text` at the caret, or
/// `None` when the snapshot does not pin down where the caret is.
///
/// `any(..., test)` for the same reason as [`is_read_only`]: the splice
/// tests caught a real off-by-one and must not be lost off macOS.
#[cfg(any(target_os = "macos", test))]
fn spliced_at_caret(snap: &TextSnapshot, text: &str) -> Option<String> {
    let value = snap.value.as_deref()?;
    if value.is_empty() {
        // Empty field: the whole value IS the transcript, no offset math.
        return Some(text.to_string());
    }
    let (loc, len) = snap.selection?;
    if len != 0 {
        return None; // selection handled by the caller
    }
    let at = crate::utf16_offset_to_byte(value, loc)?;
    // Join like a human typing: a space on either side where the insertion
    // would otherwise glue onto an existing word, none against whitespace
    // or the field's edges.
    let needs_space_before = value[..at]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace());
    let needs_space_after = value[at..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace());
    let mut out = String::with_capacity(value.len() + text.len() + 2);
    out.push_str(&value[..at]);
    if needs_space_before {
        out.push(' ');
    }
    out.push_str(text);
    if needs_space_after {
        out.push(' ');
    }
    out.push_str(&value[at..]);
    Some(out)
}

/// Whether a transcript needs a space in front of it, given what already
/// precedes the caret.
///
/// Dictation is not a single utterance. People stop, think, and start
/// again, and each utterance arrives as its own transcript, so the join
/// between them is ours to make. The AX splice path has always handled
/// this by reading the field; the typing and clipboard fallbacks could
/// not, because they never see the existing text, so on every destination
/// that refuses AX writes a new sentence was glued onto the last one:
/// "you'll see.Right now I'm talking".
///
/// `preceding` is whatever the caller could learn about the character
/// before the caret. `None` means "unknown", which is treated as needing
/// no space: a spurious leading space at the very start of an empty field
/// is a visible defect on every single utterance, whereas a missing one
/// only shows up when appending, and the caller supplies the character
/// whenever it can.
/// Gated like the other splice helpers: the real caller is macOS-only, but
/// `test` keeps the rule verified on every platform, because this encodes a
/// user-visible formatting decision rather than a platform detail.
#[cfg(any(target_os = "macos", test))]
fn needs_leading_space(preceding: Option<char>) -> bool {
    match preceding {
        // Whitespace already separates us, and a second space would show.
        Some(c) if c.is_whitespace() => false,
        // An opening bracket or quote hugs the word that follows it, the
        // way a human types `("hello` rather than `( "hello`.
        Some('(' | '[' | '{' | '<' | '"' | '\'' | '\u{201c}' | '\u{2018}') => false,
        Some(_) => true,
        None => false,
    }
}

/// The character immediately before the caret, when the field can be read.
///
/// Returns `None` for an empty field or an unmappable caret, which
/// [`needs_leading_space`] reads as "do not add a space".
/// macOS-only, with no `test` companion, unlike [`needs_leading_space`]
/// beside it. That asymmetry is deliberate: the spacing RULE is pure and
/// worth verifying on every platform, whereas reading the caret is
/// accessibility plumbing whose only callers are the macOS write paths. A
/// `test` gate here would compile it on Linux with nothing calling it,
/// which is exactly the dead-code failure this replaces.
#[cfg(target_os = "macos")]
fn char_before_caret(snap: &TextSnapshot) -> Option<char> {
    let value = snap.value.as_deref()?;
    let (loc, len) = snap.selection?;
    if len != 0 {
        return None;
    }
    let at = crate::utf16_offset_to_byte(value, loc)?;
    value[..at].chars().next_back()
}

/// One AX write with typed/clipboard fallback, shared by both paths.
/// `typing` is decided by the caller, which has the snapshot naming the
/// destination app; recomputing it here would race a focus change.
#[cfg(target_os = "macos")]
fn write_focused(text: &str, typing: TypingChoice) -> Outcome {
    match ax_edit::replace_focused(text) {
        Ok(strategy) => Outcome::Wrote {
            text: text.to_string(),
            via: strategy.to_string(),
        },
        // No preceding character: whatever the caller handed us was already
        // spliced or spaced upstream, so asking for a space again would
        // double it.
        Err(e) => deliver_without_ax(text, &e, typing, None),
    }
}

/// Replace the selection. The selection was read at key-down; writing
/// `AXSelectedText` at commit time replaces whatever is selected *now*,
/// which is still the same selection in the overwhelmingly common case
/// (the user held a key and spoke, they did not re-select). Verifying the
/// selection is unchanged before writing is future work needing
/// AXSelectedTextRange comparison in ax-edit.
#[cfg(target_os = "macos")]
fn replace_selection(rewritten: &str) -> Outcome {
    // Re-read the field rather than only asking for the frontmost app.
    // The app name decides whether a typing fallback may batch, and the
    // snapshot additionally says whether AXSelectedText is writable, which
    // is what keeps this off the document-clobbering AXValue path below.
    match ax_edit::snapshot_focused() {
        Ok(snap) if snap.is_selection_edit() => write_over_selection(&snap, rewritten),
        // The selection vanished between key-down and commit (focus moved,
        // a click landed). Writing the rewritten SELECTION as the whole
        // field value would replace the user's document with a fragment,
        // so type it instead: that is what their keyboard would have done.
        // No leading space on either arm: `rewritten` REPLACES text the
        // user selected, it is not a new utterance appended after one. The
        // spacing rule belongs to dictation only, and applying it here
        // would indent every edit by one space.
        Ok(snap) => deliver_without_ax(
            rewritten,
            &AxError::NotSettable,
            typing_strategy(snap.app.as_deref(), false),
            None,
        ),
        Err(e) => deliver_without_ax(
            rewritten,
            &e,
            typing_strategy(ax_edit::frontmost_app().as_deref(), false),
            None,
        ),
    }
}

/// Replace the live selection with `text`, and ONLY through a transport
/// that replaces a selection as such.
///
/// The trap this defuses, verified by reading `ax_edit::replace_focused`:
/// that function prefers `AXSelectedText` but, when the element refuses
/// that attribute, falls through to writing the whole `AXValue`. With a
/// selection live, `text` is selection-sized, so the fallback would set
/// the ENTIRE field to it. A one-word selection in a long document would
/// leave nothing but that one word. The same fallback also destroys the
/// app's native undo, which is the user's only recovery route.
///
/// So the AX write is only attempted when the snapshot says
/// `AXSelectedText` is settable, which is the path that stays scoped to
/// the selection. Otherwise delivery drops to synthesized keystrokes /
/// paste, which replace a selection natively (that is what typing over a
/// selection does) and remain undoable through the app's own Cmd+Z.
///
/// Measured against a live TextEdit window, three consecutive runs
/// (`cargo run -p ax-edit --example undo_semantics`):
///
/// ```text
/// case 1  set-selected-text, whole doc selected -> doc becomes the payload
/// case 2  set-value,         no selection       -> doc becomes the payload
/// case 3  set-selected-text, PARTIAL selection  -> "The customers might possi"
///                                                 becomes "WORD", and
///                                                 "bly be quite upset about
///                                                 this." SURVIVES
/// ```
///
/// Case 3 is the whole argument. `text` is SELECTION-SIZED, so routing it
/// to the `AXValue` branch does not merely lose formatting: it sets the
/// entire field to a fragment. A one-word selection in a long document
/// would leave nothing but that one word.
///
/// One claim was NOT confirmed and is deliberately not relied on here.
/// The brief (and `ax-edit`'s own doc comment) says a full `AXValue`
/// write destroys the app's undo. In TextEdit it did not: Cmd+Z restored
/// the original after the `set-value` write on all three runs. Undo may
/// well be clobbered in other applications, but on the evidence available
/// the justification for this guard is SCOPE, which was reproduced every
/// time, not undo, which was not.
#[cfg(target_os = "macos")]
fn write_over_selection(snap: &TextSnapshot, text: &str) -> Outcome {
    let typing = typing_strategy(snap.app.as_deref(), false);
    if !snap.selected_text_settable {
        // Replacing a selection, so no leading space: the text is standing
        // in for what was highlighted, not following it.
        return deliver_without_ax(text, &AxError::NotSettable, typing, None);
    }
    write_focused(text, typing)
}

/// The typing strategy type as this build knows it: the real enum on a
/// display build, a named empty type on headless where no synthetic-keys
/// tier exists.
///
/// Not `()` for headless. Unit made every call site "pass a unit value to a
/// function", which clippy denies under `-D warnings`, so the headless
/// build failed on Linux while compiling fine on a Mac. A named type also
/// reads as deliberate at the call site, where a bare `()` looks like an
/// oversight.
#[cfg(all(target_os = "macos", feature = "display"))]
type TypingChoice = text_target::targets::keys::TypingStrategy;

#[cfg(all(target_os = "macos", not(feature = "display")))]
#[derive(Debug, Clone, Copy)]
pub struct NoTypingStrategy;
#[cfg(all(target_os = "macos", not(feature = "display")))]
type TypingChoice = NoTypingStrategy;

/// Type `text` using the chosen strategy and name the transport used.
///
/// Returns the `via` string rather than a bool so the log and overlay say
/// WHICH typing path ran: "synthetic-keys-batched" is expected to be ~1ms,
/// "synthetic-keys-paced" is the deliberate slow path for ttys, and seeing
/// the wrong one against a given app is the diagnosis.
#[cfg(all(target_os = "macos", feature = "display"))]
fn type_with_strategy(text: &str, typing: TypingChoice) -> Result<String, String> {
    use text_target::targets::keys::{CgEventTarget, TypingStrategy};
    use text_target::TextTarget;
    match typing {
        TypingStrategy::Batched => match CgEventTarget.insert(text) {
            Ok(()) => Ok("synthetic-keys-batched".into()),
            // A refused batch (no trust, event creation failed) still has
            // the paced path to try before giving up on typing entirely.
            Err(e) => match ax_edit::synth::type_text(text) {
                Ok(()) => Ok(format!("synthetic-keys-paced (batched refused: {e})")),
                Err(e2) => Err(format!("batched: {e}; paced: {e2}")),
            },
        },
        TypingStrategy::PerCharPaced => match ax_edit::synth::type_text(text) {
            Ok(()) => Ok("synthetic-keys-paced".into()),
            Err(e) => Err(e.to_string()),
        },
    }
}

/// Headless macOS build: only the per-character path exists (ax-edit is
/// always linked), so the strategy is moot.
#[cfg(all(target_os = "macos", not(feature = "display")))]
fn type_with_strategy(text: &str, _typing: TypingChoice) -> Result<String, String> {
    ax_edit::synth::type_text(text)
        .map(|()| "synthetic-keys-paced".into())
        .map_err(|e| e.to_string())
}

/// The last-resort transport, for destinations with no writable
/// accessibility field. Terminals are the whole reason it exists: a
/// terminal's "field" is a character grid owned by the program running
/// inside it, so AX reads and writes nothing there.
///
/// Order matters, and it is not the tier order:
///
/// 1. **Synthesized keystrokes** (`ax_edit::synth`). Preferred despite being
///    a lower tier than the clipboard, because it does not touch the user's
///    clipboard at all, and because in a terminal it is the *only* thing
///    that works: text typed at a shell prompt is exactly what the line
///    editor expects, so history, editing, and undo all behave normally.
/// 2. **Clipboard paste.** Atomic, so it stays the better choice for very
///    long text, and it is the fallback when synthesis is refused.
/// 3. **Clipboard only**, with the user told to press Cmd+V. A named next
///    action even at the bottom of the chain.
///
/// The previous implementation went straight to the clipboard and
/// synthesized Cmd+V by shelling out to `osascript`. That fails on a
/// correctly-configured machine, because System Events keystroke synthesis
/// is TCC-gated against *osascript*, not against us:
///
/// ```text
/// System Events got an error: osascript is not allowed to send keystrokes. (1002)
/// ```
///
/// So dictation into a terminal delivered nothing at all, not even to the
/// clipboard, since the paste failure aborted the whole outcome. We already
/// hold the Accessibility grant that keystroke synthesis needs, so posting
/// the events ourselves is both more reliable and a smaller ask of the user
/// than granting a general-purpose scripting interpreter blanket input
/// synthesis.
#[cfg(target_os = "macos")]
fn deliver_without_ax(
    text: &str,
    ax_err: &AxError,
    typing: TypingChoice,
    preceding: Option<char>,
) -> Outcome {
    // Typing and pasting both append blind: neither can read the field, so
    // the caller has to tell them what the caret is sitting behind. Without
    // this, a second utterance lands hard against the first and reads as
    // "you'll see.Right now I'm talking".
    let owned;
    let text = if needs_leading_space(preceding) {
        owned = format!(" {text}");
        &owned
    } else {
        text
    };

    // Logged, not just folded into the outcome: the fallback usually
    // succeeds, and the AX refusal that caused it would otherwise vanish.
    eprintln!("outloud: AX write path refused ({ax_err}); typing it instead");

    // Tier 1: type it. Leaves the clipboard alone entirely. Two typing
    // paths, chosen per destination (see `typing_strategy_for` in
    // text-target): GUI apps take batched multi-character events (~1ms),
    // terminals take the paced per-character path that a tty can keep up
    // with. The `via` string names which, so a slow injection in the log
    // is diagnosable to the strategy rather than a mystery.
    match type_with_strategy(text, typing) {
        Ok(via) => {
            return Outcome::Wrote {
                text: text.to_string(),
                via,
            }
        }
        Err(e) => {
            eprintln!("outloud: keystroke synthesis refused ({e}); falling back to clipboard")
        }
    }

    #[cfg(feature = "display")]
    {
        use text_target::targets::clipboard::ClipboardTarget;
        use text_target::TextTarget;
        match ClipboardTarget::new() {
            Ok(mut clip) => match clip.insert(text) {
                Ok(()) => {
                    // The paste keystroke is already delivered; only the
                    // clipboard *restore* must wait for the app to consume
                    // the pasteboard. That wait is not the user's latency,
                    // so it happens on a detached thread: the outcome (and
                    // the injection timer) returns immediately, and the
                    // user's original clipboard comes back ~300ms later.
                    // 300ms rather than the old inline 150ms because a
                    // busy app reading the pasteboard late now costs
                    // nothing visible.
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        let _ = clip.restore();
                    });
                    Outcome::Wrote {
                        text: text.to_string(),
                        via: "clipboard-paste".into(),
                    }
                }
                // The paste keystroke failed, but the text IS on the
                // clipboard, and deliberately left there rather than
                // restored: the user's next action is to paste it.
                Err(paste_err) => Outcome::Failed {
                    situation_action: format!(
                        "write refused ({ax_err}), typing refused, and paste failed \
                         ({paste_err}) -> your text is on the clipboard, press Cmd+V"
                    ),
                },
            },
            Err(e) => Outcome::Failed {
                situation_action: format!(
                    "write refused ({ax_err}) and no clipboard tool ({e}) \
                     -> focus a text field and try again"
                ),
            },
        }
    }
    #[cfg(not(feature = "display"))]
    {
        let _ = text;
        Outcome::Failed {
            situation_action: format!(
                "write refused ({ax_err}) and this is a headless build \
                 -> use the terminal-native transports via spike-cli"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(selected: Option<&str>) -> TextSnapshot {
        TextSnapshot {
            role: "AXTextArea".into(),
            app: Some("TestApp".into()),
            value: Some("the quick brown fox".into()),
            selected_text: selected.map(str::to_string),
            selection: None,
            value_settable: true,
            selected_text_settable: true,
            ..Default::default()
        }
    }

    #[test]
    fn no_selection_means_dictate() {
        assert_eq!(mode_from_snapshot(Some(&snap(None))), Mode::Dictate);
        assert_eq!(mode_from_snapshot(Some(&snap(Some("")))), Mode::Dictate);
        assert_eq!(mode_from_snapshot(None), Mode::Dictate);
    }

    #[test]
    fn selection_means_edit_on_that_text() {
        assert_eq!(
            mode_from_snapshot(Some(&snap(Some("quick")))),
            Mode::Edit {
                selected: "quick".into()
            }
        );
    }

    #[test]
    fn empty_transcript_is_quiet() {
        assert!(matches!(
            deliver(&Mode::Dictate, "   "),
            Outcome::EmptyTranscript
        ));
    }

    /// The two readings of an unparsed phrase, decided by `payload_for`
    /// because it is pure and therefore assertable without a focused UI
    /// element (and without `OUTLOUD_NO_INJECT` making the assertion
    /// vacuous, which is what happened to the `deliver`-based version of
    /// this test that used to live here).
    ///
    /// Ordinary prose spoken while something happened to be selected is
    /// still dictated: selections linger far longer than users notice, and
    /// refusing every unparsed phrase presented as "the app stopped
    /// transcribing".
    #[test]
    fn unrecognised_prose_with_a_selection_is_dictated() {
        let mode = Mode::Edit {
            selected: "some prose".into(),
        };
        assert_eq!(
            payload_for(&mode, "we should tell them soon").unwrap(),
            "we should tell them soon",
        );
    }

    /// And its mirror: an instruction ABOUT the selection is refused, so
    /// the words describing the edit never become the selection's new
    /// contents. This is the reported corruption.
    #[test]
    fn an_instruction_about_the_selection_is_refused_not_written() {
        let mode = Mode::Edit {
            selected: "The customers might possibly be quite upset about this.".into(),
        };
        assert!(
            matches!(
                payload_for(&mode, "tighten this up"),
                Err(Outcome::FreeformUnsupported { .. })
            ),
            "a rewrite request must never be written over the user's text"
        );
    }

    #[test]
    fn edit_with_absent_search_text_reports_no_match() {
        let mode = Mode::Edit {
            selected: "the quick brown fox".into(),
        };
        match deliver(&mode, "change zebra to lion") {
            Outcome::EditNoMatch { command } => assert!(command.contains("zebra")),
            other => panic!("expected EditNoMatch, got {other:?}"),
        }
    }

    fn caret_snap(value: &str, caret_utf16: Option<usize>) -> TextSnapshot {
        TextSnapshot {
            role: "AXTextArea".into(),
            app: None,
            value: Some(value.into()),
            selected_text: None,
            selection: caret_utf16.map(|c| (c, 0)),
            value_settable: true,
            selected_text_settable: false,
            ..Default::default()
        }
    }

    /// The Terminal.app regression: a readable-but-unwritable text area with
    /// a caret must be treated as read-only, so delivery goes to synthesized
    /// keystrokes instead of splicing a transcript into the scrollback and
    /// writing it back.
    #[test]
    fn read_only_scrollback_is_not_a_write_target() {
        let mut s = caret_snap("$ echo hello\nhello\n$ ", Some(21));
        s.value_settable = false;
        s.selected_text_settable = false;
        assert!(is_read_only(&s));

        // The splice itself is still well-defined; the point is that the
        // caller must never get as far as writing it back.
        assert!(
            spliced_at_caret(&s, "some dictated text").is_some(),
            "guard must be the settability check, not a splice failure"
        );
    }

    /// Either writable attribute is enough: some applications expose only
    /// AXSelectedText, others only AXValue.
    #[test]
    fn either_settable_attribute_makes_it_writable() {
        let mut s = caret_snap("hello", Some(5));

        s.value_settable = true;
        s.selected_text_settable = false;
        assert!(!is_read_only(&s));

        s.value_settable = false;
        s.selected_text_settable = true;
        assert!(!is_read_only(&s));

        s.value_settable = true;
        s.selected_text_settable = true;
        assert!(!is_read_only(&s));
    }

    #[test]
    fn splice_inserts_at_caret_with_joining_space() {
        let s = caret_snap("hello world", Some(5)); // caret after "hello"
        assert_eq!(
            spliced_at_caret(&s, "brave").as_deref(),
            Some("hello brave world")
        );
    }

    #[test]
    fn splice_at_end_appends() {
        let s = caret_snap("hello", Some(5));
        assert_eq!(
            spliced_at_caret(&s, "world").as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn splice_at_start_pads_before_the_word() {
        let s = caret_snap("world", Some(0));
        assert_eq!(
            spliced_at_caret(&s, "hello").as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn splice_into_empty_field_is_just_the_text() {
        let s = caret_snap("", None);
        assert_eq!(spliced_at_caret(&s, "hello").as_deref(), Some("hello"));
    }

    #[test]
    fn splice_without_caret_refuses() {
        let s = caret_snap("hello", None);
        assert_eq!(spliced_at_caret(&s, "x"), None);
    }
}

/// Tests for the platform-tier delivery ladder (Windows today).
///
/// These exist because the ladder makes a CORRECTNESS claim that compiling
/// cannot check and that nobody can currently check on hardware: an edit
/// must never be delivered through an insert-only transport, because that
/// appends the rewrite next to the original rather than replacing it.
#[cfg(test)]
mod tier_tests {
    use super::*;

    fn edit(selected: &str) -> Mode {
        Mode::Edit {
            selected: selected.to_string(),
        }
    }

    #[test]
    fn an_edit_may_never_fall_back_to_an_insert_only_tier() {
        // The whole point: SendInput cannot address existing text, so an
        // edit routed there would corrupt the field silently.
        assert!(!may_use_insert_only_tier(&edit("the quick brown fox")));
        // Dictation is pure insertion, so typing it is a fine degradation.
        assert!(may_use_insert_only_tier(&Mode::Dictate));
    }

    #[test]
    fn dictation_payload_is_the_transcript_verbatim() {
        // No punctuation stripping for dictation: the user dictated the
        // sentence, trailing period included.
        assert_eq!(
            payload_for(&Mode::Dictate, "hello there.").unwrap(),
            "hello there."
        );
    }

    #[test]
    fn an_edit_payload_is_the_rewritten_selection_not_the_command() {
        // The field must receive the REWRITTEN TEXT, never the spoken
        // instruction. Writing the command into the document is the most
        // embarrassing failure this path has.
        let got = payload_for(&edit("the quick brown fox"), "change quick to slow").unwrap();
        assert_eq!(got, "the slow brown fox");
        assert!(
            !got.contains("change"),
            "the instruction must not be written"
        );
    }

    #[test]
    fn spoken_punctuation_is_stripped_from_the_command_not_the_result() {
        // Recognizers add a period the user never said; without stripping,
        // the replacement text becomes "slow." with a stray period.
        let got = payload_for(&edit("the quick brown fox"), "change quick to slow.").unwrap();
        assert_eq!(got, "the slow brown fox");
    }

    #[test]
    fn a_freeform_instruction_is_reported_never_guessed_at() {
        // No local LLM: the honest answer is to say so. Guessing would mean
        // a model rewriting text nobody asked it to touch.
        let out = payload_for(&edit("some text"), "tighten this up");
        assert!(
            matches!(out, Err(Outcome::FreeformUnsupported { .. })),
            "freeform must not silently fall through to a write"
        );
    }

    #[test]
    fn an_unmatched_edit_writes_nothing() {
        // "change zebra to horse" against text with no zebra: the field
        // must be left ALONE rather than receiving anything at all.
        let out = payload_for(&edit("the quick brown fox"), "change zebra to horse");
        assert!(
            matches!(out, Err(Outcome::EditNoMatch { .. })),
            "a non-matching edit must not reach any transport"
        );
    }

    #[test]
    fn an_empty_edit_selection_cannot_produce_a_destructive_write() {
        // Degenerate input from a field that reported a selection it could
        // not return: whatever happens, we must not write the command.
        if let Ok(payload) = payload_for(&edit(""), "change a to b") {
            assert!(
                !payload.contains("change"),
                "never write the spoken instruction into the user's field"
            );
        }
    }

    /// Regression, in both directions at once.
    ///
    /// Dictating an ordinary sentence while something happens to be
    /// selected must INSERT it, not refuse it: that shipped broken and
    /// read as "the app stopped transcribing". Speaking an instruction
    /// ABOUT the selection must refuse, not insert: that also shipped
    /// broken and silently destroyed the selected text.
    #[test]
    fn freeform_phrase_with_a_selection_is_dictation_not_a_failed_edit() {
        let intent = edit_intent::parse("this is just a normal sentence");
        assert!(
            matches!(intent, EditIntent::Freeform { .. }),
            "the fixture must be a phrase that does not parse as a command"
        );
        assert_eq!(
            payload_for(&edit("some prose"), "this is just a normal sentence").unwrap(),
            "this is just a normal sentence",
        );
        assert!(matches!(
            payload_for(&edit("some prose"), "summarize this"),
            Err(Outcome::FreeformUnsupported { .. })
        ));

        // A command, by contrast, must still parse as one so the edit path
        // is not accidentally disabled by the fix above.
        let command = edit_intent::parse("change quick to slow");
        assert!(
            !matches!(command, EditIntent::Freeform { .. }),
            "edit commands must still reach the edit path"
        );
        assert_eq!(
            edit_intent::apply("the quick brown fox", &command).as_deref(),
            Some("the slow brown fox"),
        );
    }

    /// The terminal edit-by-voice decision, as a pure function of the
    /// destination app and the transcript (no bridge, no AX, no terminal).
    #[cfg(feature = "display")]
    mod shell_bridge_routing {
        use super::super::shell_bridge_command;

        #[test]
        fn edit_command_at_a_terminal_is_staged() {
            assert_eq!(
                shell_bridge_command(Some("iTerm2"), "change prod-web to staging-web").as_deref(),
                Some("change prod-web to staging-web"),
            );
        }

        #[test]
        fn recognizer_punctuation_is_stripped_before_staging() {
            // Spoken imperatives carry no trailing period; the recognizer
            // added it, so it must not survive into the staged intent.
            assert_eq!(
                shell_bridge_command(Some("Terminal"), "change prod to staging.").as_deref(),
                Some("change prod to staging"),
            );
        }

        #[test]
        fn freeform_phrase_at_a_terminal_is_dictation() {
            // Someone dictating a commit message into their shell must keep
            // getting typed text; hijacking it into an intent would make
            // plain dictation silently stop working at a prompt.
            assert_eq!(
                shell_bridge_command(Some("Terminal"), "fix the login bug and add tests"),
                None,
            );
        }

        #[test]
        fn edit_command_at_a_gui_app_takes_the_gui_path() {
            assert_eq!(
                shell_bridge_command(Some("Safari"), "change quick to slow"),
                None
            );
            assert_eq!(shell_bridge_command(None, "change quick to slow"), None);
        }
    }

    /// The reported bug: a second utterance glued onto the first.
    ///
    /// Verbatim from the report, dictated into Discord: "...and you'll
    /// see.Right now I'm talking again". The AX splice path had always
    /// spaced correctly, so this only appeared on destinations that refuse
    /// AX writes and fall back to typing, which is exactly where a user is
    /// least likely to blame the transport.
    #[test]
    fn a_second_utterance_is_separated_from_the_first() {
        // Sentence-ending punctuation is the case that was reported.
        assert!(needs_leading_space(Some('.')));
        assert!(needs_leading_space(Some('!')));
        assert!(needs_leading_space(Some('?')));
        // Any other word character too: "hello" + "world", not "helloworld".
        assert!(needs_leading_space(Some('o')));
        // Closing punctuation separates; only OPENERS hug (see the test
        // below). A double quote is ambiguous in ASCII, so it is treated as
        // an opener: gluing onto `"` is far commoner than gluing after one.
        assert!(needs_leading_space(Some(',')));
        assert!(needs_leading_space(Some(')')));
    }

    /// Cases where a space would be the defect rather than the fix.
    #[test]
    fn no_space_where_a_human_would_not_type_one() {
        // Already separated. A second space would be visible.
        assert!(!needs_leading_space(Some(' ')));
        assert!(!needs_leading_space(Some('\n')));
        assert!(!needs_leading_space(Some('\t')));

        // Openers hug what follows: a human types ("hello, never ( "hello.
        for c in ['(', '[', '{', '<', '\'', '\u{201c}', '\u{2018}'] {
            assert!(!needs_leading_space(Some(c)), "opener {c:?} must hug");
        }

        // Unknown means an empty field or an unreadable caret. Erring
        // toward no space is deliberate: a stray leading space shows on
        // every single utterance, while a missing one only shows when
        // appending, and the caller passes the character whenever it has it.
        assert!(!needs_leading_space(None));
    }
}
