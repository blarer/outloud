//! How long does the deterministic path actually take?
//!
//! The whole argument for this crate is that it costs microseconds where a
//! language model costs hundreds of milliseconds
//! (`docs/investigations/edit-intent.md` measured 324-429ms for the model on
//! the same commands). That argument is only as good as the measurement, and
//! the scope-aware grammar does far more work per utterance than the original
//! four verbs did: tokenisation, sentence segmentation, span resolution, and
//! a splice.
//!
//! Measures parse and apply separately, over the same realistic prose the
//! test corpus uses, and reports the distribution rather than a single mean
//! so an outlier cannot hide behind an average.
//!
//! Run: `cargo run -p edit-intent --release --example parse_timing`

use edit_intent::{apply, parse};
use std::time::Instant;

const PROSE: &str = "It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon";

/// A spread across every rule family, so the reported figures are not
/// dominated by whichever shape happens to be cheapest.
const COMMANDS: &[&str] = &[
    "change quick to slow",
    "delete really",
    "add and thanks",
    "make it all caps",
    "delete the last sentence",
    "remove the first sentence",
    "delete the last word",
    "uppercase the first letter",
    "capitalize the first word",
    "make the last sentence title case",
    "add a period at the end",
    "add a comma after today",
    "remove the last comma",
    "wrap this in quotes",
    "make it snake case",
    "make it camel case",
    "turn this into bullet points",
    "number these sentences",
    "split this into sentences",
    "in the last sentence change its to it's",
    "undo that",
    "tighten this up",
    "translate this to spanish",
];

fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn main() {
    // Warm the instruction cache and any lazy allocation, so the reported
    // distribution describes steady state rather than first-call cost. The
    // investigation's 52us figure was a cold first call.
    for _ in 0..1_000 {
        for c in COMMANDS {
            let _ = apply(PROSE, &parse(c));
        }
    }

    const ROUNDS: usize = 2_000;
    let mut samples = Vec::with_capacity(ROUNDS * COMMANDS.len());
    let mut parse_only = Vec::with_capacity(ROUNDS * COMMANDS.len());
    for _ in 0..ROUNDS {
        for c in COMMANDS {
            let t = Instant::now();
            let intent = parse(c);
            let parsed_ns = t.elapsed().as_nanos();
            let _ = apply(PROSE, &intent);
            samples.push(t.elapsed().as_nanos());
            parse_only.push(parsed_ns);
        }
    }
    samples.sort_unstable();
    parse_only.sort_unstable();

    let us = |ns: u128| ns as f64 / 1000.0;
    println!("commands:  {}", COMMANDS.len());
    println!("samples:   {}", samples.len());
    println!(
        "parse:       p50 {:.2}us  p90 {:.2}us  p99 {:.2}us  max {:.2}us",
        us(percentile(&parse_only, 0.50)),
        us(percentile(&parse_only, 0.90)),
        us(percentile(&parse_only, 0.99)),
        us(*parse_only.last().unwrap())
    );
    println!(
        "parse+apply: p50 {:.2}us  p90 {:.2}us  p99 {:.2}us  max {:.2}us",
        us(percentile(&samples, 0.50)),
        us(percentile(&samples, 0.90)),
        us(percentile(&samples, 0.99)),
        us(*samples.last().unwrap())
    );

    // Per-command, so a slow family is visible rather than pooled away.
    println!("\n{:<44} p50 parse+apply", "command");
    println!("{}", "-".repeat(64));
    for c in COMMANDS {
        let mut per = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let t = Instant::now();
            let _ = apply(PROSE, &parse(c));
            per.push(t.elapsed().as_nanos());
        }
        per.sort_unstable();
        println!("{c:<44} {:.2}us", us(percentile(&per, 0.50)));
    }
}
