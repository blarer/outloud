//! Why `inject_ms` is 16ms in one log and 57ms in another: the transport.
//!
//! docs/beta-readiness.md's runs all landed via `synthetic-keys`, with
//! inject 33-57ms. docs/macos-quickstart.md's landed via `set-value`, with
//! inject 16.5ms. Those are different code paths with different cost laws,
//! and the reported spread is mostly WHICH ONE RAN, not variance within one.
//!
//! The paced path (`ax_edit::synth::type_text`) posts two CGEvents per
//! CHARACTER and spins `KEY_INTERVAL` (700us) between them, so its cost is
//! linear in transcript length with a large constant. This measures that
//! law without posting a single event, by timing the spin loop the path is
//! built on -- so it is safe to run while the user is working.
//!
//! Run: cargo run --release -p ax-edit --example typing_cost_model

use std::time::{Duration, Instant};

/// Same primitive `ax_edit::synth` uses between characters.
fn spin_for(d: Duration) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

fn main() {
    // Measure the real cost of one KEY_INTERVAL spin (700us nominal).
    let reps = 500;
    let t = Instant::now();
    for _ in 0..reps {
        spin_for(Duration::from_micros(700));
    }
    let per_char_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;
    println!("measured spin per character: {per_char_us:.0} us (nominal 700)");

    println!("\npaced synthetic-keys cost by transcript length:");
    println!("  {:>6}  {:>12}", "chars", "spin-only ms");
    for n in [10usize, 20, 44, 60, 80, 100] {
        println!("  {n:>6}  {:>12.1}", n as f64 * per_char_us / 1000.0);
    }
    println!(
        "\n(spin only: excludes 2 CGEventPost syscalls per character, so the\n\
         real path is strictly slower than these numbers.)"
    );
    println!(
        "\nFor comparison, the batched path posts ceil(len/20) events with no\n\
         spin at all, and the AX set-value path is ONE round trip (~265us,\n\
         docs/latency.md). Length-independence is the whole difference."
    );

    // Batched: chunk count for the same lengths.
    println!("\nbatched synthetic-keys event count (20 UTF-16 units/event):");
    for n in [10usize, 44, 100] {
        println!("  {n:>6} chars -> {} events", n.div_ceil(20));
    }
}
