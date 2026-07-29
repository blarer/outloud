//! Is "drop system-prompt rule 5" a real effect or sampling noise?
//!
//! `prompt_ablation` measured 12 requests per variant and saw the verbatim-
//! echo rate fall from 6-7/12 (shipped) to 2-3/12 (rule 5 replaced with an
//! explicit obligation). That is a large effect, but 12 samples at p~0.5 has
//! a standard error near 14 percentage points, so the two runs it was based
//! on cannot distinguish "halved" from "got lucky twice".
//!
//! This is the confirmation run: the two variants that matter, many more
//! samples, and a two-proportion z-test so the conclusion is a statistic
//! rather than an impression. The recommendation in
//! docs/investigations/edit-intent.md depends on this holding up.
//!
//! Run: `cargo run -p llm --features llama --release --example rule5_confirm`

use std::time::Instant;

use llm::llama_backend::LlamaTransformer;
use llm::{models, prompt, Transformer};

/// Inputs spanning the sizes real selections take, since the echo rate is
/// known to worsen with length and a single size would bias the result.
const INPUTS: &[&str] = &[
    "we should probably ship the thing today i think",
    "the meeting got moved to thursday so we have a bit more time now",
    "It is really quite important that we should try to make sure that the \
     deploy happens today, because otherwise the customers might possibly be \
     quite upset about it.",
    "I think the main blocker was the migration script but that landed on \
     Tuesday and has been running in staging without any problems since then, \
     so as far as I can tell there is nothing stopping us.",
];

/// The soft instructions the model actually fails on. Literal commands are
/// excluded: the parser handles those, and including them would inflate the
/// success rate of both variants and mask the effect.
const INSTRUCTIONS: &[&str] = &[
    "tighten this up",
    "make it more formal",
    "fix the grammar",
    "make it sound friendlier",
    "make this shorter",
    "make the tone more direct",
];

const REPEATS: usize = 9;

fn shipped() -> String {
    prompt::SYSTEM_PROMPT.to_string()
}

/// Rule 5 (the do-nothing escape hatch) replaced with an explicit obligation.
fn no_escape_hatch() -> String {
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

/// Verbatim-echo count and total for one variant.
fn measure(path: &std::path::Path, system: String, label: &str) -> (usize, usize) {
    let mut backend = LlamaTransformer::load(path)
        .expect("load")
        .with_system_prompt(system);
    let mut echoed = 0;
    let mut total = 0;
    let start = Instant::now();
    for original in INPUTS {
        for instruction in INSTRUCTIONS {
            for _ in 0..REPEATS {
                let raw = backend.transform(original, instruction).unwrap_or_default();
                let cleaned = llm::sanitize::sanitize(&raw, instruction);
                if cleaned.trim() == original.trim() {
                    echoed += 1;
                }
                total += 1;
            }
        }
    }
    println!(
        "{label:<16} echoed {echoed:>3}/{total:<3} ({:>5.1}%)  [{:?}]",
        100.0 * echoed as f64 / total as f64,
        start.elapsed()
    );
    (echoed, total)
}

/// Two-proportion z-test. Returns (z, approximate two-sided p).
fn z_test(x1: usize, n1: usize, x2: usize, n2: usize) -> (f64, f64) {
    let p1 = x1 as f64 / n1 as f64;
    let p2 = x2 as f64 / n2 as f64;
    let pooled = (x1 + x2) as f64 / (n1 + n2) as f64;
    let se = (pooled * (1.0 - pooled) * (1.0 / n1 as f64 + 1.0 / n2 as f64)).sqrt();
    if se == 0.0 {
        return (0.0, 1.0);
    }
    let z = (p1 - p2) / se;
    // Two-sided p via the complementary error function, approximated with
    // Abramowitz & Stegun 7.1.26 (max error 1.5e-7): good to far more
    // precision than this sample size warrants.
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erfc = poly * (-x * x).exp();
    (z, erfc)
}

fn main() {
    let spec = &models::registry()[0];
    let path = models::fetch(spec, &models::default_cache_dir(), |_| {}).expect("fetch");
    let n = INPUTS.len() * INSTRUCTIONS.len() * REPEATS;
    println!("{n} requests per variant, {} total\n", n * 2);

    let (e_ship, n_ship) = measure(&path, shipped(), "shipped");
    let (e_fixed, n_fixed) = measure(&path, no_escape_hatch(), "no_escape_hatch");

    let (z, p) = z_test(e_ship, n_ship, e_fixed, n_fixed);
    let r_ship = 100.0 * e_ship as f64 / n_ship as f64;
    let r_fixed = 100.0 * e_fixed as f64 / n_fixed as f64;

    println!("\n{}", "=".repeat(70));
    println!("shipped echo rate:         {r_ship:.1}%");
    println!("rule-5-removed echo rate:  {r_fixed:.1}%");
    println!("absolute reduction:        {:.1} points", r_ship - r_fixed);
    if r_fixed > 0.0 {
        println!("relative:                  {:.2}x fewer", r_ship / r_fixed);
    }
    println!("two-proportion z = {z:.2}, two-sided p = {p:.4}");
    println!(
        "\nverdict: {}",
        if p < 0.05 {
            "SIGNIFICANT at p<0.05 - the prompt fix is a real effect"
        } else {
            "NOT significant - do not claim the prompt fix helps"
        }
    );
}
