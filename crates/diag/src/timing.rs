//! Latency instrumentation matching spike-cli's read/parse/apply/write
//! breakdown, with percentile reporting.
//!
//! M0 established hard numbers (read ~30ms, write ~13ms). Regressions in
//! synchronous IPC latency are invisible to feel until they blow the 800ms
//! budget, so this harness exists to catch them numerically: record spans,
//! report p50/p90/p99, compare against a stated budget.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The pipeline stages spike-cli already reports, plus a catch-all so new
/// stages do not need an enum change to be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Read,
    Parse,
    Apply,
    Write,
    Other,
}

impl Stage {
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Read => "read",
            Stage::Parse => "parse",
            Stage::Apply => "apply",
            Stage::Write => "write",
            Stage::Other => "other",
        }
    }
}

/// A running timer for one stage. Finish it with [`Recorder::finish`], or let
/// it drop unfinished (an abandoned span records nothing, which is correct:
/// a failed operation's partial time would poison the percentiles).
pub struct Span {
    stage: Stage,
    started: Instant,
}

/// Collects span durations per stage. Not thread-safe by design: the edit
/// pipeline is single-threaded, and keeping this lock-free keeps the
/// instrumentation itself off the latency budget.
#[derive(Default)]
pub struct Recorder {
    samples: BTreeMap<Stage, Vec<Duration>>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&self, stage: Stage) -> Span {
        Span {
            stage,
            started: Instant::now(),
        }
    }

    pub fn finish(&mut self, span: Span) -> Duration {
        let elapsed = span.started.elapsed();
        self.samples.entry(span.stage).or_default().push(elapsed);
        elapsed
    }

    /// Record an externally measured duration (e.g. replayed from a log).
    pub fn record(&mut self, stage: Stage, d: Duration) {
        self.samples.entry(stage).or_default().push(d);
    }

    /// Time a closure as one span of `stage`.
    pub fn time<T>(&mut self, stage: Stage, f: impl FnOnce() -> T) -> T {
        let span = self.start(stage);
        let out = f();
        self.finish(span);
        out
    }

    /// Percentile summary for every stage that has samples.
    pub fn summary(&self) -> Vec<StageSummary> {
        self.samples
            .iter()
            .map(|(stage, samples)| StageSummary::from_samples(*stage, samples))
            .collect()
    }
}

/// Percentiles for one stage.
#[derive(Debug, Clone, PartialEq)]
pub struct StageSummary {
    pub stage: Stage,
    pub count: usize,
    pub p50: Duration,
    pub p90: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl StageSummary {
    fn from_samples(stage: Stage, samples: &[Duration]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort();
        StageSummary {
            stage,
            count: sorted.len(),
            p50: percentile(&sorted, 50.0),
            p90: percentile(&sorted, 90.0),
            p99: percentile(&sorted, 99.0),
            max: *sorted.last().expect("summary of empty samples"),
        }
    }

    /// One line suitable for doctor output.
    pub fn render(&self) -> String {
        format!(
            "{:<6} n={:<4} p50={:>9.3?} p90={:>9.3?} p99={:>9.3?} max={:>9.3?}",
            self.stage.label(),
            self.count,
            self.p50,
            self.p90,
            self.p99,
            self.max
        )
    }

    /// Whether p99 exceeds the given budget. Budget is judged at p99 rather
    /// than p50 because a dictation tool that is fast usually but hangs
    /// occasionally reads as broken, not as fast.
    pub fn over_budget(&self, budget: Duration) -> bool {
        self.p99 > budget
    }
}

/// Nearest-rank percentile on a pre-sorted slice. Nearest-rank is chosen over
/// interpolation because with the small sample counts a doctor run produces,
/// interpolated values report durations that never actually happened.
fn percentile(sorted: &[Duration], p: f64) -> Duration {
    assert!(!sorted.is_empty(), "percentile of empty slice");
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let sorted: Vec<Duration> = (1..=100).map(ms).collect();
        assert_eq!(percentile(&sorted, 50.0), ms(50));
        assert_eq!(percentile(&sorted, 90.0), ms(90));
        assert_eq!(percentile(&sorted, 99.0), ms(99));
    }

    #[test]
    fn single_sample_is_every_percentile() {
        let sorted = vec![ms(7)];
        assert_eq!(percentile(&sorted, 50.0), ms(7));
        assert_eq!(percentile(&sorted, 99.0), ms(7));
    }

    #[test]
    fn recorder_summarizes_per_stage() {
        let mut r = Recorder::new();
        for n in [10u64, 20, 30] {
            r.record(Stage::Read, ms(n));
        }
        r.record(Stage::Write, ms(5));
        let summary = r.summary();
        assert_eq!(summary.len(), 2);
        let read = summary.iter().find(|s| s.stage == Stage::Read).unwrap();
        assert_eq!(read.count, 3);
        assert_eq!(read.p50, ms(20));
        assert_eq!(read.max, ms(30));
    }

    #[test]
    fn abandoned_span_records_nothing() {
        let r = Recorder::new();
        {
            let _span = r.start(Stage::Apply); // dropped, never finished
        }
        assert!(r.summary().is_empty());
    }

    #[test]
    fn budget_judged_at_p99() {
        let mut r = Recorder::new();
        // 50 fast samples + 1 outlier: nearest-rank p99 of n=51 is the 51st
        // sample, so the single outlier must be what the budget is judged on.
        for _ in 0..50 {
            r.record(Stage::Read, ms(10));
        }
        r.record(Stage::Read, ms(500)); // one outlier
        let s = &r.summary()[0];
        assert!(s.over_budget(ms(100)), "outlier must trip the budget");
        assert!(!s.over_budget(ms(600)));
    }

    #[test]
    fn time_closure_returns_value_and_records() {
        let mut r = Recorder::new();
        let v = r.time(Stage::Parse, || 42);
        assert_eq!(v, 42);
        assert_eq!(r.summary()[0].count, 1);
    }
}
