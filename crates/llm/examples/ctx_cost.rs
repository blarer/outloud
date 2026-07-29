//! Is the per-request `new_context` call actually costing latency?
//!
//! `ttft_breakdown` measured warm TTFT at ~185ms but could not attribute it,
//! because `transform_streaming` builds a fresh `LlamaContext` inside the
//! measured region. This times the two pieces separately using the same
//! llama-cpp-2 API the backend uses, so the answer is a measurement rather
//! than an inference from reading the code.
//!
//! Run: `cargo run -p llm --features llama --release --example ctx_cost`

use std::num::NonZeroU32;
use std::time::Instant;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use llm::{models, prompt};

fn main() {
    let spec = &models::registry()[0];
    let path = models::fetch(spec, &models::default_cache_dir(), |_| {}).expect("fetch");

    let backend = LlamaBackend::init().expect("backend");
    let params = LlamaModelParams::default().with_n_gpu_layers(1_000_000);
    let model = LlamaModel::load_from_file(&backend, &path, &params).expect("model");
    let n_ctx = NonZeroU32::new(4096).unwrap();

    let template = model.chat_template(None).expect("template");
    let messages = vec![
        LlamaChatMessage::new(
            "system".into(),
            format!("{} /no_think", prompt::SYSTEM_PROMPT),
        )
        .unwrap(),
        LlamaChatMessage::new(
            "user".into(),
            prompt::user_prompt(
                "we should probably ship the thing today i think",
                "tighten this up",
            ),
        )
        .unwrap(),
    ];
    let prompt_text = model
        .apply_chat_template(&template, &messages, true)
        .expect("render");
    let tokens = model
        .str_to_token(&prompt_text, AddBos::Never)
        .expect("tok");
    println!("prompt tokens: {}", tokens.len());

    let mut ctx_times = Vec::new();
    let mut prefill_times = Vec::new();
    let mut first_tok_times = Vec::new();

    for _ in 0..8 {
        let t0 = Instant::now();
        let mut ctx = model
            .new_context(
                &backend,
                LlamaContextParams::default().with_n_ctx(Some(n_ctx)),
            )
            .expect("ctx");
        ctx_times.push(t0.elapsed().as_secs_f64() * 1000.0);

        let t1 = Instant::now();
        let mut batch = LlamaBatch::new(n_ctx.get() as usize, 1);
        let last = tokens.len() - 1;
        for (i, tok) in tokens.iter().enumerate() {
            batch.add(*tok, i as i32, &[0], i == last).unwrap();
        }
        ctx.decode(&mut batch).expect("decode");
        prefill_times.push(t1.elapsed().as_secs_f64() * 1000.0);

        // The backend's first `on_token` fires after the first sampled
        // token, so anything between prefill and that callback belongs in
        // TTFT too. Qwen3 emits an empty `<think>\n\n</think>` pair even
        // with /no_think, and those tokens are sampled and decoded before
        // any real content, which is a plausible home for the remainder.
        let t2 = Instant::now();
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(0.3),
            LlamaSampler::dist(0),
        ]);
        let tok = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(tok);
        first_tok_times.push(t2.elapsed().as_secs_f64() * 1000.0);
    }

    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let c = med(&mut ctx_times);
    let p = med(&mut prefill_times);
    let f = med(&mut first_tok_times);
    println!("new_context()     p50 {c:.0}ms");
    println!("prefill decode    p50 {p:.0}ms");
    println!("first token sample p50 {f:.1}ms");
    println!(
        "\nsum of measured stages: {:.0}ms. Context construction is {:.0}% of it, \
and it is pure per-request overhead: the model and backend are already \
resident, so a reused context would remove it entirely.",
        c + p + f,
        100.0 * c / (c + p + f)
    );
}
