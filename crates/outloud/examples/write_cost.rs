//! Cost of the WRITE stage against the currently focused field, broken down
//! by the transport the daemon would actually choose.
//!
//! The pipeline's `inject_ms` is one opaque number covering: intent parse,
//! the `stage_terminal_edit` AX probe, the re-snapshot, the caret splice,
//! and the transport write. docs/beta-readiness.md shows it ranging 16-57ms
//! across runs, which is a big fraction of a 116ms utterance and is
//! currently unattributed.
//!
//! SAFETY: this WRITES into the focused field. Run it against a scratch
//! TextEdit document only. It refuses to run unless OUTLOUD_INJECT_PROBE=1
//! is set, so it can never fire by accident during another measurement.
//!
//! Run: focus a scratch TextEdit doc, then
//!   OUTLOUD_INJECT_PROBE=1 cargo run --release -p outloud --example write_cost

use std::time::Instant;

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * q) as usize).min(v.len() - 1)]
}

fn main() {
    if std::env::var_os("OUTLOUD_INJECT_PROBE").is_none_or(|v| v != "1") {
        eprintln!(
            "write_cost WRITES into the focused text field.\n\
             Focus a SCRATCH TextEdit document, then rerun with \
             OUTLOUD_INJECT_PROBE=1."
        );
        std::process::exit(2);
    }
    let Ok(snap) = ax_edit::snapshot_focused() else {
        eprintln!("no focused text field; focus a scratch TextEdit doc and rerun");
        std::process::exit(1);
    };
    println!("target: {} in {:?}", snap.role, snap.app);

    let reps = 50;

    // Stage: the AX probe inject.rs makes on EVERY utterance before it even
    // decides dictate-vs-edit (stage_terminal_edit -> frontmost_app).
    let mut fa = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        let _ = ax_edit::frontmost_app();
        fa.push(t.elapsed().as_secs_f64() * 1e3);
    }

    // Stage: the re-snapshot insert_with_fallback takes at commit time.
    let mut re = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        let _ = ax_edit::snapshot_focused();
        re.push(t.elapsed().as_secs_f64() * 1e3);
    }

    // Stage: the AXValue write itself, at a realistic transcript length.
    let base = snap.value.clone().unwrap_or_default();
    let mut wr = Vec::new();
    for i in 0..reps {
        let payload = format!("{base}{}", " probe".repeat((i % 3) + 1));
        let t = Instant::now();
        let _ = ax_edit::replace_focused(&payload);
        wr.push(t.elapsed().as_secs_f64() * 1e3);
    }
    // Put the field back the way we found it.
    let _ = ax_edit::replace_focused(&base);

    // Stage: intent parse, the pure-CPU half of the write path.
    let mut ip = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        let intent = edit_intent::parse("change quick to slow");
        std::hint::black_box(edit_intent::apply("the quick brown fox", &intent));
        ip.push(t.elapsed().as_secs_f64() * 1e3);
    }

    let row = |name: &str, v: &mut [f64]| {
        println!(
            "  {name:<34} p50 {:>7.3}ms  p99 {:>7.3}ms",
            pct(v, 0.5),
            pct(v, 0.99)
        );
    };
    println!("write-stage breakdown (n={reps} each):");
    row("frontmost_app (terminal probe)", &mut fa);
    row("snapshot_focused (re-read at commit)", &mut re);
    row("replace_focused (the AXValue write)", &mut wr);
    row("edit_intent parse+apply", &mut ip);
    println!(
        "\n  sum of the three AX round trips: {:.2}ms p50",
        pct(&mut fa, 0.5) + pct(&mut re, 0.5) + pct(&mut wr, 0.5)
    );
}
