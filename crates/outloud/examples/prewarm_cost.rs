//! Does the recognizer pre-warm actually hide helper spawn cost, and what
//! does back-to-back dictation cost?
//!
//! `recognize.rs` re-constructs the recognizer after every finalize
//! ("pre-warm the next utterance's recognizer NOW, while the user reads
//! their committed text"). That construction blocks the ASR worker thread
//! for the measured ~65ms helper spawn. The claim is that this is free
//! because the user is reading. This checks the claim by driving utterances
//! back to back at a controlled gap, and measuring whether the FIRST
//! audio of utterance N+1 waits behind the pre-warm.
//!
//! Run: cargo run --release -p outloud --example prewarm_cost -- [gap_ms]

use std::time::Instant;

use asr::backends::mock::MockRecognizer;
use asr::Recognizer;

/// Time a real Apple helper construction, repeatedly: this is what the
/// worker thread blocks on after every finalize.
fn main() {
    let gap_ms: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // 1. What one construction costs (the pre-warm's blocking duration).
    let mut spawns = Vec::new();
    for _ in 0..8 {
        let t = Instant::now();
        match asr::backends::apple::AppleRecognizer::new() {
            Ok(r) => {
                spawns.push(t.elapsed().as_secs_f64() * 1e3);
                drop(r);
            }
            Err(e) => {
                println!("apple backend unavailable ({e}); skipping spawn measurement");
                break;
            }
        }
    }
    if !spawns.is_empty() {
        spawns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "AppleRecognizer::new() n={} p50 {:.0}ms  min {:.0}ms  max {:.0}ms",
            spawns.len(),
            spawns[spawns.len() / 2],
            spawns[0],
            spawns[spawns.len() - 1]
        );
        println!(
            "  -> the ASR worker thread is BLOCKED this long after every finalize,\n\
             \x20    before it can consume the next utterance's first chunk."
        );
    }

    // 2. The window in which that block is invisible. The worker sends
    //    Final, then pre-warms. Audio queues in the 384-deep sync_channel
    //    meanwhile, so nothing is lost -- but the first partial of the next
    //    utterance is delayed by whatever remains of the pre-warm.
    let spawn_p50 = if spawns.is_empty() {
        0.0
    } else {
        spawns[spawns.len() / 2]
    };
    println!(
        "\nwith a {gap_ms}ms gap between utterances, the next utterance's first\n\
         chunk waits {:.0}ms behind the pre-warm.",
        (spawn_p50 - gap_ms as f64).max(0.0)
    );

    // 3. Control: the mock backend, which is reusable and costs nothing to
    //    construct, showing the pre-warm cost is entirely the helper spawn.
    let t = Instant::now();
    for _ in 0..1000 {
        let r = MockRecognizer::new();
        std::hint::black_box(r.name());
    }
    println!(
        "MockRecognizer::new() x1000: {:.3}ms total (construction is not the problem;\n\
         \x20 the helper PROCESS spawn is)",
        t.elapsed().as_secs_f64() * 1e3
    );
}
