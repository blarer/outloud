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

/// What an edit-mode utterance should do, decided without touching the
/// accessibility tree, the clipboard, or the undo ring's contents.
///
/// Separated from delivery so the routing is unit-testable on any platform,
/// and so that `deliver`'s match must list every route: a new intent that
/// nobody wired up is a compile error rather than a silent "no match".
#[derive(Debug, PartialEq, Eq)]
pub enum EditRoute {
    /// "scratch that" -- resolved against the undo ring, not the selection.
    Undo,
    /// Plain dictation that happened while something was selected.
    Dictate { text: String },
    /// An instruction about the selection that nothing here can carry out.
    /// Writes nothing: the selected text must survive.
    Unsupported { instruction: String },
    /// A parsed edit that matched, with the replacement text.
    Rewrite { rewritten: String },
    /// Parsed as an edit but matched nothing in the selection. Worth
    /// reporting rather than inserting.
    NoMatch { command: String },
}

impl EditRoute {
    /// A short, stable name for logs and dry runs. Deliberately carries no
    /// user text: this is printed on a measurement path.
    pub fn label(&self) -> &'static str {
        match self {
            EditRoute::Undo => "undo",
            EditRoute::Dictate { .. } => "dictate",
            EditRoute::Unsupported { .. } => "unsupported",
            EditRoute::Rewrite { .. } => "rewrite",
            EditRoute::NoMatch { .. } => "no-match",
        }
    }
}

/// Decide what an edit-mode utterance means.
pub fn route_edit(text: &str, selected: &str) -> EditRoute {
    // Recognizers punctuate ("Change quick to slow."), but spoken edit
    // commands are imperatives whose trailing punctuation was never said:
    // strip it so "to slow." does not write "slow.".
    let command = text.trim_end_matches(['.', '!', '?', ',']);
    let intent = edit_intent::parse(command);

    // Undo resolves against the ring, not the selection.
    // `edit_intent::apply` returns None for it and says so itself: "which
    // the caller's undo ring resolves". This is that caller.
    if let EditIntent::Undo(_) = &intent {
        return EditRoute::Undo;
    }

    // A selection means an edit is POSSIBLE, not that one was intended, and
    // the two readings of an unparsed phrase have opposite correct
    // behaviours. Text is selected far more often than people realise (a
    // terminal keeps the last drag selected, editors highlight the current
    // word, browsers hold a selection long after the click), so refusing
    // every unparsed phrase turned ordinary dictation into "the app stopped
    // transcribing". But inserting every unparsed phrase meant "tighten
    // this up" REPLACED the selected sentence with the words "Tighten this
    // up.", destroying it silently.
    //
    // `freeform::classify` is the rule that separates them, and it is
    // biased: a wrong refusal costs one retry, a wrong overwrite costs a
    // paragraph. See that module for the signals and the escape hatch
    // ("type: ...").
    if let EditIntent::Freeform { .. } = &intent {
        return match classify(text, selected) {
            FreeformDisposition::Dictate { text } => EditRoute::Dictate { text },
            FreeformDisposition::RewriteRequest { instruction } => {
                EditRoute::Unsupported { instruction }
            }
        };
    }

    match edit_intent::apply(selected, &intent) {
        Some(rewritten) => EditRoute::Rewrite { rewritten },
        None => EditRoute::NoMatch {
            command: text.to_string(),
        },
    }
}

/// The undo ring behind `EditIntent::Undo` ("scratch that", "undo that").
///
/// Process-lifetime, because undo spans utterances by definition: the
/// dictation being undone finished before the one asking for the undo began.
/// Depth 10 is the roadmap's stated exit criterion.
///
/// `crates/stream/src/undo.rs` was a complete, tested ring with no caller for
/// weeks. The phrase parsed, `edit_intent::apply` returned None for it saying
/// "the caller's undo ring resolves this", and no such caller existed, so the
/// user was told their command did not match.
#[cfg(target_os = "macos")]
static UNDO: std::sync::Mutex<Option<stream::undo::UndoRing>> = std::sync::Mutex::new(None);

/// Record a completed edit so it can be undone.
///
/// A unit that changed nothing is dropped by the ring itself: an undo step
/// that does nothing would make "scratch that" feel broken.
#[cfg(target_os = "macos")]
fn record_undo(before: &str, after: &str) {
    // A poisoned lock means a panic happened mid-record. Undo history is a
    // convenience rather than anything the user typed, so losing it beats
    // refusing to dictate.
    let mut guard = UNDO.lock().unwrap_or_else(|p| p.into_inner());
    let ring = guard.get_or_insert_with(|| stream::undo::UndoRing::new(10));
    ring.begin_unit(before, None);
    ring.end_unit(after);
}

/// Resolve an undo against the ring and the field's current contents.
///
/// Reads the field back first so the ring's stale-snapshot guard can compare
/// what we wrote against what is there now, and decline rather than destroy
/// work the user did afterwards.
#[cfg(target_os = "macos")]
fn apply_undo() -> Outcome {
    let now = match ax_edit::snapshot_focused() {
        Ok(snap) => snap.value.unwrap_or_default(),
        Err(e) => {
            return Outcome::Failed {
                situation_action: format!(
                    "could not read the field to undo into ({e}) -> click into it and try again"
                ),
            }
        }
    };
    let mut guard = UNDO.lock().unwrap_or_else(|p| p.into_inner());
    let outcome = match guard.as_mut() {
        Some(ring) => ring.undo(&now),
        None => stream::undo::UndoOutcome::Empty,
    };
    drop(guard);
    undo_outcome_to_result(outcome)
}

/// The half of undo that needs no accessibility tree, so the routing can be
/// tested at all. The live read is exactly the dependency that let the
/// unwired ring go unnoticed.
#[cfg(target_os = "macos")]
fn undo_outcome_to_result(outcome: stream::undo::UndoOutcome) -> Outcome {
    use stream::undo::UndoOutcome;
    match outcome {
        // `write_focused` writes a whole field value and already routes
        // through the per-app transport rules, so undoing in Discord takes
        // the same clipboard path a dictation does.
        UndoOutcome::Restore(unit) => write_focused(
            &unit.before,
            typing_strategy(None, must_pace_typing(AxRefusal::WriteIgnored)),
        ),
        // Deliberately not forced: restoring here would destroy edits the
        // user made after ours, which is worse than declining.
        UndoOutcome::FieldChanged { .. } => Outcome::EditNoMatch {
            command: "undo: nothing to undo, the field changed since".to_string(),
        },
        UndoOutcome::Empty => Outcome::EditNoMatch {
            command: "undo: nothing to undo yet".to_string(),
        },
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
///
/// Not gated to macOS. The pipeline calls this unconditionally, and a
/// macOS-only definition compiled fine here while breaking every other
/// target -- which is exactly what CI's Linux, Windows and msrv jobs are
/// for. On platforms with no snapshot the argument is `None` and this
/// returns `None`, so profiles are simply unresolved rather than absent
/// at compile time.
pub fn app_identity(snap: Option<&TextSnapshot>) -> Option<config::AppIdentity> {
    if let Some(snap) = snap {
        if snap.bundle_id.is_some() || snap.app.is_some() {
            return Some(config::AppIdentity {
                bundle_id: snap.bundle_id.clone(),
                process_name: snap.app.clone(),
                window_class: None,
            });
        }
    }
    // No snapshot: ask the OS directly.
    //
    // Windows never has one, because `snapshot_and_mode_at_keydown` returns
    // None for it, so this returned None every time and `resolve_for_app` was
    // never called. The whole [profile.*] feature documented in
    // docs/configuration.md was unreachable on Windows while appearing to be
    // supported.
    //
    // `match.bundle-id` still cannot match here: Windows has no bundle ids.
    // `match.process-name` can, which is what the docs already suggest for
    // terminal programs.
    #[cfg(all(target_os = "windows", feature = "display"))]
    {
        let name = text_target::targets::keys::foreground_process_name()?;
        return Some(config::AppIdentity {
            bundle_id: None,
            process_name: Some(name),
            window_class: None,
        });
    }
    #[cfg(not(all(target_os = "windows", feature = "display")))]
    None
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
        // Report the ROUTE an edit would take, not just the transcript.
        // Returning early with the raw text meant no automated run could
        // ever observe the edit routing, which is precisely how the undo
        // ring stayed unreachable: the only way to exercise it was to speak
        // into a real window and watch. Routing is pure, so a dry run can
        // answer "what would this have done" without touching the UI.
        let text = match mode {
            Mode::Edit { selected } => {
                format!("{text} [route: {}]", route_edit(text, selected).label())
            }
            Mode::Dictate => text.to_string(),
        };
        return Outcome::Suppressed { text };
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
        Mode::Edit { selected } => match route_edit(text, selected) {
            // Every route is dispatched here and nowhere else. An enum
            // rather than a chain of early returns because a forgotten
            // branch then fails to COMPILE instead of silently falling
            // through to "that command did not match" -- which is exactly
            // how the undo ring sat unreachable behind a passing test
            // suite. Adding a variant must break this match.
            EditRoute::Undo => apply_undo(),
            EditRoute::Dictate { text } => insert_with_fallback(&text),
            EditRoute::Unsupported { instruction } => Outcome::FreeformUnsupported { instruction },
            EditRoute::Rewrite { rewritten } => {
                let outcome = replace_selection(&rewritten);
                // Record only a write that happened, so every undo step
                // corresponds to something the user actually saw.
                if let Outcome::Wrote { .. } = &outcome {
                    record_undo(selected, &rewritten);
                }
                outcome
            }
            EditRoute::NoMatch { command } => Outcome::EditNoMatch { command },
        },
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
            // Undo is not wired on the tier platforms: restoring needs to
            // read the field's CURRENT text back, which the tier ladder has
            // no equivalent of yet (see the macOS `apply_undo`).
            //
            // Say so, rather than falling through to `apply`, which returns
            // None for Undo and would report the user's own phrase as an
            // unmatched command. Being told "not supported here" is a fact
            // to act on; being told "scratch that did not match" is a lie
            // that sends someone looking for a typo in what they said.
            if let EditIntent::Undo(_) = &intent {
                return Err(Outcome::EditNoMatch {
                    command: "undo: not supported on this platform yet".to_string(),
                });
            }
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

/// The frontmost application's name, for the per-app transport rules.
///
/// Windows has no `ax_edit`, so this asks the OS directly. `None` when the
/// window or its process cannot be identified, which `accepts` treats as an
/// ordinary destination: assuming the worst would push every unrecognised app
/// onto the clipboard and clobber the pasteboard during ordinary dictation.
#[cfg(all(target_os = "windows", feature = "display"))]
fn frontmost_app_name() -> Option<String> {
    text_target::targets::keys::foreground_process_name()
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
        use text_target::targets::keys::{accepts, Acceptance};
        use text_target::TextTarget;

        // The same per-app transport question macOS asks, asked here too.
        //
        // This path did not ask it at all, so every per-app rule was
        // macOS-only. That is not a theoretical gap: Discord accepts an
        // accessibility write, reports success, and reverts it a moment
        // later, which is exactly the failure that took five separate fixes
        // on macOS because each write path had to remember the rule
        // independently. Windows had none of them.
        //
        // `accepts` keys on the app name, so a destination we cannot name
        // resolves to AxAndTyping and behaves as before.
        let acceptance = accepts(frontmost_app_name().as_deref());
        let uia_err = if acceptance == Acceptance::ClipboardOnly {
            // Skip both UIA and synthetic keys: this app discards both.
            "destination discards accessibility writes and synthetic keys".to_string()
        } else {
            match UiaTarget::new() {
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
            }
        };
        eprintln!("outloud: UI Automation write refused ({uia_err}); falling back");

        // Tier 3: SendInput. Insert-only, so an edit that reaches here
        // would APPEND the rewritten text next to the original rather than
        // replacing it. That is a corruption, not a degradation, so edits
        // stop at the clipboard (where the user pastes over their own
        // selection deliberately) and only dictation types.
        // TypingOnly apps ignore accessibility writes but take keystrokes,
        // so they reach here and type normally. ClipboardOnly apps discard
        // keystrokes too, so typing would report success into a field that
        // empties itself a second later; they fall through to the clipboard.
        if may_use_insert_only_tier(mode) && acceptance != Acceptance::ClipboardOnly {
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
/// Why the AX tier was abandoned, which decides how the typing fallback runs.
///
/// Extracted because a unit test over `typing_strategy_for` passes happily
/// while a CALLER hands it the wrong flag, and that is exactly what happened:
/// the AX-ignored branch passed `true`, forcing ~73ms per sentence onto the
/// paced path in nine apps that are not terminals. Naming the reason makes
/// the two cases impossible to conflate, and makes the choice testable
/// without a live accessibility tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub enum AxRefusal {
    /// The app accepts an `AXValue` write and ignores it (Slack, Notion, and
    /// the rest of `AX_VALUE_IGNORED_APPS`). Says nothing about typing speed:
    /// these are GUI apps that take keystrokes as fast as anything else.
    WriteIgnored,
    /// The field reads back and refuses every write. This IS the
    /// accessibility signature of a terminal scrollback, including from
    /// emulators whose names we do not know, so pacing is forced on it
    /// regardless of the app.
    ReadOnlyField,
}

/// Whether `reason` forces the paced per-character path on its own.
#[cfg(any(target_os = "macos", test))]
pub fn must_pace_typing(reason: AxRefusal) -> bool {
    match reason {
        AxRefusal::WriteIgnored => false,
        AxRefusal::ReadOnlyField => true,
    }
}

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
            // Checked BEFORE the AXValue rule, because an app can fail both
            // tiers and this is the narrower fact. Discord ignores AXValue
            // writes AND discards synthetic keystrokes about a second after
            // they land (measured; see docs/compat-matrix.md), so typing was
            // reporting success into a field that then emptied itself.
            //
            // A real paste is the remaining option: it travels the same path
            // as the user's own Cmd-V, which an editor cannot easily tell
            // apart from a human.
            #[cfg(feature = "display")]
            if matches!(
                text_target::targets::keys::accepts(snap.app.as_deref()),
                text_target::targets::keys::Acceptance::ClipboardOnly
            ) {
                return paste_with_leading_space(text, char_before_caret(&snap));
            }

            // Same feature gate as the clipboard branch above: the per-app
            // lists live behind `display` because they describe GUI
            // destinations, and a headless build has none to exclude.
            #[cfg(feature = "display")]
            if matches!(
                text_target::targets::keys::accepts(snap.app.as_deref()),
                text_target::targets::keys::Acceptance::TypingOnly
            ) {
                return deliver_without_ax(
                    text,
                    &AxError::NotSettable,
                    // `false`: these apps ignore an AXValue WRITE, which says
                    // nothing about how fast they accept keystrokes. Passing
                    // `true` short-circuits typing_strategy_for straight to
                    // PerCharPaced, which exists for tty-backed destinations
                    // that drop batched events. At 700us/char that spent an
                    // extra ~70ms on a 100-character sentence in Slack,
                    // Notion, Linear, Figma, Signal, Element, Teams,
                    // Obsidian and Spotify, none of which are terminals.
                    //
                    // The read-only branch below is where `true` belongs: a
                    // readable-but-unwritable field IS the accessibility
                    // signature of a terminal scrollback.
                    typing_strategy(
                        snap.app.as_deref(),
                        must_pace_typing(AxRefusal::WriteIgnored),
                    ),
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
                    typing_strategy(
                        snap.app.as_deref(),
                        must_pace_typing(AxRefusal::ReadOnlyField),
                    ),
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
    // Apps whose editors ignore AX writes, and this function is reachable
    // from the EDIT path (replace_selection -> write_over_selection) which
    // never consults the destination at all.
    //
    // `replace_focused` REPORTS SUCCESS in those apps, which is worse than a
    // refusal: the fallback never runs, so the outcome says "wrote" while the
    // app quietly reverts. Checking here covers every caller, present and
    // future, rather than adding another place to remember it.
    //
    // BOTH lists are consulted, because they answer different questions and
    // the edit path skipped both:
    //   - discards_synthetic_typing (Discord): typing is discarded too, so
    //     clipboard paste is the only transport left.
    //   - ignores_ax_value_writes (Slack, Notion, Linear, Figma, Signal,
    //     Element, Teams, Obsidian, Spotify): the AX write is ignored, but
    //     typing works, so fall through to the typing ladder rather than
    //     clobbering the clipboard.
    #[cfg(feature = "display")]
    {
        use text_target::targets::keys::{accepts, Acceptance};
        match accepts(ax_edit::frontmost_app().as_deref()) {
            // No leading space on either fallback: callers pass a spliced
            // whole-field value or a selection replacement, both already
            // spaced upstream.
            Acceptance::ClipboardOnly => return paste_with_leading_space(text, None),
            // NotSettable is the honest reason: the element accepts the write
            // and does not honour it, which is what "the AX tier is unusable"
            // means from here.
            Acceptance::TypingOnly => {
                return deliver_without_ax(text, &AxError::NotSettable, typing, None)
            }
            Acceptance::AxAndTyping => {}
        }
    }
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
/// Whether focus is still on the app that was targeted at key-down.
///
/// Dictation aims at whatever holds focus when the key goes down, and the
/// write lands a few hundred milliseconds later. Anything that raises a
/// window in between silently redirects the text: chat apps do this on a new
/// message, and so does any app with a notification that steals focus.
///
/// This was observed rather than imagined. While testing Messages, Discord
/// repeatedly raised itself mid-utterance, and dictations aimed at Messages
/// landed in Discord. From the user's side that is indistinguishable from
/// "dictation does not work in this app", which is exactly how it was first
/// reported.
///
/// Returns the name of the app that has focus NOW when it differs from the
/// one targeted, so the caller can say where the text actually went. `None`
/// means focus is unchanged, or that neither app could be identified, in
/// which case claiming a move would be a guess.
pub fn focus_moved_to(targeted: Option<&str>) -> Option<String> {
    // Only macOS can answer "what has focus right now". Elsewhere the honest
    // answer is "cannot tell", which `focus_changed` renders as no warning:
    // claiming a move on missing information would send users hunting for a
    // window that never held their text.
    #[cfg(target_os = "macos")]
    {
        // Same fallback as the key-down side, and for the same reason: an
        // app that just stole focus may not have a focused text element yet,
        // and comparing a real name against None reads as "cannot tell",
        // which suppresses the warning exactly when it is most wanted.
        //
        // Snapshot first, since it cannot race focus the way this can.
        let now = ax_edit::snapshot_focused()
            .ok()
            .and_then(|s| s.app)
            .or_else(ax_edit::frontmost_app);
        focus_changed(targeted, now.as_deref())
    }
    #[cfg(not(target_os = "macos"))]
    {
        focus_changed(targeted, None)
    }
}

/// The pure half of [`focus_moved_to`]: given the app targeted at key-down
/// and the app focused now, did the target move?
///
/// Split out because the comparison is the part with rules worth pinning,
/// while "what has focus right now" is an OS lookup no test can fake. It is
/// also the part that must not guess: an unknown on either side means the
/// answer is "cannot tell", and reporting a move on missing information
/// would send users chasing a window that never had their text.
fn focus_changed(targeted: Option<&str>, current: Option<&str>) -> Option<String> {
    let was = targeted?;
    let now = current?;
    if now == was {
        return None;
    }
    Some(now.to_string())
}

/// Paste `text` at the caret, adding the separating space the join needs.
///
/// For destinations that accept neither an AXValue write nor synthetic
/// typing. Skips both tiers rather than falling through them: trying a
/// transport already known to fail costs a visible second of wrong text in
/// the user's message box before it is discarded.
///
/// The clipboard is saved and restored around the paste, so a dictation does
/// not silently eat whatever the user had copied.
#[cfg(all(target_os = "macos", feature = "display"))]
fn paste_with_leading_space(text: &str, preceding: Option<char>) -> Outcome {
    use text_target::targets::clipboard::ClipboardTarget;
    use text_target::TextTarget;

    let owned;
    let payload = if needs_leading_space(preceding) {
        owned = format!(" {text}");
        &owned
    } else {
        text
    };

    match ClipboardTarget::new() {
        Ok(mut clip) => match clip.insert(payload) {
            Ok(()) => {
                // Let the target consume the paste before handing the user's
                // own clipboard back; restoring too early races the app and
                // pastes the wrong thing.
                std::thread::sleep(std::time::Duration::from_millis(150));
                let _ = clip.restore();
                Outcome::Wrote {
                    text: payload.to_string(),
                    via: "clipboard-paste".into(),
                }
            }
            Err(e) => Outcome::Failed {
                situation_action: format!(
                    "clipboard paste refused ({e}) -> check Accessibility permission"
                ),
            },
        },
        Err(e) => Outcome::Failed {
            situation_action: format!(
                "clipboard unavailable ({e}) -> check Accessibility permission"
            ),
        },
    }
}

/// Whether a failed typing attempt is known to have delivered NOTHING.
///
/// Retrying is only safe when the answer is yes. See the call site for the
/// corruption that results otherwise.
#[cfg(all(target_os = "macos", feature = "display"))]
fn retry_is_safe(e: &text_target::TargetError) -> bool {
    // Unsupported is produced before any event is posted; every other
    // variant can follow a partially delivered sequence.
    matches!(e, text_target::TargetError::Unsupported(_))
}

#[cfg(all(target_os = "macos", feature = "display"))]
fn type_with_strategy(text: &str, typing: TypingChoice) -> Result<String, String> {
    use text_target::targets::keys::{CgEventTarget, TypingStrategy};
    use text_target::TextTarget;
    match typing {
        TypingStrategy::Batched => match CgEventTarget.insert(text) {
            Ok(()) => Ok("synthetic-keys-batched".into()),
            // Retry ONLY when the batched attempt is known to have delivered
            // nothing. Anything else and the paced path types the whole
            // string on top of a partial one.
            //
            // This shipped broken and was caught in live use: dictating into
            // Discord a second time produced
            //   "  tthhee  qquuiicckk  bbrroowwnn the quick brown"
            // which is the text with every character doubled, then a clean
            // copy. Discord's editor was left in a state where the batched
            // post failed partway, the fallback retyped everything, and the
            // field ended up unusable until the app was restarted.
            //
            // `Unsupported` is the safe case: `CgEventTarget::insert` returns
            // it before posting anything (empty text, or no Accessibility
            // trust), so nothing reached the field. `Transport` is not safe:
            // it means event creation failed mid-sequence, after earlier
            // chunks were already posted.
            Err(e) if retry_is_safe(&e) => match ax_edit::synth::type_text(text) {
                Ok(()) => Ok(format!("synthetic-keys-paced (batched refused: {e})")),
                Err(e2) => Err(format!("batched: {e}; paced: {e2}")),
            },
            // Partial delivery. Report it rather than retrying: half a
            // sentence the user can see and fix beats a doubled one they
            // have to decipher, and silently making it worse is the failure
            // mode that cost a Discord restart per utterance.
            Err(e) => Err(format!(
                "batched typing failed after posting some text ({e}); \
                 not retyping, because that would duplicate what landed"
            )),
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
    // Last line of defence for apps that throw synthetic keystrokes away.
    //
    // The splice path checks this before choosing a transport, but every
    // fallback below it re-derives a TYPING strategy and loses the fact, so
    // an app on the discard list still got typed into whenever the AX write
    // was refused for any reason. Measured live in Discord: three
    // consecutive dictations were served by clipboard-paste, ax-stream and
    // synthetic-keys-paced, and only the first survived. Checking here means
    // every route out of this function respects it, including ones added
    // later.
    #[cfg(feature = "display")]
    if matches!(
        text_target::targets::keys::accepts(ax_edit::frontmost_app().as_deref()),
        text_target::targets::keys::Acceptance::ClipboardOnly
    ) {
        return paste_with_leading_space(text, preceding);
    }
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
        // Excludes the suppression switch even though this test does not
        // set it: a sibling test that does would otherwise turn this into
        // Suppressed and fail it.
        let _guard = crate::testenv::deliver_lock();
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

    /// A failed write may only be retried when it delivered nothing.
    ///
    /// The live failure this encodes, probed straight out of Discord:
    ///
    ///   "  tthhee  qquuiicckk  bbrroowwnn the quick brown"
    ///
    /// which is " the quick brown" with every character doubled, followed by
    /// a clean copy. The batched CGEvent path had posted some chunks before
    /// failing, the caller treated the whole attempt as failed, and the
    /// paced path retyped the entire string on top.
    ///
    /// The rule is about what the error PROVES, not which error it is:
    /// `Unsupported` is returned before anything is posted, so a retry is
    /// safe. `Transport` means the sequence broke partway, so it is not.
    #[test]
    #[cfg(all(target_os = "macos", feature = "display"))]
    fn only_a_provably_empty_failure_may_be_retried() {
        use text_target::TargetError;

        // Returned before a single event is posted (empty text, or missing
        // Accessibility trust), so nothing reached the field.
        assert!(
            retry_is_safe(&TargetError::Unsupported("no trust")),
            "a refusal that posted nothing is safe to retry"
        );

        // Returned when event creation failed mid-sequence, which says
        // nothing about how many earlier chunks already landed.
        assert!(
            !retry_is_safe(&TargetError::Transport("event creation failed".into())),
            "a partial write must never be retyped"
        );
    }

    /// Focus moving mid-utterance sends the text somewhere the user is not
    /// looking, and they cannot tell that from the app being broken.
    ///
    /// Observed while testing Messages: Discord raised itself and dictations
    /// aimed at Messages landed in Discord. That is almost certainly what
    /// "dictation does not work in iMessage" was: the text was never lost,
    /// it was delivered to whichever window had grabbed focus.
    ///
    /// Reproduced end to end after the fix: with Messages targeted and
    /// Discord raising itself mid-utterance, the overlay names Discord.
    ///
    /// A note on a wrong turn, kept because it was nearly believed: an
    /// earlier probe read snapshot.app=None for Messages, and I concluded
    /// the warning had been structurally dead there. Reverting the
    /// frontmost_app fallback disproved it, the warning still fired, and a
    /// direct probe with Messages frontmost showed app=Some("Messages") on
    /// an AXTextField. The None was simply a moment with no focused field,
    /// not a property of the app. The fallback is still right, because
    /// Discord genuinely does return None, but Messages was never the case
    /// that needed it.
    #[test]
    fn a_moved_target_names_where_the_text_went() {
        assert_eq!(
            focus_changed(Some("Messages"), Some("Discord")),
            Some("Discord".to_string())
        );
    }

    /// Silence is correct when nothing moved, and when the answer is unknown.
    ///
    /// The unknown cases matter more than they look: warning on missing
    /// information would send users hunting for a window that never held
    /// their text, which is worse than saying nothing.
    #[test]
    fn an_unknown_target_never_claims_a_move() {
        assert_eq!(focus_changed(Some("Messages"), Some("Messages")), None);
        assert_eq!(focus_changed(None, Some("Discord")), None);
        assert_eq!(focus_changed(Some("Messages"), None), None);
        assert_eq!(focus_changed(None, None), None);
    }

    /// Why both sides of the focus comparison need a `frontmost_app`
    /// fallback, pinned as a rule rather than as a live AX call.
    ///
    /// `snapshot_focused()` fails outright when no TEXT element is focused,
    /// and returns None for the app name with it. Measured live: Discord
    /// gives frontmost_app=Some("Discord") but snapshot.app=None. Feeding
    /// that None into the comparison reads as "cannot tell", so the warning
    /// was silently dead in exactly the AX-hostile apps most likely to steal
    /// focus, while every unit test and the FAKE_TARGET path still passed.
    #[test]
    fn a_missing_app_name_disables_the_warning_which_is_why_the_fallback_exists() {
        // A None on either side must stay silent: claiming a move on missing
        // information sends users hunting for a window that never held their
        // text. This is correct, and it is also the failure mode.
        assert_eq!(focus_changed(None, Some("Discord")), None);
        assert_eq!(focus_changed(Some("TextEdit"), None), None);

        // So when both names ARE available, the move must be reported. The
        // fallback's whole job is to keep us in this case.
        assert_eq!(
            focus_changed(Some("TextEdit"), Some("Discord")),
            Some("Discord".to_string()),
            "with both names known the move must be named"
        );
        assert_eq!(focus_changed(Some("Discord"), Some("Discord")), None);
    }

    /// The two reasons the AX tier gets abandoned must not be conflated.
    ///
    /// Regression: the AX-ignored branch passed the read-only flag, which
    /// short-circuits typing_strategy_for to PerCharPaced before it looks at
    /// the app. At 700us/char that is a ~73ms floor on a 104-character
    /// sentence, paid by Slack, Notion, Linear, Figma, Signal, Element,
    /// Teams, Obsidian and Spotify, none of which are terminals.
    ///
    /// Honest limit, verified rather than assumed: this test does NOT catch
    /// the original bug either. Swapping the two constants back at the call
    /// site leaves the whole suite green, because choosing between them
    /// depends on a live accessibility snapshot no unit test can produce.
    ///
    /// What the named enum buys is that the mistake is now READABLE. The old
    /// call passed a bare `true` whose meaning lived in another crate's
    /// parameter name; the call now says `AxRefusal::ReadOnlyField` in a
    /// branch that has just finished proving the field is writable, which
    /// contradicts itself in front of the reader. That is a weaker guarantee
    /// than a failing test and is worth naming as such.
    #[test]
    fn the_two_ax_refusals_choose_different_typing_paths() {
        assert!(
            !must_pace_typing(AxRefusal::WriteIgnored),
            "an ignored AXValue write says nothing about keystroke speed"
        );
        assert!(
            must_pace_typing(AxRefusal::ReadOnlyField),
            "a field refusing every write is the terminal signature"
        );

        // End to end through the real strategy chooser, so the assertion is
        // about the path a user's text actually takes.
        use text_target::targets::keys::{typing_strategy_for, TypingStrategy};
        assert_eq!(
            typing_strategy_for(Some("Slack"), must_pace_typing(AxRefusal::WriteIgnored)),
            TypingStrategy::Batched,
            "Slack ignores AX writes but types at full speed"
        );
        assert_eq!(
            typing_strategy_for(Some("Slack"), must_pace_typing(AxRefusal::ReadOnlyField)),
            TypingStrategy::PerCharPaced,
            "the same app pinned to a read-only field must still be paced"
        );
    }

    /// "scratch that" must be answered by the undo ring.
    ///
    /// Every piece of undo existed and passed its own tests for weeks while
    /// nothing connected them: the ring in `stream::undo`, the parse in
    /// `edit_intent`, and `apply` returning None with a doc comment saying
    /// "the caller's undo ring resolves" it. No caller was ever written, so
    /// the user said "scratch that" and was told the command did not match.
    ///
    /// Testing the pieces is what let that happen, so this asserts on the
    /// connection: the empty ring's own wording. Un-wired, an Undo intent
    /// falls through to `edit_intent::apply`, which returns None, and
    /// EditNoMatch carries the user's raw phrase instead.
    #[test]
    fn an_undo_command_is_answered_by_the_ring() {
        for phrase in ["scratch that", "undo that", "scratch this"] {
            let intent = edit_intent::parse(phrase);
            assert!(
                matches!(intent, EditIntent::Undo(_)),
                "{phrase:?} must parse as Undo, got {intent:?}"
            );
            // The routing assertion: the phrase must reach the ring even
            // with text selected, which is when an edit looks plausible.
            assert_eq!(
                route_edit(phrase, "some selected text"),
                EditRoute::Undo,
                "{phrase:?} must route to the undo ring"
            );
            // apply declining is what makes a caller mandatory rather than
            // optional, so it is part of the contract worth pinning.
            assert!(
                edit_intent::apply("some selected text", &intent).is_none(),
                "apply must leave Undo to the ring"
            );
        }

        // The ring's answer for "nothing recorded yet", which is the wording
        // a user gets and the thing a missing call site cannot produce.
        // macOS-only because the restore path writes through the
        // accessibility tree; the routing assertions above are checked on
        // every platform.
        #[cfg(target_os = "macos")]
        match undo_outcome_to_result(stream::undo::UndoOutcome::Empty) {
            Outcome::EditNoMatch { command } => assert!(
                command.starts_with("undo:"),
                "the ring must answer in its own words, got {command:?}"
            ),
            other => panic!("expected the ring to answer, got {other:?}"),
        }

        // And a recorded edit comes back restorable rather than being
        // reported as a failed match.
        let mut ring = stream::undo::UndoRing::new(10);
        ring.begin_unit("before", None);
        ring.end_unit("after");
        assert_eq!(ring.len(), 1, "a real change must be undoable");
        assert!(
            matches!(ring.undo("after"), stream::undo::UndoOutcome::Restore(_)),
            "an unchanged field must restore"
        );
    }

    /// A dry run must report the edit ROUTE, not just the transcript.
    ///
    /// OUTLOUD_NO_INJECT returned before any routing happened, so no
    /// automated run could observe which branch an edit command took. The
    /// only way to find out was to speak into a live window and watch,
    /// which is how an unreachable undo ring survived for weeks.
    #[test]
    fn a_dry_run_reports_which_route_an_edit_took() {
        // Sets the switch AND holds the lock for as long as it is set.
        let _guard = crate::testenv::no_inject();
        let mode = Mode::Edit {
            selected: "the quick brown fox".to_string(),
        };
        let outcome = deliver(&mode, "scratch that");

        match outcome {
            Outcome::Suppressed { text } => assert!(
                text.contains("[route: undo]"),
                "a dry run must name the route, got {text:?}"
            ),
            other => panic!("expected suppression, got {other:?}"),
        }
    }
}
