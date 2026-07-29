//! Does freeform editing actually WORK, and does it depend on input size?
//!
//! `prompt_ablation` showed Qwen3-1.7B returning its input verbatim on a
//! large fraction of requests, and the failures clustered visibly on the
//! longer input. This isolates that: same instructions, four input sizes,
//! enough repeats to separate signal from temperature-0.3 sampling noise.
//!
//! The number that matters for the product decision is the usable-output
//! rate per input size, because "freeform edits work" is a claim about the
//! text users actually select, and dictated selections are usually one or
//! two sentences, not a page.
//!
//! Run: `cargo run -p llm --features llama --release --example size_sweep`

use std::time::Instant;

use llm::llama_backend::LlamaTransformer;
use llm::{guardrail::GuardrailConfig, models, prompt, Transformer};

/// Inputs at increasing size, all of them plausible dictation output.
const INPUTS: &[(&str, &str)] = &[
    (
        "1 sentence",
        "we should probably ship the thing today i think",
    ),
    (
        "2 sentences",
        "we should probably ship the thing today i think. the customers have \
         been waiting on this for a while now and they are getting restless",
    ),
    (
        "paragraph",
        "It is really quite important that we should try to make sure that the \
         deploy happens today, because otherwise the customers might possibly \
         be quite upset about it. We have been putting this off for a while \
         now and the longer we wait the worse it is going to get for everyone \
         involved, including the support team who have to field the questions.",
    ),
    (
        "long paragraph",
        "It is really quite important that we should try to make sure that the \
         deploy happens today, because otherwise the customers might possibly \
         be quite upset about it. We have been putting this off for a while \
         now and the longer we wait the worse it is going to get for everyone \
         involved, including the support team who have to field the questions. \
         I think the main blocker was the migration script but that landed on \
         Tuesday and has been running in staging without any problems since \
         then, so as far as I can tell there is nothing actually stopping us \
         from cutting the release this afternoon if we decide to.",
    ),
];

const INSTRUCTIONS: &[&str] = &[
    "tighten this up",
    "make it more formal",
    "fix the grammar",
    "make it sound friendlier",
];

const REPEATS: usize = 3;

/// The shipped prompt with rule 5 (the do-nothing escape hatch) removed and
/// an explicit obligation to change the text: the best variant measured by
/// `prompt_ablation`.
fn best_prompt() -> String {
    let p = prompt::SYSTEM_PROMPT;
    let trimmed = match p.find("5. If the instruction cannot be applied") {
        Some(i) => p[..i].trim_end().to_string(),
        None => p.to_string(),
    };
    format!(
        "{trimmed}\n5. You MUST apply the instruction and output a changed \
text. Returning the input unchanged is a failure."
    )
}

fn main() {
    let spec = &models::registry()[0];
    let path = models::fetch(spec, &models::default_cache_dir(), |_| {}).expect("fetch");
    let mut backend = LlamaTransformer::load(&path)
        .expect("load")
        .with_system_prompt(best_prompt());

    println!(
        "{:<16} {:>6} {:>6} {:>10} {:>10} {:>10} {:>10}",
        "input size", "words", "runs", "usable", "echoed", "rejected", "p50_ms"
    );
    println!("{}", "-".repeat(80));

    for (label, original) in INPUTS {
        let mut usable = 0;
        let mut echoed = 0;
        let mut rejected = 0;
        let mut times = Vec::new();
        for instruction in INSTRUCTIONS {
            for _ in 0..REPEATS {
                let start = Instant::now();
                let raw = backend.transform(original, instruction).unwrap_or_default();
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                let cleaned = llm::sanitize::sanitize(&raw, instruction);
                if cleaned.trim() == original.trim() {
                    echoed += 1;
                }
                match llm::guardrail::check(
                    original,
                    &cleaned,
                    instruction,
                    &GuardrailConfig::default(),
                ) {
                    Some(_) => rejected += 1,
                    None => usable += 1,
                }
            }
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = times[times.len() / 2];
        let runs = INSTRUCTIONS.len() * REPEATS;
        println!(
            "{label:<16} {:>6} {runs:>6} {:>9.0}% {:>9.0}% {:>9.0}% {p50:>10.0}",
            original.split_whitespace().count(),
            100.0 * usable as f64 / runs as f64,
            100.0 * echoed as f64 / runs as f64,
            100.0 * rejected as f64 / runs as f64,
        );
    }
}
