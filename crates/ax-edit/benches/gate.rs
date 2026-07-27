//! Regression gate for the accessibility hot path, wired to `diag::timing`.
//!
//! Not a criterion benchmark: criterion answers "how fast is it", this
//! answers "is it still fast enough", with an exit code CI can act on. It
//! samples the real `snapshot_focused` path repeatedly, summarises with the
//! same Recorder/percentile machinery the doctor uses, and fails loudly when
//! a percentile crosses its threshold.
//!
//! Thresholds are derived from measurements on M-series/macOS 26 (see
//! docs/latency.md): warm snapshot p50 measured at ~155us and cold full path
//! at ~20ms. The gates leave an order of magnitude of headroom so they fail
//! on genuine regressions (an added synchronous round trip, a lost batch
//! read) rather than on a slow CI machine or a busy target application:
//!
//!   read p50  < 2ms    (measured ~0.16ms warm: 12x headroom)
//!   read p99  < 50ms   (measured cold path ~20ms: covers first-contact cost)
//!
//! Skips (exit 0) when the environment cannot produce a valid measurement,
//! because a gate that fails for environmental reasons trains people to
//! ignore it. CI that wants to *require* the gate should run it via
//! scripts/bench-gate.sh, which sets up a focused text field first.

use std::time::Duration;

use diag::timing::{Recorder, Stage};

const SAMPLES: usize = 200;
const P50_BUDGET: Duration = Duration::from_millis(2);
const P99_BUDGET: Duration = Duration::from_millis(50);

fn main() {
    if !cfg!(target_os = "macos") {
        eprintln!("gate: macOS-only, skipping");
        return;
    }
    if !ax_edit::is_trusted(false) {
        eprintln!("gate: process not trusted for Accessibility, skipping");
        return;
    }
    // One probe outside the recorder: its cost is the cold path, which the
    // p99 budget covers, but if the environment has no focused text field at
    // all the gate must skip rather than time 200 failures.
    if let Err(e) = ax_edit::snapshot_focused() {
        eprintln!("gate: no focused text field ({e}), skipping");
        return;
    }

    let mut recorder = Recorder::new();
    // Include one deliberately cold-ish first sample by recording from the
    // start; the p99 budget is what judges it.
    for _ in 0..SAMPLES {
        let span = recorder.start(Stage::Read);
        match ax_edit::snapshot_focused() {
            Ok(_) => {
                recorder.finish(span);
            }
            Err(e) => {
                // Focus drifted away mid-run (the operator switched apps).
                // Partial data would judge the code on the environment, so
                // report and skip instead.
                eprintln!("gate: lost the focused field mid-run ({e}), skipping");
                return;
            }
        }
    }

    let mut failed = false;
    for summary in recorder.summary() {
        println!("{}", summary.render());
        if summary.p50 > P50_BUDGET {
            eprintln!(
                "gate FAIL: {} p50 {:?} exceeds budget {:?}",
                summary.stage.label(),
                summary.p50,
                P50_BUDGET
            );
            failed = true;
        }
        if summary.over_budget(P99_BUDGET) {
            eprintln!(
                "gate FAIL: {} p99 {:?} exceeds budget {:?}",
                summary.stage.label(),
                summary.p99,
                P99_BUDGET
            );
            failed = true;
        }
    }
    if failed {
        std::process::exit(1);
    }
    println!("gate OK: p50 <= {P50_BUDGET:?}, p99 <= {P99_BUDGET:?}");
}
