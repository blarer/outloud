//! Investigation harness: measured freeform-edit latency and quality for the
//! real llama.cpp backend, over a realistic instruction set with repeats.
//!
//!   cargo run -p llm --features llama --release --example bench_investigation
//!
//! Reports per-instruction TTFT and total, plus p50/p90 across all runs, so
//! docs/investigations/edit-intent.md can quote distribution rather than a
//! single lucky sample.

use std::time::Instant;

use llm::guardrail::GuardrailConfig;
use llm::llama_backend::LlamaTransformer;
use llm::models;

/// A short dictated sentence: the common case for an edit-by-voice target.
const SHORT: &str = "we should probably ship the thing today i think";

/// A realistic paragraph-sized selection: the expensive case.
const LONG: &str = "It is really quite important that we should try to make \
sure that the deploy happens today, because otherwise the customers might \
possibly be quite upset about it. We have been putting this off for a while \
now and the longer we wait the worse it is going to get for everyone \
involved, including the support team who have to field the questions.";

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn main() {
    let spec = &models::registry()[0];
    let cache = models::default_cache_dir();
    let path = models::fetch(spec, &cache, |_| {}).expect("model fetch failed");

    let t0 = Instant::now();
    let mut backend = LlamaTransformer::load(&path).expect("model load failed");
    let load = t0.elapsed();

    let cases: &[(&str, &str)] = &[
        (SHORT, "tighten this up"),
        (SHORT, "make it more formal"),
        (SHORT, "fix the grammar"),
        (SHORT, "make it more concise"),
        (LONG, "tighten this up"),
        (LONG, "make it more formal"),
        (LONG, "turn this into bullet points"),
        (LONG, "summarize this"),
    ];

    const REPEATS: usize = 3;
    let mut ttfts = Vec::new();
    let mut totals = Vec::new();
    let mut short_totals = Vec::new();
    let mut long_totals = Vec::new();
    let mut rejects = 0usize;

    println!("== model load: {load:?} (warm page cache, mmap)\n");
    println!(
        "{:<7} {:<30} {:>9} {:>9}  verdict",
        "input", "instruction", "ttft_ms", "total_ms"
    );
    println!("{}", "-".repeat(100));

    for (original, instruction) in cases {
        let is_short = std::ptr::eq(*original, SHORT);
        for _ in 0..REPEATS {
            let start = Instant::now();
            let mut first = None;
            let result = llm::transform(
                &mut backend,
                original,
                instruction,
                &GuardrailConfig::default(),
                &mut |_| {
                    if first.is_none() {
                        first = Some(start.elapsed());
                    }
                },
            );
            let total = start.elapsed().as_secs_f64() * 1000.0;
            let ttft = first.unwrap_or_default().as_secs_f64() * 1000.0;
            ttfts.push(ttft);
            totals.push(total);
            if is_short {
                short_totals.push(total);
            } else {
                long_totals.push(total);
            }
            let verdict = match &result {
                Ok(e) => {
                    let t = e.transformed.replace('\n', " ");
                    format!("OK: {}", &t[..t.len().min(60)])
                }
                Err(e) => {
                    rejects += 1;
                    format!("REJECTED: {e}")
                }
            };
            println!(
                "{:<7} {:<30} {ttft:>9.0} {total:>9.0}  {verdict}",
                if is_short { "short" } else { "long" },
                instruction
            );
        }
    }

    ttfts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    short_totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    long_totals.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!("\n{}", "=".repeat(100));
    println!(
        "n = {} requests, {rejects} guardrail rejections",
        totals.len()
    );
    println!(
        "ttft   p50 {:.0}ms  p90 {:.0}ms  max {:.0}ms",
        pct(&ttfts, 0.5),
        pct(&ttfts, 0.9),
        ttfts.last().copied().unwrap_or(0.0)
    );
    println!(
        "total  p50 {:.0}ms  p90 {:.0}ms  max {:.0}ms",
        pct(&totals, 0.5),
        pct(&totals, 0.9),
        totals.last().copied().unwrap_or(0.0)
    );
    println!(
        "short-input total  p50 {:.0}ms  p90 {:.0}ms",
        pct(&short_totals, 0.5),
        pct(&short_totals, 0.9)
    );
    println!(
        "long-input total   p50 {:.0}ms  p90 {:.0}ms",
        pct(&long_totals, 0.5),
        pct(&long_totals, 0.9)
    );
}
