//! The streaming-partials driver: `stream::DictationSession` wired to a
//! writer thread that owns an [`crate::ax_stream::AxRegion`].
//!
//! Division of labour, matching the pipeline's rule that the event loop
//! never blocks on a target application:
//!
//! - The **supervisor** owns the [`Streamer`]: it feeds partials into the
//!   session (pure logic, microseconds), and forwards any released
//!   [`WriteCommand`] to the writer thread over an unbounded channel.
//! - The **writer thread** performs the AX round trips (~0.5ms warm,
//!   unbounded in a wedged app) and reports each completion back as a
//!   [`StreamerEvent`] on the supervisor's event channel. The session's
//!   coalescer holds further releases until that completion arrives, which
//!   is exactly the backpressure contract from `crates/stream`: a slow
//!   transport folds intermediates together instead of queueing them.
//!
//! The session enforces the safety rules (never retract, ~80ms cadence,
//! word-boundary commits); this module only moves its outputs across
//! threads.

use std::time::Instant;

use ax_edit::TextSnapshot;
use stream::{DictationSession, HorizonConfig, TransportProfile, WriteCommand};
use tokio::sync::mpsc::UnboundedSender;

use crate::ax_stream::AxRegion;

/// Completions from the writer thread, merged into the supervisor's select.
#[derive(Debug)]
pub enum StreamerEvent {
    /// One mid-utterance write finished (or failed). Unlocks the coalescer.
    WriteDone(Result<(), String>),
    /// The final settle (and trailing-space seal) finished. `wrote_any` is
    /// whether ANY text landed in the field this utterance, which decides
    /// whether a failure may still fall back to the buffered insert path.
    Finished {
        result: Result<(), String>,
        wrote_any: bool,
    },
}

enum WriterMsg {
    Apply(WriteCommand),
    Finish(Option<WriteCommand>),
}

/// One utterance's live streaming state, owned by the supervisor.
pub struct Streamer {
    session: DictationSession,
    to_writer: std::sync::mpsc::Sender<WriterMsg>,
    /// Set on the first failed write: committed text stands (never
    /// retracted), but no further streaming writes are attempted; the
    /// final pass reports the failure.
    dead: bool,
}

impl Streamer {
    /// Probe the key-down snapshot and start a streaming session on it.
    ///
    /// `None` means the field cannot take in-place streamed writes (no
    /// caret, refuses range/selection writes, not macOS); the caller keeps
    /// today's buffered commit-on-release path, which is the degradation
    /// the design mandates rather than an error.
    pub fn begin(snap: &TextSnapshot, events: UnboundedSender<StreamerEvent>) -> Option<Streamer> {
        let mut region = AxRegion::begin(snap).ok()?;
        let session = DictationSession::new(
            TransportProfile {
                can_write_in_place: true,
                preserves_undo: true,
            },
            true,
            HorizonConfig::default(),
        );
        let (tx, rx) = std::sync::mpsc::channel::<WriterMsg>();
        std::thread::Builder::new()
            .name("outloud-stream-writer".into())
            .spawn(move || {
                while let Ok(msg) = rx.recv() {
                    match msg {
                        WriterMsg::Apply(cmd) => {
                            let r = region.apply(&cmd);
                            if events.send(StreamerEvent::WriteDone(r)).is_err() {
                                return;
                            }
                        }
                        WriterMsg::Finish(cmd) => {
                            let result = match cmd {
                                Some(c) => region.apply(&c),
                                None => Ok(()),
                            }
                            .and_then(|()| region.seal());
                            let _ = events.send(StreamerEvent::Finished {
                                result,
                                wrote_any: region.wrote_any(),
                            });
                            return; // one utterance, one writer
                        }
                    }
                }
            })
            .ok()?;
        Some(Streamer {
            session,
            to_writer: tx,
            dead: false,
        })
    }

    /// Feed a whole-hypothesis partial. Returns the unstable tail for the
    /// overlay's ghost text (committed text is in the field, not the
    /// overlay).
    pub fn on_partial(&mut self, hypothesis: &str, now: Instant) -> String {
        let update = self.session.on_partial(hypothesis, now);
        self.dispatch(update.write);
        update.overlay.ghost_tail
    }

    /// Flush a parked write whose 80ms interval has elapsed.
    pub fn on_tick(&mut self, now: Instant) {
        let cmd = self.session.on_tick(now);
        self.dispatch(cmd);
    }

    /// When the next parked write becomes due, for the supervisor's sleep.
    pub fn deadline(&self) -> Option<Instant> {
        if self.dead {
            return None;
        }
        self.session.next_deadline()
    }

    /// The writer finished one mid-utterance write.
    pub fn on_write_done(&mut self, result: Result<(), String>, now: Instant) {
        self.session.on_write_done(now);
        if let Err(e) = result {
            // Committed text stands; we just stop adding to it. The final
            // pass will surface the failure with the full transcript.
            eprintln!("outloud: streamed write failed ({e}); holding until the final pass");
            self.dead = true;
        }
    }

    /// End of utterance: send the consolidated final settle to the writer.
    /// The answer arrives as [`StreamerEvent::Finished`].
    pub fn finish(mut self, final_text: &str, now: Instant) {
        let update = self.session.finish(final_text, now);
        let _ = self.to_writer.send(WriterMsg::Finish(update.write));
    }

    fn dispatch(&mut self, cmd: Option<WriteCommand>) {
        if self.dead {
            return;
        }
        if let Some(cmd) = cmd {
            // Send failure means the writer died; treat like a failed write.
            if self.to_writer.send(WriterMsg::Apply(cmd)).is_err() {
                self.dead = true;
            }
        }
    }
}

/// Whether this key-down should even attempt streaming: the setting asks
/// for it, the utterance is a dictation (edits rewrite a selection once, at
/// commit, by design), and the destination does not throw away the writes
/// streaming depends on.
///
/// The app check is the important one and was missing. Streaming revises
/// text in place, which needs either AXValue writes or synthetic keys to
/// stick. Discord discards both: it reconciles its DOM against a model that
/// never saw the events. The commit path already knew this and routed
/// Discord to clipboard paste, but streaming ran BEFORE that decision and
/// silently bypassed it, so whether dictation worked depended on which
/// transport happened to win the race. Observed live: three consecutive
/// utterances into Discord took clipboard-paste, ax-stream and
/// synthetic-keys-paced, and only the first survived.
pub fn wants_streaming(prefer: bool, mode: &crate::inject::Mode, app: Option<&str>) -> bool {
    // BOTH lists, because streaming's transport is an accessibility write.
    //
    // `AxRegion::apply` writes AXSelectedTextRange and AXSelectedText
    // directly (crates/outloud/src/ax_stream.rs), so an app that ignores
    // accessibility writes ignores these too: the revision lands nowhere and
    // the user watches nothing appear while speaking.
    //
    // The narrower `discards_synthetic_typing` list alone was not enough. It
    // holds Discord only, while AX_VALUE_IGNORED_APPS also holds Slack,
    // Notion, Linear, Figma, Signal, Element, Teams, Obsidian and Spotify,
    // every one of which would have streamed into a void. Declining here
    // costs those apps live partials and leaves commit-on-release working,
    // which is the correct trade: no in-place preview beats a preview that
    // silently does not exist.
    if app.is_some_and(|a| {
        text_target::targets::keys::discards_synthetic_typing(a)
            || text_target::targets::keys::ignores_ax_value_writes(a)
    }) {
        return false;
    }
    prefer && matches!(mode, crate::inject::Mode::Dictate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_never_stream() {
        assert!(!wants_streaming(
            true,
            &crate::inject::Mode::Edit {
                selected: "x".into()
            },
            None
        ));
        assert!(wants_streaming(true, &crate::inject::Mode::Dictate, None));
        assert!(!wants_streaming(false, &crate::inject::Mode::Dictate, None));
    }

    /// Apps that discard synthetic writes must never stream, however the
    /// setting is configured.
    ///
    /// Regression, caught by watching a real Discord field across three
    /// dictations: they were served by clipboard-paste, ax-stream and
    /// synthetic-keys-paced respectively, and only clipboard-paste survived.
    /// The commit path already routed Discord correctly; streaming ran first
    /// and bypassed that decision entirely, so "does dictation work" came
    /// down to which transport won a race the user could not see.
    #[test]
    fn apps_that_discard_writes_never_stream() {
        assert!(
            !wants_streaming(true, &crate::inject::Mode::Dictate, Some("Discord")),
            "Discord discards the writes streaming relies on"
        );
        // Case-insensitive, because the AX title's exact casing is not ours
        // to depend on.
        assert!(!wants_streaming(
            true,
            &crate::inject::Mode::Dictate,
            Some("discord")
        ));
        // Apps that accept writes must be unaffected: this is a narrow
        // exclusion, not a reason to disable streaming broadly.
        assert!(wants_streaming(
            true,
            &crate::inject::Mode::Dictate,
            Some("TextEdit")
        ));

        // Streaming writes AXSelectedText, so EVERY app that ignores
        // accessibility writes must decline, not just the one that also
        // discards typing. Otherwise the user speaks and watches nothing
        // appear, because the revisions land nowhere.
        for app in ["Slack", "Notion", "Linear", "Figma", "Signal", "Teams"] {
            assert!(
                !wants_streaming(true, &crate::inject::Mode::Dictate, Some(app)),
                "{app} ignores AX writes, so streaming would show nothing"
            );
        }
    }

    /// Off-macOS (and in CI with no focused field) `begin` must decline,
    /// not panic: declining IS the buffered degradation path.
    #[test]
    fn begin_declines_without_a_streamable_field() {
        let snap = TextSnapshot {
            role: "AXTextArea".into(),
            app: None,
            value: Some("hello".into()),
            selected_text: None,
            selection: None, // no caret -> not streamable
            value_settable: true,
            selected_text_settable: true,
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(Streamer::begin(&snap, tx).is_none());
    }
}
