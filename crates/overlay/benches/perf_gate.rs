//! Regression gate for the pure hot paths, runnable anywhere.
//!
//! WHY a second gate when `gate.rs` already exists: that one measures the
//! real accessibility path, which needs a focused text field, a granted
//! permission and a window server. On a CI runner it correctly skips, so it
//! protects nothing there. A gate that only runs on the maintainer's laptop
//! is a gate that catches a regression after it has already shipped.
//!
//! This one measures only pure computation, so it produces the same answer
//! on a headless Linux container as on an M4 laptop, and it fails the build
//! rather than skipping.
//!
//! WHAT it protects. Every stage here sits between the user's voice and
//! their text, and a slowdown in any of them is invisible to a correctness
//! test: the app still produces the right characters, just late. That is
//! precisely the failure mode ordinary CI cannot see.
//!
//!   overlay step   runs up to 120 times a second while dictating. Today it
//!                  costs ~1ms of an 8.33ms frame; if it reached 8ms the
//!                  overlay would visibly stutter and every test would
//!                  still pass.
//!   overlay ingest runs on every recognizer partial, which arrive in
//!                  bursts of four or five words (docs/partial-timing.md).
//!   intent parse   runs once per utterance, measured in microseconds. It
//!                  is here because an accidental quadratic in the parser
//!                  would show up on long dictations only.
//!
//! Budgets sit roughly an order of magnitude above measured cost. Measured
//! on an M4 Pro across three runs, the numbers were stable to within a few
//! percent:
//!
//!   overlay::step        51-63ns    budget 1us    (~16x headroom)
//!   overlay::ingest      2.15-2.2us budget 25us   (~11x headroom)
//!   edit_intent::parse   1.8-1.9us  budget 20us   (~10x headroom)
//!
//! The headroom is for the runner, not for the code: a shared CI machine
//! can be several times slower than a laptop, and a gate that fails on a
//! noisy neighbour trains people to ignore it. What remains is still tight
//! enough to catch the regressions that matter, since the realistic failure
//! is not "20% slower" but "someone added an allocation to a 120Hz loop"
//! or "someone made a linear scan quadratic", and both of those are orders
//! of magnitude, not percentages.

use std::hint::black_box;
use std::time::{Duration, Instant};

use overlay::layout::RollingWindow;

/// Iterations per measurement. Enough to swamp timer granularity without
/// making the gate a meaningful part of CI's runtime.
const ITERS: usize = 2_000;

/// A realistic dictation: the burst sizes match what SpeechTranscriber
/// actually emits, so the measurement reflects real use rather than a
/// synthetic worst case.
const HYPOTHESES: &[&str] = &[
    "The",
    "The dog",
    "The dog is brown",
    "The dog is brown and has a lot",
    "The dog is brown and has a lot of fun running",
    "The dog is brown and has a lot of fun running through the yard",
    "The dog is brown and has a lot of fun running through the yard every single morning",
];

fn main() {
    let mut failures = Vec::new();

    check(
        "overlay::step",
        Duration::from_micros(1),
        &mut failures,
        bench_step,
    );
    check(
        "overlay::ingest",
        Duration::from_micros(25),
        &mut failures,
        bench_ingest,
    );
    check(
        "edit_intent::parse",
        Duration::from_micros(20),
        &mut failures,
        bench_parse,
    );

    if failures.is_empty() {
        println!("\nperf gate OK");
        return;
    }
    eprintln!("\nperf gate FAILED:");
    for f in &failures {
        eprintln!("  {f}");
    }
    eprintln!(
        "\nA correctness test cannot see this: the app still produces the\n\
         right text, only slower. If the change is deliberate, raise the\n\
         budget in this file and say why in the commit."
    );
    std::process::exit(1);
}

/// Run one measurement and record it against its budget.
fn check(name: &str, budget: Duration, failures: &mut Vec<String>, body: impl Fn() -> Duration) {
    // Warm first: the first iteration pays for lazy allocation and cold
    // caches, and charging that to the measurement would make the gate
    // depend on how the runner scheduled us.
    let _ = body();
    let per_op = body();

    let verdict = if per_op <= budget { "ok" } else { "OVER" };
    println!("{name:<22} {per_op:>10.2?} / {budget:?}  {verdict}");
    if per_op > budget {
        failures.push(format!(
            "{name} took {per_op:.2?} per operation, budget {budget:?}"
        ));
    }
}

/// One animation frame of the rolling text window.
fn bench_step() -> Duration {
    let mut w = RollingWindow::new();
    let mut measure = |s: &str| s.len() as f64 * 8.0;
    w.ingest(HYPOTHESES[HYPOTHESES.len() - 1], 0.0, &mut measure);

    let dt = 1.0 / 120.0;
    let start = Instant::now();
    for i in 0..ITERS {
        black_box(w.step(i as f64 * dt, dt, false));
    }
    start.elapsed() / ITERS as u32
}

/// One recognizer partial arriving, including the revision path.
fn bench_ingest() -> Duration {
    let mut measure = |s: &str| s.len() as f64 * 8.0;
    let start = Instant::now();
    for i in 0..ITERS {
        // A fresh window each pass: ingest is idempotent per hypothesis, so
        // reusing one would measure the early-return and prove nothing.
        let mut w = RollingWindow::new();
        for (n, h) in HYPOTHESES.iter().enumerate() {
            w.ingest(h, (i * HYPOTHESES.len() + n) as f64 * 0.1, &mut measure);
        }
        black_box(w.words().len());
    }
    start.elapsed() / ITERS as u32
}

/// Parsing a spoken command, both the matching and freeform paths.
fn bench_parse() -> Duration {
    let commands = [
        "change quick to slow",
        "delete the last sentence",
        "make it all caps",
        "this is just an ordinary sentence with no command in it at all",
    ];
    let start = Instant::now();
    for _ in 0..ITERS {
        for c in &commands {
            black_box(edit_intent::parse(black_box(c)));
        }
    }
    start.elapsed() / ITERS as u32
}
