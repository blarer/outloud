//! Property tests for the streaming commit layer's core invariants.
//!
//! These are the guarantees the whole product's streaming UX rests on, so
//! they are checked against generated adversarial inputs, not just the
//! examples in the unit tests:
//!
//! 1. `minimal_edit(old, new).apply(old) == new` for arbitrary Unicode.
//! 2. Committed text is never retracted: across any hypothesis sequence,
//!    each committed string is a prefix-extension of the previous one.
//! 3. A streaming session's simulated target field always holds a prefix
//!    of some committed state, and finishes exactly equal to the final
//!    transcript.
//! 4. Coalescer releases never violate the interval, regardless of offer
//!    timing.

use proptest::prelude::*;
use std::time::{Duration, Instant};
use stream::{
    minimal_edit, Coalescer, CommitHorizon, DictationSession, HorizonConfig, TransportProfile,
    WriteCommand,
};

/// Text strategy that leans adversarial: ASCII words, CJK (no spaces),
/// emoji ZWJ sequences, combining marks, and mixtures.
fn adversarial_text() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        "[a-z]{1,8}( [a-z]{1,8}){0,5}",
        "[\\u{4e00}-\\u{4e2f}]{1,10}",        // Han, no spaces
        Just("👩‍👩‍👧‍👦".to_string()),               // ZWJ family
        Just("🇯🇵".to_string()),               // flag (regional pair)
        Just("e\u{301}e\u{301}".to_string()), // combining acute
        Just("नमस्ते".to_string()),             // Devanagari clusters
        Just(String::new()),
    ];
    proptest::collection::vec(piece, 0..4).prop_map(|v| v.concat())
}

/// A hypothesis sequence shaped like a real recognizer: mostly growth,
/// with revisions of the recent tail and the occasional total rewrite.
fn hypothesis_sequence() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(adversarial_text(), 1..12)
}

fn apply(target: &mut String, cmd: &WriteCommand) {
    match cmd {
        WriteCommand::Append(s) => target.push_str(s),
        WriteCommand::Splice { range, insert } => target.replace_range(range.clone(), insert),
    }
}

proptest! {
    /// Invariant 1: the diff, applied to the old state, always yields the
    /// new state, for any Unicode pair, and its range is sliceable.
    #[test]
    fn diff_roundtrips(old in adversarial_text(), new in adversarial_text()) {
        let e = minimal_edit(&old, &new);
        // The range must lie on char boundaries or apply() would panic,
        // so apply() succeeding is itself part of the assertion.
        prop_assert_eq!(e.apply(&old), new);
    }

    /// Invariant 2: whatever the recognizer does, including total
    /// rewrites, the committed string only ever grows by extension.
    #[test]
    fn committed_text_is_never_retracted(
        hyps in hypothesis_sequence(),
        stability in 1usize..4,
        lookback in 0usize..3,
    ) {
        let mut h = CommitHorizon::new(HorizonConfig { stability, lookback_words: lookback });
        let mut prev = String::new();
        for hyp in &hyps {
            let _ = h.update(hyp);
            let cur = h.committed().to_string();
            prop_assert!(
                cur.starts_with(&prev),
                "committed text retracted: {:?} -> {:?}", prev, cur
            );
            prev = cur;
        }
    }

    /// Invariant 3: driving a full streaming session, the simulated field
    /// never contains anything but the currently committed prefix, and
    /// the final settle makes it exactly the final transcript.
    #[test]
    fn session_field_converges_to_final(
        hyps in hypothesis_sequence(),
        final_text in adversarial_text(),
    ) {
        let mut s = DictationSession::new(
            TransportProfile { can_write_in_place: true, preserves_undo: true },
            true,
            HorizonConfig { stability: 2, lookback_words: 1 },
        );
        let mut field = String::new();
        let start = Instant::now();
        let mut now = start;
        for hyp in &hyps {
            now += Duration::from_millis(150); // always past the interval
            let u = s.on_partial(hyp, now);
            if let Some(cmd) = u.write {
                apply(&mut field, &cmd);
                s.on_write_done(now);
                // The field must exactly track the session's committed view;
                // divergence here is the flicker/garbage failure mode.
                prop_assert_eq!(&field, s.written());
            }
        }
        now += Duration::from_millis(150);
        if let Some(cmd) = s.finish(&final_text, now).write {
            apply(&mut field, &cmd);
        }
        prop_assert_eq!(field, final_text);
    }

    /// Invariant 4: releases respect the interval for arbitrary offer
    /// timings, including bursts far faster than the interval.
    #[test]
    fn coalescer_respects_interval(gaps in proptest::collection::vec(0u64..200, 1..40)) {
        let interval = Duration::from_millis(80);
        let mut c = Coalescer::new(interval);
        let start = Instant::now();
        let mut now = start;
        let mut releases: Vec<Instant> = Vec::new();
        for (i, gap) in gaps.iter().enumerate() {
            now += Duration::from_millis(*gap);
            if let Some(_v) = c.offer(i, now) {
                releases.push(now);
                c.write_done(now);
            }
        }
        for pair in releases.windows(2) {
            prop_assert!(pair[1] - pair[0] >= interval);
        }
    }

    /// Buffered degradation: an insert-only transport sees EXACTLY one
    /// write no matter what the partial stream does.
    #[test]
    fn insert_only_transport_gets_one_write(
        hyps in hypothesis_sequence(),
        final_text in adversarial_text(),
    ) {
        let mut s = DictationSession::new(
            TransportProfile { can_write_in_place: false, preserves_undo: false },
            true, // caller asked for streaming; capability must override
            HorizonConfig::default(),
        );
        let now = Instant::now();
        let mut writes = 0;
        for hyp in &hyps {
            writes += s.on_partial(hyp, now).write.iter().count();
        }
        let fin = s.finish(&final_text, now);
        writes += fin.write.iter().count();
        let expected = usize::from(!final_text.is_empty());
        prop_assert_eq!(writes, expected, "buffered mode must write once (or zero on silence)");
        if let Some(WriteCommand::Append(text)) = fin.write {
            prop_assert_eq!(text, final_text);
        }
    }
}
