//! Manual latency/quality harness for the real llama.cpp backend.
//!
//! Requires the `llama` feature and a downloaded model:
//! ```sh
//! cargo run -p llm --features llama --release --example bench_freeform
//! ```
//! Measures model load, time-to-first-token, and total generation for a
//! realistic dictation sentence, then runs the full guarded pipeline. These
//! are the numbers docs/llm.md must quote.

use std::io::Write as _;
use std::time::Instant;

use llm::guardrail::GuardrailConfig;
use llm::llama_backend::LlamaTransformer;
use llm::models;

fn main() {
    let spec = &models::registry()[0];
    let cache = models::default_cache_dir();
    let path = models::fetch(spec, &cache, |p| {
        if let Some(total) = p.bytes_total {
            eprint!(
                "\rdownloading {}: {}/{} MB",
                spec.id,
                p.bytes_done / 1_000_000,
                total / 1_000_000
            );
        }
    })
    .expect("model fetch failed");

    let t0 = Instant::now();
    let mut backend = LlamaTransformer::load(&path).expect("model load failed");
    println!("model load: {:?}", t0.elapsed());

    let original = "It is really quite important that we should try to make \
                    sure that the deploy happens today, because otherwise \
                    the customers might possibly be quite upset about it.";

    for instruction in [
        "tighten this up",
        "make it more formal",
        "turn this into bullet points",
    ] {
        let t_start = Instant::now();
        let mut t_first = None;
        let result = llm::transform(
            &mut backend,
            original,
            instruction,
            &GuardrailConfig::default(),
            &mut |tok| {
                if t_first.is_none() {
                    t_first = Some(t_start.elapsed());
                }
                print!("{tok}");
                let _ = std::io::stdout().flush();
            },
        );
        println!();
        match result {
            Ok(edit) => println!(
                "instruction={instruction:?} ttft={:?} total={:?}\n  -> {}",
                t_first.unwrap_or_default(),
                t_start.elapsed(),
                edit.transformed
            ),
            Err(e) => println!(
                "instruction={instruction:?} ttft={:?} total={:?} REJECTED: {e}",
                t_first.unwrap_or_default(),
                t_start.elapsed()
            ),
        }
    }
}
