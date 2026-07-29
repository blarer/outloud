//! Head-to-head: on the commands a deterministic parser CAN handle, does the
//! model do better, worse, or the same, and at what latency?
//!
//! This is the decisive comparison for the recommendation. The prototype in
//! the investigation handles 21 commands the shipped parser cannot, in ~6us,
//! with exactly predictable output. If Qwen3-1.7B also handles them
//! correctly, the parser work is redundant and only the model matters. If it
//! does not, the parser is strictly better on that traffic and the model
//! should be reserved for what only it can do.
//!
//! Each case carries the exact string a correct deterministic implementation
//! produces, so scoring is a string comparison, not a judgement call.
//!
//! Run: `cargo run -p llm --features llama --release --example det_vs_model`

use std::time::Instant;

use llm::llama_backend::LlamaTransformer;
use llm::{models, prompt, Transformer};

const TEXT: &str = "we should ship today. the customers are waiting. lets go";

/// (instruction, exact output a deterministic implementation produces)
const CASES: &[(&str, &str)] = &[
    (
        "delete the last sentence",
        "we should ship today. the customers are waiting.",
    ),
    (
        "delete the last word",
        "we should ship today. the customers are waiting. lets",
    ),
    (
        "add a period at the end",
        "we should ship today. the customers are waiting. lets go.",
    ),
    (
        "wrap this in quotes",
        "\"we should ship today. the customers are waiting. lets go\"",
    ),
    (
        "make it snake case",
        "we_should_ship_today_the_customers_are_waiting_lets_go",
    ),
    (
        "make it camel case",
        "weShouldShipTodayTheCustomersAreWaitingLetsGo",
    ),
    (
        "make it kebab case",
        "we-should-ship-today-the-customers-are-waiting-lets-go",
    ),
    (
        "turn this into bullet points",
        "- we should ship today.\n- the customers are waiting.\n- lets go",
    ),
    (
        "number these lines",
        "1. we should ship today.\n2. the customers are waiting.\n3. lets go",
    ),
    (
        "capitalize the first word",
        "We should ship today. the customers are waiting. lets go",
    ),
];

const REPEATS: usize = 3;

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

/// Compare ignoring only whitespace runs and trailing whitespace: a model
/// that gets the transformation right but pads a newline should not be
/// scored as wrong.
fn equivalent(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    norm(a) == norm(b)
}

fn main() {
    let spec = &models::registry()[0];
    let path = models::fetch(spec, &models::default_cache_dir(), |_| {}).expect("fetch");
    let mut backend = LlamaTransformer::load(&path)
        .expect("load")
        .with_system_prompt(best_prompt());

    let mut exact = 0;
    let mut runs = 0;
    let mut times = Vec::new();

    println!(
        "{:<30} {:>5} {:>8}  model output",
        "instruction", "match", "ms"
    );
    println!("{}", "-".repeat(110));
    for (instruction, expected) in CASES {
        for _ in 0..REPEATS {
            let start = Instant::now();
            let raw = backend.transform(TEXT, instruction).unwrap_or_default();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            times.push(ms);
            let cleaned = llm::sanitize::sanitize(&raw, instruction);
            let ok = equivalent(&cleaned, expected);
            if ok {
                exact += 1;
            }
            runs += 1;
            let shown = cleaned.replace('\n', " | ");
            println!(
                "{instruction:<30} {:>5} {ms:>8.0}  {}",
                if ok { "YES" } else { "no" },
                &shown[..shown.len().min(60)]
            );
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("\n{}", "=".repeat(110));
    println!(
        "model matched the deterministic result: {exact}/{runs} ({:.0}%)",
        100.0 * exact as f64 / runs as f64
    );
    println!(
        "model latency p50 {:.0}ms / p90 {:.0}ms   vs deterministic ~6us",
        times[times.len() / 2],
        times[(times.len() as f64 * 0.9) as usize]
    );
}
