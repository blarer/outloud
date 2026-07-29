//! Does reusing the context actually cut TTFT, or does KV-cache clearing
//! cost back what context construction saved?
//!
//! `ctx_cost` showed `new_context()` at 39ms of a 177ms measured TTFT, and
//! the investigation recommended hoisting it out of the request. That
//! recommendation was a projection, not a measurement: a reused context must
//! have its KV cache cleared between requests, and if clearing costs as much
//! as constructing, the recommendation is worthless.
//!
//! This measures both arrangements against the same model and prompt:
//!
//! - `per_request`  a fresh `LlamaContext` each time (what ships today)
//! - `reused`       one context, KV cache cleared between requests
//!
//! Run: `cargo run -p llm --features llama --release --example ctx_reuse`

use std::num::NonZeroU32;
use std::time::Instant;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use llm::{models, prompt};

const RUNS: usize = 10;

fn sampler() -> LlamaSampler {
    LlamaSampler::chain_simple([
        LlamaSampler::min_p(0.05, 1),
        LlamaSampler::temp(0.3),
        LlamaSampler::dist(0),
    ])
}

fn med(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let spec = &models::registry()[0];
    let path = models::fetch(spec, &models::default_cache_dir(), |_| {}).expect("fetch");

    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(
        &backend,
        &path,
        &LlamaModelParams::default().with_n_gpu_layers(1_000_000),
    )
    .expect("model");
    let n_ctx = NonZeroU32::new(4096).unwrap();

    let template = model.chat_template(None).expect("template");
    let render = |text: &str| {
        let messages = vec![
            LlamaChatMessage::new(
                "system".into(),
                format!("{} /no_think", prompt::SYSTEM_PROMPT),
            )
            .unwrap(),
            LlamaChatMessage::new("user".into(), prompt::user_prompt(text, "tighten this up"))
                .unwrap(),
        ];
        model
            .apply_chat_template(&template, &messages, true)
            .expect("render")
    };

    // Vary the input per run so neither arrangement gets an unfair cache hit
    // on an identical prompt.
    let inputs: Vec<String> = (0..RUNS)
        .map(|i| format!("we should probably ship the thing today i think, item {i}"))
        .collect();

    // --- Arrangement A: fresh context per request (today's behaviour).
    let mut per_request = Vec::new();
    for input in &inputs {
        let tokens = model.str_to_token(&render(input), AddBos::Never).unwrap();
        let start = Instant::now();
        let mut ctx = model
            .new_context(
                &backend,
                LlamaContextParams::default().with_n_ctx(Some(n_ctx)),
            )
            .expect("ctx");
        let mut batch = LlamaBatch::new(n_ctx.get() as usize, 1);
        let last = tokens.len() - 1;
        for (i, t) in tokens.iter().enumerate() {
            batch.add(*t, i as i32, &[0], i == last).unwrap();
        }
        ctx.decode(&mut batch).unwrap();
        let mut s = sampler();
        let tok = s.sample(&ctx, batch.n_tokens() - 1);
        s.accept(tok);
        per_request.push(start.elapsed().as_secs_f64() * 1000.0);
    }

    // --- Arrangement B: one context, cleared between requests.
    let mut ctx = model
        .new_context(
            &backend,
            LlamaContextParams::default().with_n_ctx(Some(n_ctx)),
        )
        .expect("ctx");
    let mut reused = Vec::new();
    for input in &inputs {
        let tokens = model.str_to_token(&render(input), AddBos::Never).unwrap();
        let start = Instant::now();
        // Drop the previous request's KV entries. This is the cost that
        // could eat the saving, so it is inside the timed region.
        ctx.clear_kv_cache_seq(Some(0), None, None).unwrap();
        let mut batch = LlamaBatch::new(n_ctx.get() as usize, 1);
        let last = tokens.len() - 1;
        for (i, t) in tokens.iter().enumerate() {
            batch.add(*t, i as i32, &[0], i == last).unwrap();
        }
        ctx.decode(&mut batch).unwrap();
        let mut s = sampler();
        let tok = s.sample(&ctx, batch.n_tokens() - 1);
        s.accept(tok);
        reused.push(start.elapsed().as_secs_f64() * 1000.0);
    }

    let a = med(&mut per_request);
    let b = med(&mut reused);
    println!("\n{}", "=".repeat(70));
    println!("fresh context per request (ships today): p50 {a:.0}ms");
    println!("reused context, KV cleared:              p50 {b:.0}ms");
    println!(
        "\nsaving: {:.0}ms ({:.0}% of prefill-path TTFT)",
        a - b,
        100.0 * (a - b) / a
    );
}
