//! `doctor`: run every diagnostic check and print named next actions.
//!
//! Usage:
//!   doctor            run all checks, human-readable
//!   doctor --report   also print the redacted bug-report bundle
//!   doctor --bench    also run the AX latency micro-benchmark (needs trust)
//!
//! Exit code is the worst status seen (0 pass, 1 warn, 2 fail) so scripts can
//! gate on it. Output mirrors to HEXA_SPIKE_LOG when set, because a
//! LaunchServices launch (the correct way, see docs/macos-permissions.md)
//! detaches from the terminal.

use std::io::Write as _;
use std::time::Duration;

use diag::timing::{Recorder, Stage};
use diag::{run_all, Env, Status};

fn main() {
    // Same log-mirroring contract as spike-cli: a LaunchServices launch has
    // no terminal, so the wrapper script tails this file instead.
    let log = std::env::var("HEXA_SPIKE_LOG")
        .ok()
        .and_then(|p| std::fs::File::create(p).ok());
    let mut sink = MultiSink { log };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let want_report = args.iter().any(|a| a == "--report");
    let want_bench = args.iter().any(|a| a == "--bench");

    let env = Env::capture();
    let reports = run_all(&env);

    let mut worst = Status::Pass;
    writeln!(sink, "hexavoice doctor\n").ok();
    for r in &reports {
        worst = worst.max(r.outcome.status);
        writeln!(
            sink,
            "[{}] {:<26} {}",
            r.outcome.status, r.name, r.outcome.detail
        )
        .ok();
        if let (Some(class), Some(remedy)) = (&r.outcome.class, &r.outcome.remedy) {
            writeln!(sink, "       class:  {class}").ok();
            writeln!(sink, "       remedy: {remedy}").ok();
        }
    }

    let bug_count = reports
        .iter()
        .filter(|r| {
            r.outcome
                .class
                .map(|c| c.worth_a_github_issue())
                .unwrap_or(false)
        })
        .count();
    writeln!(
        sink,
        "\nverdict: {worst}. {bug_count} bug-class failure(s); only those belong in a GitHub issue."
    )
    .ok();

    if want_bench {
        bench(&mut sink);
    }

    if want_report {
        writeln!(
            sink,
            "\n----- pasteable redacted report -----\n{}",
            diag::redact::bundle(&reports)
        )
        .ok();
    }

    // Marker consumed by scripts/doctor.sh so a detached launch still yields
    // an exit status.
    let code = match worst {
        Status::Pass => 0,
        Status::Warn => 1,
        Status::Fail => 2,
    };
    if std::env::var("HEXA_SPIKE_LOG").is_ok() {
        writeln!(sink, "__EXIT__{code}").ok();
    }
    std::process::exit(code);
}

/// Micro-benchmark of the read path with percentile reporting. This is the
/// numeric regression tripwire: M0 measured read at 25-33ms, so a p99 over
/// the 100ms budget below means something environmental has degraded (busy
/// Electron target, new OS throttling) even though nothing "errored".
fn bench(sink: &mut MultiSink) {
    const ITERATIONS: usize = 20;
    const READ_BUDGET: Duration = Duration::from_millis(100);
    if !ax_edit::is_trusted(false) {
        writeln!(sink, "\nbench: skipped (accessibility not trusted)").ok();
        return;
    }
    let mut rec = Recorder::new();
    for _ in 0..ITERATIONS {
        // The snapshot may fail (e.g. no focused field); a failure is not a
        // latency sample, so only successful reads are recorded.
        let span = rec.start(Stage::Read);
        if ax_edit::snapshot_focused().is_ok() {
            rec.finish(span);
        }
    }
    writeln!(sink, "\nbench ({ITERATIONS} focused-field reads):").ok();
    for s in rec.summary() {
        let flag = if s.over_budget(READ_BUDGET) {
            "  <-- OVER BUDGET"
        } else {
            ""
        };
        writeln!(sink, "  {}{}", s.render(), flag).ok();
    }
    if rec.summary().is_empty() {
        writeln!(sink, "  no successful reads (focus a text field and rerun)").ok();
    }
}

/// Write to stdout and, when configured, the log file. A doctor that loses
/// its own output when launched correctly (detached) would be self-defeating.
struct MultiSink {
    log: Option<std::fs::File>,
}

impl std::io::Write for MultiSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = std::io::stdout().write(buf)?;
        if let Some(f) = &mut self.log {
            let _ = f.write_all(buf);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()?;
        if let Some(f) = &mut self.log {
            let _ = f.flush();
        }
        Ok(())
    }
}
