//! Prompt ablation: why does the shipped prompt echo the input verbatim?
//!
//! The investigation bench measured an 11/24 guardrail rejection rate against
//! Qwen3-1.7B, every one of them `NoChange`: the model returned its input
//! untouched. The suspect is system-prompt rule 5, "If the instruction cannot
//! be applied, output the original text unchanged", which reads to a 1.7B
//! model as a standing permission to do nothing, and "tighten this up" is
//! exactly the sort of soft instruction it will decide it cannot apply.
//!
//! This measures three prompts against the same model and inputs:
//!
//! - `shipped`   the current `prompt::SYSTEM_PROMPT`
//! - `no_rule5`  identical, minus the do-nothing escape hatch
//! - `imperative` no_rule5 plus an explicit "you MUST change the text"
//!
//! Run: `cargo run -p llm --features llama --release --example prompt_ablation`

use std::time::Instant;

use llm::llama_backend::LlamaTransformer;
use llm::{guardrail::GuardrailConfig, models, prompt, Transformer};

const SHORT: &str = "we should probably ship the thing today i think";
const LONG: &str = "It is really quite important that we should try to make \
sure that the deploy happens today, because otherwise the customers might \
possibly be quite upset about it.";

/// The shipped prompt with rule 5 removed.
fn no_rule5() -> String {
    let p = prompt::SYSTEM_PROMPT;
    match p.find("5. If the instruction cannot be applied") {
        Some(i) => p[..i].trim_end().to_string(),
        None => p.to_string(),
    }
}

/// no_rule5 plus an explicit obligation to produce a changed text.
fn imperative() -> String {
    format!(
        "{}\n5. You MUST apply the instruction and output a changed text. \
Returning the input unchanged is a failure.",
        no_rule5()
    )
}

/// `imperative` plus two worked examples. Small instruct models imitate a
/// demonstrated output shape far more reliably than they follow a described
/// one, and the failure being measured here is a *shape* failure (echo the
/// input) rather than a comprehension failure.
fn few_shot() -> String {
    format!(
        "{}\n\nExamples of the required behaviour:\n\n\
TEXT BEGIN\nthe meeting is at 3 and we should probably all be there on time\nTEXT END\n\
Instruction: tighten this up\nTransformed text: The meeting is at 3. Please be on time.\n\n\
TEXT BEGIN\nhey can you send me that file when you get a sec\nTEXT END\n\
Instruction: make it more formal\nTransformed text: Could you please send me that file at your convenience?",
        imperative()
    )
}

struct Tally {
    name: &'static str,
    unchanged: usize,
    rejected: usize,
    runs: usize,
    total_ms: f64,
}

fn main() {
    let spec = &models::registry()[0];
    let path = models::fetch(spec, &models::default_cache_dir(), |_| {}).expect("fetch");

    let variants: Vec<(&'static str, String)> = vec![
        ("shipped", prompt::SYSTEM_PROMPT.to_string()),
        ("no_rule5", no_rule5()),
        ("imperative", imperative()),
        ("few_shot", few_shot()),
    ];

    let cases: &[(&str, &str)] = &[
        (SHORT, "tighten this up"),
        (SHORT, "make it more formal"),
        (SHORT, "fix the grammar"),
        (LONG, "tighten this up"),
        (LONG, "fix the grammar"),
        (LONG, "make it sound friendlier"),
    ];
    const REPEATS: usize = 2;

    let mut tallies = Vec::new();
    for (name, system) in &variants {
        // Reload per variant: the KV prefix cache is keyed on the system
        // prompt, so reusing one context across variants would measure a
        // warm prefix for the first and a cold one for the rest.
        let mut backend = LlamaTransformer::load(&path)
            .expect("load")
            .with_system_prompt(system.clone());
        let mut t = Tally {
            name,
            unchanged: 0,
            rejected: 0,
            runs: 0,
            total_ms: 0.0,
        };
        println!("\n=== variant: {name}");
        for (original, instruction) in cases {
            for _ in 0..REPEATS {
                let start = Instant::now();
                let raw = backend.transform(original, instruction).unwrap_or_default();
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                let cleaned = llm::sanitize::sanitize(&raw, instruction);
                let rejection = llm::guardrail::check(
                    original,
                    &cleaned,
                    instruction,
                    &GuardrailConfig::default(),
                );
                if cleaned.trim() == original.trim() {
                    t.unchanged += 1;
                }
                if rejection.is_some() {
                    t.rejected += 1;
                }
                t.runs += 1;
                t.total_ms += ms;
                let shown = cleaned.replace('\n', "\\n");
                println!(
                    "  {:<26} {:<5} {ms:>6.0}ms  {}",
                    instruction,
                    if std::ptr::eq(*original, SHORT) {
                        "short"
                    } else {
                        "long"
                    },
                    &shown[..shown.len().min(66)]
                );
            }
        }
        tallies.push(t);
    }

    println!("\n{}", "=".repeat(80));
    println!(
        "{:<12} {:>10} {:>10} {:>12}",
        "variant", "unchanged", "rejected", "mean_ms"
    );
    for t in &tallies {
        println!(
            "{:<12} {:>7}/{:<2} {:>7}/{:<2} {:>12.0}",
            t.name,
            t.unchanged,
            t.runs,
            t.rejected,
            t.runs,
            t.total_ms / t.runs as f64
        );
    }
}
