//! What the key-down path costs BEFORE the microphone is opened.
//!
//! `pipeline.rs` handles KeyDown in this order: AX snapshot -> streamer
//! probe -> `mic.open()`. Everything before `mic.open()` is pure added
//! delay on the capture start, and capture start is already the measured
//! 71ms weak point (docs/input-latency.md). Audio the device never captured
//! cannot be recovered by any downstream buffer, so this delay is charged
//! directly against the user's first syllable.
//!
//! docs/latency.md reports the WARM snapshot (~134-155us). The daemon's
//! first key-down of a session against a given application is COLD, and
//! that is the number that matters here, because it is the one the user
//! feels when they start dictating into a newly focused app.
//!
//! Run with a text field focused:
//!   cargo run --release -p outloud --example keydown_cost

use std::time::Instant;

fn main() {
    println!("cold (first AX contact this process makes):");
    let t = Instant::now();
    let snap = ax_edit::snapshot_focused();
    let cold_us = t.elapsed().as_secs_f64() * 1e6;
    match &snap {
        Ok(s) => println!(
            "  snapshot_focused  {cold_us:8.0} us   role={} app={:?}",
            s.role, s.app
        ),
        Err(e) => {
            println!("  snapshot_focused  {cold_us:8.0} us   FAILED: {e}");
            println!("  (focus a text field, e.g. TextEdit, and rerun)");
        }
    }

    // Warm: the connection to that target process is now established.
    let reps = 200;
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        let _ = ax_edit::snapshot_focused();
        samples.push(t.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
    println!(
        "warm snapshot_focused (n={reps}): p50 {:.0}us  p90 {:.0}us  p99 {:.0}us  max {:.0}us",
        p(0.5),
        p(0.9),
        p(0.99),
        samples[samples.len() - 1]
    );

    // frontmost_app: the extra AX call the buffered commit path makes on
    // top of its own snapshot (inject.rs::stage_terminal_edit runs on EVERY
    // utterance, before any mode decision).
    let mut fa = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        let _ = ax_edit::frontmost_app();
        fa.push(t.elapsed().as_secs_f64() * 1e6);
    }
    fa.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "warm frontmost_app     (n={reps}): p50 {:.0}us  p99 {:.0}us",
        fa[fa.len() / 2],
        fa[(fa.len() as f64 * 0.99) as usize]
    );

    // How many AX round trips does ONE buffered dictation utterance make?
    // key-down snapshot (1) + stage_terminal_edit's frontmost_app (2) +
    // insert_with_fallback's re-snapshot (3) + replace_focused (4).
    println!(
        "\none buffered dictation utterance makes 4 AX conversations:\n\
         \x20 1. key-down snapshot_focused        (pipeline.rs KeyDown)\n\
         \x20 2. frontmost_app                    (inject.rs stage_terminal_edit)\n\
         \x20 3. snapshot_focused AGAIN           (inject.rs insert_with_fallback)\n\
         \x20 4. replace_focused write            (inject.rs write_focused)\n\
         warm cost of the two redundant reads (2+3): {:.0}us",
        fa[fa.len() / 2] + p(0.5)
    );
}
