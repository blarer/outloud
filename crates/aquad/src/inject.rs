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

use ax_edit::{AxError, TextSnapshot};
use edit_intent::EditIntent;

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
                eprintln!("aquad: could not read the focused element ({e}); assuming dictation");
                Mode::Dictate
            }
        }
    }

    #[cfg(not(any(target_os = "macos", all(target_os = "windows", feature = "display"))))]
    Mode::Dictate
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

    // Non-macOS platforms have no ax-edit, so they take the platform-tier
    // path below rather than the AX-specific splice logic. Keeping the two
    // separate (instead of abstracting ax-edit away) means the macOS path,
    // the only one measured end to end, is byte-for-byte what M0 proved.
    #[cfg(not(target_os = "macos"))]
    {
        return deliver_via_tiers(mode, text);
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
            if let EditIntent::Freeform { instruction } = &intent {
                // No local LLM yet: say so instead of guessing (the README's
                // "a model rewriting text nobody asked it to touch" risk).
                return Outcome::FreeformUnsupported {
                    instruction: instruction.clone(),
                };
            }
            match edit_intent::apply(selected, &intent) {
                Some(rewritten) => replace_selection(&rewritten),
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
            if let EditIntent::Freeform { instruction } = &intent {
                return Err(Outcome::FreeformUnsupported {
                    instruction: instruction.clone(),
                });
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

#[cfg(not(target_os = "macos"))]
fn deliver_via_tiers(mode: &Mode, text: &str) -> Outcome {
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
        eprintln!("aquad: UI Automation write refused ({uia_err}); falling back");

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
                Err(e) => eprintln!("aquad: SendInput refused ({e}); falling back to clipboard"),
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
            // A non-empty selection at commit time: typing replaces it, so
            // dictation does too. replace_focused takes the undo-preserving
            // AXSelectedText path here.
            if snap.is_selection_edit() {
                return write_focused(text);
            }
            match spliced_at_caret(&snap, text) {
                Some(new_value) => write_focused(&new_value),
                // Field readable but caret unknown/unmappable: paste inserts
                // at the caret without us knowing where it is.
                None => clipboard_fallback(text, &AxError::NoTextValue),
            }
        }
        Err(e) => clipboard_fallback(text, &e),
    }
}

/// The spliced whole-field value for inserting `text` at the caret, or
/// `None` when the snapshot does not pin down where the caret is.
#[cfg(target_os = "macos")]
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

/// One AX write with clipboard fallback, shared by both paths.
#[cfg(target_os = "macos")]
fn write_focused(text: &str) -> Outcome {
    match ax_edit::replace_focused(text) {
        Ok(strategy) => Outcome::Wrote {
            text: text.to_string(),
            via: strategy.to_string(),
        },
        Err(e) => clipboard_fallback(text, &e),
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
    write_focused(rewritten)
}

/// The last-resort transport. On success the text is on screen via a
/// synthesized paste; on failure the text is at least *on the clipboard*,
/// and the outcome tells the user to press Cmd+V themselves: a named next
/// action even at the bottom of the fallback chain.
#[cfg(target_os = "macos")]
fn clipboard_fallback(text: &str, ax_err: &AxError) -> Outcome {
    // Logged, not just folded into the outcome: the fallback usually
    // succeeds, and the AX refusal that caused it would otherwise vanish.
    eprintln!("aquad: AX write path refused ({ax_err}); falling back to clipboard paste");
    #[cfg(feature = "display")]
    {
        use text_target::targets::clipboard::ClipboardTarget;
        use text_target::TextTarget;
        match ClipboardTarget::new() {
            Ok(mut clip) => match clip.insert(text) {
                Ok(()) => {
                    // Give the target app a beat to consume the paste before
                    // handing the user's original clipboard back.
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let _ = clip.restore();
                    Outcome::Wrote {
                        text: text.to_string(),
                        via: "clipboard-paste".into(),
                    }
                }
                Err(paste_err) => Outcome::Failed {
                    situation_action: format!(
                        "write refused ({ax_err}) and paste failed ({paste_err}) \
                         -> your text is on the clipboard, press Cmd+V"
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

    #[test]
    fn freeform_edit_reports_instead_of_guessing() {
        let mode = Mode::Edit {
            selected: "some prose".into(),
        };
        match deliver(&mode, "make this sound more professional") {
            Outcome::FreeformUnsupported { instruction } => {
                assert!(instruction.contains("professional"));
            }
            other => panic!("expected FreeformUnsupported, got {other:?}"),
        }
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
        }
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
        let out = payload_for(&edit("some text"), "make this sound more professional");
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
}
