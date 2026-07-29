//! The stale-prefill artifact, as a test, so the measurement that found it
//! cannot quietly regress into the wrong answer again.
//!
//! `examples/device_latency.rs` decides when an input device really started
//! hearing the room by looking for the reference tone in the captured buffers.
//! The obvious implementation, "report the first buffer above threshold", is
//! wrong on real hardware, and wrong in the flattering direction.
//!
//! Measured on this machine's built-in microphone (macOS 26, cpal 0.18): after
//! a cold open, the *first* buffer routinely contains audio from before the
//! open. It was proven stale rather than merely early by muting the reference
//! tone for 2.5 seconds and then opening the stream, at which point buffer 1
//! still arrived full of tone, in 14 of 15 runs. After that one buffer the
//! device goes quiet for ~150ms and only then starts delivering live audio.
//!
//! So a first-hit detector reports ~60ms on a device whose true figure is
//! ~233ms: it stops its clock on evidence of a moment that had already passed.
//! That is precisely the class of error this whole investigation exists to
//! remove, so the rule that avoids it (require a sustained run of buffers, not
//! one) is pinned here against synthetic captures shaped like the real ones.
//!
//! These tests need no hardware: they encode the *shape* the hardware produced.

/// Buffers that must all contain the tone before it counts as live. Mirrors
/// `SUSTAIN_BUFFERS` in the example. Kept in sync by the assertion in
/// `the_example_and_this_test_agree_on_the_sustain_window` below, which reads
/// the example's source rather than trusting a comment.
const SUSTAIN: usize = 5;

/// The detector under test, in the same form the example uses: the arrival time
/// of the first buffer that begins a run of `SUSTAIN` consecutive buffers all
/// above the threshold.
fn sustained_from(buffers: &[(f64, f32)], threshold: f32) -> Option<f64> {
    buffers
        .windows(SUSTAIN)
        .find(|w| w.iter().all(|(_, m)| *m > threshold))
        .map(|w| w[0].0)
}

/// The naive detector, kept so the tests can state what it would have said.
/// Its answers are the bug, in numbers.
fn first_hit(buffers: &[(f64, f32)], threshold: f32) -> Option<f64> {
    buffers
        .iter()
        .find(|(_, m)| *m > threshold)
        .map(|(t, _)| *t)
}

/// One buffer every 10.7ms, the cadence the built-in microphone delivered at
/// 48kHz with a 512-frame buffer.
const PERIOD_MS: f64 = 10.7;

fn at(index: usize) -> f64 {
    60.0 + index as f64 * PERIOD_MS
}

/// The measured shape: one stale buffer holding the tone, a dead notch, then
/// sustained live audio.
///
/// Levels are the real ones: ~0.004 with the tone, ~0.00002 without.
fn stale_prefill_capture() -> Vec<(f64, f32)> {
    let mut out = vec![(at(0), 0.0042)]; // stale: pre-open audio
    for i in 1..16 {
        out.push((at(i), 0.00002)); // the notch: device not yet capturing
    }
    for i in 16..40 {
        out.push((at(i), 0.0041)); // live at last
    }
    out
}

const THRESHOLD: f32 = 0.0003;

#[test]
fn a_stale_first_buffer_does_not_count_as_the_device_hearing_you() {
    let capture = stale_prefill_capture();

    // What the naive detector says, and why it is not merely imprecise: it
    // points at the very first buffer, whose contents predate the open.
    assert_eq!(
        first_hit(&capture, THRESHOLD),
        Some(at(0)),
        "precondition: the naive detector really is fooled by this shape"
    );

    let sustained = sustained_from(&capture, THRESHOLD).expect("live audio does arrive");
    assert_eq!(
        sustained,
        at(16),
        "the sustained detector must skip the stale buffer and report the live run"
    );

    // The gap is the whole point, and it is large enough to swallow a word.
    let error = sustained - at(0);
    assert!(
        error > 150.0,
        "the naive detector under-reports by {error:.0}ms, which is the bug this \
         rule exists to prevent; if this shrinks, re-derive it from hardware \
         rather than relaxing the assertion"
    );
}

#[test]
fn a_device_that_is_hearing_immediately_is_not_penalised() {
    // The rule must not make a genuinely fast device look slow: that would
    // trade a false alarm for a false reassurance, which is no better.
    let capture: Vec<(f64, f32)> = (0..40).map(|i| (at(i), 0.0041)).collect();
    assert_eq!(
        sustained_from(&capture, THRESHOLD),
        Some(at(0)),
        "a device hearing from buffer 1 onward must report buffer 1"
    );
}

#[test]
fn a_single_glitch_buffer_mid_silence_is_ignored() {
    // Not the same shape as the prefill (the blip is in the middle), and it
    // must also not be believed. A lone buffer is never evidence.
    let mut capture: Vec<(f64, f32)> = (0..40).map(|i| (at(i), 0.00002)).collect();
    capture[7].1 = 0.0044;
    assert_eq!(
        sustained_from(&capture, THRESHOLD),
        None,
        "one hot buffer in continuous silence must not be read as live audio"
    );
    assert_eq!(
        first_hit(&capture, THRESHOLD),
        Some(at(7)),
        "precondition: the naive detector would have believed the glitch"
    );
}

#[test]
fn a_device_that_never_hears_reports_nothing_rather_than_guessing() {
    let capture: Vec<(f64, f32)> = (0..40).map(|i| (at(i), 0.00002)).collect();
    assert_eq!(
        sustained_from(&capture, THRESHOLD),
        None,
        "silence must produce no answer at all, not an optimistic one"
    );
}

#[test]
fn a_run_one_buffer_short_of_the_window_is_not_enough() {
    // The boundary, stated explicitly: SUSTAIN-1 consecutive hot buffers is
    // still not evidence, SUSTAIN is. Without this, the window could be
    // weakened to 1 and every other test here would still pass.
    let short: Vec<(f64, f32)> = (0..40)
        .map(|i| {
            (
                at(i),
                if (5..5 + SUSTAIN - 1).contains(&i) {
                    0.0041
                } else {
                    0.00002
                },
            )
        })
        .collect();
    assert_eq!(sustained_from(&short, THRESHOLD), None);

    let exact: Vec<(f64, f32)> = (0..40)
        .map(|i| {
            (
                at(i),
                if (5..5 + SUSTAIN).contains(&i) {
                    0.0041
                } else {
                    0.00002
                },
            )
        })
        .collect();
    assert_eq!(sustained_from(&exact, THRESHOLD), Some(at(5)));
}

/// The constant above is a copy of one in the example, and a copy that can
/// drift is a lie waiting to happen. This reads the example's source and
/// checks the two agree, the same way `noise_floor.rs` derives its ceiling
/// instead of restating it.
#[test]
fn the_example_and_this_test_agree_on_the_sustain_window() {
    let src = include_str!("../examples/device_latency.rs");
    let line = src
        .lines()
        .find(|l| l.trim_start().starts_with("const SUSTAIN_BUFFERS"))
        .expect("device_latency.rs must define SUSTAIN_BUFFERS");
    let value: usize = line
        .rsplit_once('=')
        .and_then(|(_, rhs)| rhs.trim().trim_end_matches(';').parse().ok())
        .expect("SUSTAIN_BUFFERS must be a plain integer literal");
    assert_eq!(
        value, SUSTAIN,
        "device_latency.rs changed its sustain window to {value} but this test \
         still encodes {SUSTAIN}; update both, and re-check the artifact still \
         does not survive the new window"
    );
}
