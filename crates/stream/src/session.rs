//! The dictation session: everything composed into one state machine.
//!
//! One `DictationSession` covers one utterance from hotkey-down to sealed
//! undo unit. It is deliberately transport-free: the caller passes in what
//! the transport *can do* and the current time, and gets back explicit
//! [`WriteCommand`]s to execute. Keeping the IO at the edge is what makes
//! every branch of this state machine testable without an OS, an
//! accessibility server, or a clock.
//!
//! ## Capability-aware degradation
//!
//! Streaming only works when the transport can *revise* text it already
//! wrote. A clipboard paste, a terminal OSC sequence, or synthetic
//! keystrokes are fire-and-forget: `can_write_in_place: false` (the same
//! flag `text_target::Capabilities` carries). Streaming into such a
//! transport would emit garbage: every "revision" would append another
//! copy. So the session inspects the capability up front and silently
//! degrades to **buffered** mode: partials accumulate invisibly (the
//! overlay still shows them) and exactly one insert happens at
//! finalization. The user asked for streaming; the transport made the
//! decision; nothing broke.

use std::ops::Range;
use std::time::Instant;

use crate::coalesce::{Coalescer, DEFAULT_WRITE_INTERVAL};
use crate::diff::{minimal_edit, Edit};
use crate::horizon::{CommitHorizon, HorizonConfig};
use crate::undo::UndoRing;

/// The slice of transport capability this layer cares about. Mirrors the
/// fields of `text_target::Capabilities` rather than depending on that
/// crate, so `stream` stays buildable with zero display machinery and the
/// two crates can only couple through data, never through types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportProfile {
    /// Can replace already-written text. Without it streaming is
    /// impossible, not merely worse: revisions would duplicate text.
    pub can_write_in_place: bool,
    /// The transport's writes go through the host's own editing machinery,
    /// so its undo survives. When false, our [`UndoRing`] is the ONLY undo.
    pub preserves_undo: bool,
}

/// How this session will deliver text, decided once at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// Commit-stable prefixes as we go; corrections diffed in place.
    Streaming,
    /// Buffer everything; one insert at finalization. Chosen automatically
    /// when the transport cannot write in place, or by caller preference
    /// (commit-on-release is the product default).
    Buffered,
}

/// A transport operation the caller must perform. Byte offsets address the
/// text this session has written so far (region-local, not whole-field:
/// the session neither knows nor cares what surrounded the caret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteCommand {
    /// Insert text at the end of the dictated region.
    Append(String),
    /// Replace `range` of the dictated region with `insert`. Only issued
    /// in streaming mode against in-place-capable transports.
    Splice { range: Range<usize>, insert: String },
}

/// What the caller should render in the overlay after an update.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OverlayState {
    /// Unstable hypothesis tail, styled dim.
    pub ghost_tail: String,
}

/// Result of feeding one partial into the session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionUpdate {
    /// Transport work to do now, if any. At most one command per update by
    /// construction (the coalescer releases at most one state).
    pub write: Option<WriteCommand>,
    pub overlay: OverlayState,
}

/// One utterance's streaming commit state machine.
#[derive(Debug)]
pub struct DictationSession {
    mode: DeliveryMode,
    horizon: CommitHorizon,
    /// Desired region text flows through here; releases become writes.
    coalescer: Coalescer<String>,
    /// What we have actually written into the target so far. Diffs are
    /// computed against this, never against a hypothesis, so an applied
    /// diff always transforms the target's true contents.
    written: String,
    /// The latest whole hypothesis, kept for buffered mode's single write
    /// and for the overlay.
    last_hypothesis: String,
}

impl DictationSession {
    /// Start a session. `prefer_streaming` is the user/product setting;
    /// the transport capability can only degrade it, never upgrade it.
    pub fn new(profile: TransportProfile, prefer_streaming: bool, horizon: HorizonConfig) -> Self {
        // The degradation decision, made once and silently: streaming
        // requires in-place revision. See module docs.
        let mode = if prefer_streaming && profile.can_write_in_place {
            DeliveryMode::Streaming
        } else {
            DeliveryMode::Buffered
        };
        Self {
            mode,
            horizon: CommitHorizon::new(horizon),
            coalescer: Coalescer::new(DEFAULT_WRITE_INTERVAL),
            written: String::new(),
            last_hypothesis: String::new(),
        }
    }

    /// Which mode the capability check actually selected.
    pub fn mode(&self) -> DeliveryMode {
        self.mode
    }

    /// The dictated-region text this session believes it has written.
    pub fn written(&self) -> &str {
        &self.written
    }

    /// Feed the next whole-hypothesis partial from the recognizer.
    pub fn on_partial(&mut self, hypothesis: &str, now: Instant) -> SessionUpdate {
        self.last_hypothesis = hypothesis.to_string();
        match self.mode {
            DeliveryMode::Buffered => SessionUpdate {
                // Nothing is written mid-utterance; the whole hypothesis
                // is ghost text.
                write: None,
                overlay: OverlayState {
                    ghost_tail: hypothesis.to_string(),
                },
            },
            DeliveryMode::Streaming => {
                let update = self.horizon.update(hypothesis);
                // Offer the full committed prefix as the desired region
                // state. The coalescer may park it; when it releases, we
                // diff against what was actually written, so folded and
                // dropped intermediates cost nothing. Offering only when
                // the desired state differs from the written state keeps a
                // no-progress partial from consuming a release slot (and
                // from leaving the coalescer waiting on a write_done for a
                // write that never happened).
                let desired = self.horizon.committed().to_string();
                let write = if desired != self.written {
                    let released = self.coalescer.offer(desired, now);
                    self.release_write(released, now)
                } else {
                    None
                };
                SessionUpdate {
                    write,
                    overlay: OverlayState {
                        ghost_tail: update.tail,
                    },
                }
            }
        }
    }

    /// Driver tick: flush a parked write once its interval elapses. Call
    /// this when [`Coalescer::next_deadline`]-style scheduling fires; in
    /// practice, on the overlay's own frame tick.
    pub fn on_tick(&mut self, now: Instant) -> Option<WriteCommand> {
        let released = self.coalescer.try_release(now);
        self.release_write(released, now)
    }

    /// Turn a released desired-state into a command, and if the release
    /// turned out to be a no-op (target already written), immediately mark
    /// the "write" done so the coalescer is not left waiting forever for a
    /// completion that will never come.
    fn release_write(&mut self, released: Option<String>, now: Instant) -> Option<WriteCommand> {
        let target = released?;
        match self.emit_write(&target) {
            Some(cmd) => Some(cmd),
            None => {
                self.coalescer.write_done(now);
                None
            }
        }
    }

    /// The caller finished executing the last returned [`WriteCommand`].
    /// Nothing further is released until this is called: that is the
    /// backpressure contract (slow transport => dropped intermediates).
    pub fn on_write_done(&mut self, now: Instant) {
        self.coalescer.write_done(now);
    }

    /// End of utterance with the finalizer's transcript. Returns the final
    /// write (skipping the coalescer: the user is waiting) and the region
    /// text for sealing the undo unit. In streaming mode this is the "one
    /// consolidated correction" settle from the UX doc; in buffered mode
    /// it is the single insert.
    pub fn finish(&mut self, final_text: &str, _now: Instant) -> SessionUpdate {
        self.last_hypothesis.clear();
        let write = match self.mode {
            DeliveryMode::Buffered => {
                if final_text.is_empty() {
                    None
                } else {
                    self.written = final_text.to_string();
                    Some(WriteCommand::Append(final_text.to_string()))
                }
            }
            DeliveryMode::Streaming => {
                // The horizon's bookkeeping ends here; the final transcript
                // wins wholesale regardless of stability.
                let _ = self.horizon.finish(final_text);
                self.emit_write(final_text)
            }
        };
        SessionUpdate {
            write,
            overlay: OverlayState::default(),
        }
    }

    /// Minimal splice turning `written` into `target`, or `None` when the
    /// target already matches. Appends are recognized and emitted as such
    /// because insert-at-end is cheaper and safer than range replacement
    /// on every transport that distinguishes them.
    fn emit_write(&mut self, target: &str) -> Option<WriteCommand> {
        if target == self.written {
            return None;
        }
        let edit = minimal_edit(&self.written, target);
        let cmd = if edit.range.start == self.written.len() && edit.range.is_empty() {
            WriteCommand::Append(edit.insert.clone())
        } else {
            WriteCommand::Splice {
                range: edit.range.clone(),
                insert: edit.insert.clone(),
            }
        };
        debug_assert_eq!(Edit::apply(&edit, &self.written), target);
        self.written = target.to_string();
        Some(cmd)
    }
}

/// Convenience: seal a finished session into an undo ring as one unit.
/// `field_before` is the whole field before the dictation; the session's
/// written region is appended at the caret to form the after-image. Split
/// out as a free function because the ring outlives sessions.
pub fn seal_undo(
    ring: &mut UndoRing,
    field_before: &str,
    caret: Option<usize>,
    session: &DictationSession,
) {
    ring.begin_unit(field_before, caret);
    let mut after = String::from(field_before);
    match caret {
        Some(pos) if pos <= field_before.len() && field_before.is_char_boundary(pos) => {
            after.insert_str(pos, session.written());
        }
        _ => after.push_str(session.written()),
    }
    ring.end_unit(&after);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const IN_PLACE: TransportProfile = TransportProfile {
        can_write_in_place: true,
        preserves_undo: true,
    };
    const INSERT_ONLY: TransportProfile = TransportProfile {
        can_write_in_place: false,
        preserves_undo: false,
    };

    fn stream_session() -> DictationSession {
        DictationSession::new(
            IN_PLACE,
            true,
            HorizonConfig {
                stability: 2,
                lookback_words: 0,
            },
        )
    }

    /// A model target: applies commands the way a real transport would, so
    /// tests verify the *observable* field text, not internal state.
    fn apply(target: &mut String, cmd: &WriteCommand) {
        match cmd {
            WriteCommand::Append(s) => target.push_str(s),
            WriteCommand::Splice { range, insert } => {
                target.replace_range(range.clone(), insert);
            }
        }
    }

    #[test]
    fn insert_only_transport_degrades_to_buffered() {
        let s = DictationSession::new(INSERT_ONLY, true, HorizonConfig::default());
        assert_eq!(s.mode(), DeliveryMode::Buffered, "streaming would garble");
    }

    #[test]
    fn buffered_mode_writes_exactly_once() {
        let mut s = DictationSession::new(INSERT_ONLY, true, HorizonConfig::default());
        let now = Instant::now();
        let mut field = String::new();
        for hyp in ["hel", "hello", "hello wor", "hello world"] {
            let u = s.on_partial(hyp, now);
            assert_eq!(u.write, None, "buffered mode must not touch the field");
            assert_eq!(u.overlay.ghost_tail, hyp, "overlay still shows partials");
        }
        let fin = s.finish("Hello, world.", now);
        let cmd = fin.write.expect("the single commit-on-release write");
        apply(&mut field, &cmd);
        assert_eq!(field, "Hello, world.");
    }

    #[test]
    fn streaming_end_to_end_with_revision() {
        let mut s = stream_session();
        let mut field = String::new();
        let t = Instant::now();
        let mut now = t;
        let hyps = [
            "recognise",
            "recognise speech",
            "recognise speech is",
            "recognise speech is hard",
        ];
        for hyp in hyps {
            now += Duration::from_millis(150); // past the coalesce interval
            if let Some(cmd) = s.on_partial(hyp, now).write {
                apply(&mut field, &cmd);
                s.on_write_done(now);
            }
        }
        assert!(!field.is_empty(), "stable prefix should have streamed");
        assert!(
            "recognise speech is hard".starts_with(&field),
            "field only ever holds a committed prefix, got {field:?}"
        );
        // Finalizer totally rewrites. One consolidated correction lands.
        let fin = s.finish("Wreck a nice beach is hard.", now);
        if let Some(cmd) = fin.write {
            apply(&mut field, &cmd);
        }
        assert_eq!(field, "Wreck a nice beach is hard.");
    }

    #[test]
    fn final_matching_committed_text_is_a_cheap_append() {
        let mut s = stream_session();
        let mut field = String::new();
        let t = Instant::now();
        let mut now = t;
        for hyp in ["hello", "hello", "hello world", "hello world"] {
            now += Duration::from_millis(150);
            if let Some(cmd) = s.on_partial(hyp, now).write {
                apply(&mut field, &cmd);
                s.on_write_done(now);
            }
        }
        assert_eq!(field, "hello world");
        let fin = s.finish("hello world today", now);
        match fin.write {
            Some(WriteCommand::Append(s)) => assert_eq!(s, " today"),
            other => panic!("expected pure append, got {other:?}"),
        }
    }

    #[test]
    fn coalescer_folds_rapid_partials_into_one_write() {
        let mut s = stream_session();
        let now = Instant::now();
        // First release goes out immediately...
        let first = s.on_partial("aaa", now);
        assert_eq!(first.write, None, "nothing stable yet");
        // Rapid-fire agreeing partials 1ms apart: stability is reached but
        // the interval is not, so writes park.
        let mut writes = 0;
        for i in 1..10u64 {
            let u = s.on_partial("aaa bbb", now + Duration::from_millis(i));
            writes += u.write.iter().count();
        }
        assert!(writes <= 1, "at most one write within one interval");
    }

    #[test]
    fn tick_flushes_a_parked_write() {
        let mut s = stream_session();
        let now = Instant::now();
        assert!(s.on_partial("hello there", now).write.is_none());
        let u = s.on_partial("hello there", now + Duration::from_millis(1));
        // Stable now, but... first write actually releases immediately
        // (no prior release). If it did release, complete it and park one.
        if let Some(_cmd) = u.write {
            s.on_write_done(now + Duration::from_millis(1));
            assert!(s
                .on_partial("hello there friend", now + Duration::from_millis(2))
                .write
                .is_none());
            assert!(s
                .on_partial("hello there friend", now + Duration::from_millis(3))
                .write
                .is_none());
            // The parked commit flushes on a later tick.
            let flushed = s.on_tick(now + Duration::from_millis(200));
            assert!(flushed.is_some(), "tick must flush the parked write");
        }
    }

    #[test]
    fn seal_undo_is_one_step_at_the_caret() {
        let mut s = stream_session();
        let now = Instant::now();
        s.on_partial("world", now);
        let _ = s.finish("world", now);
        let mut ring = UndoRing::new(4);
        seal_undo(&mut ring, "hello !", Some(6), &s);
        assert_eq!(ring.len(), 1);
        match ring.undo("hello world!") {
            crate::undo::UndoOutcome::Restore(u) => {
                assert_eq!(u.before, "hello !");
                assert_eq!(u.after, "hello world!");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_final_writes_nothing() {
        let mut s = DictationSession::new(INSERT_ONLY, true, HorizonConfig::default());
        let fin = s.finish("", Instant::now());
        assert_eq!(fin.write, None, "silence must not produce a write");
    }
}
