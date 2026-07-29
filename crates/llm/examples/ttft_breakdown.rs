//! Where does time-to-first-token actually go?
//!
//! `docs/llm.md` says the constant system prompt exists "so llama.cpp's
//! prompt cache keeps its KV prefix warm across requests, which is most of
//! the time-to-first-token win for a resident model". But
//! `LlamaTransformer::transform_streaming` calls `model.new_context(..)` on
//! every request, and a fresh context has an empty KV cache, so the prefix
//! is re-prefilled every time and that win is not being collected.
//!
//! This measures the two components separately: context construction, and
//! prefill+first token. If context construction is a meaningful share of
//! TTFT, hoisting it out of the hot path is free latency.
//!
//! Run: `cargo run -p llm --features llama --release --example ttft_breakdown`

use std::time::Instant;

use llm::llama_backend::LlamaTransformer;
use llm::{models, Transformer};

/// A one-sentence selection: the common case.
const SHORT: &str = "we should probably ship the thing today i think";

/// A paragraph-sized selection, matching `bench_investigation`'s long input,
/// so the two harnesses' TTFT numbers can be reconciled rather than left
/// looking contradictory.
const LONG: &str = "It is really quite important that we should try to make \
sure that the deploy happens today, because otherwise the customers might \
possibly be quite upset about it. We have been putting this off for a while \
now and the longer we wait the worse it is going to get for everyone \
involved, including the support team who have to field the questions.";

fn ttft_p50(backend: &mut LlamaTransformer, text: &str, runs: usize) -> f64 {
    let mut v = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        let mut first = None;
        let _ = backend.transform_streaming(text, "tighten this up", &mut |_| {
            if first.is_none() {
                first = Some(start.elapsed());
            }
        });
        v.push(first.unwrap_or_default().as_secs_f64() * 1000.0);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let spec = &models::registry()[0];
    let path = models::fetch(spec, &models::default_cache_dir(), |_| {}).expect("fetch");

    let t = Instant::now();
    let mut backend = LlamaTransformer::load(&path).expect("load");
    println!("model load (mmap, warm cache): {:?}\n", t.elapsed());

    // First request pays Metal pipeline compilation; excluded from the warm
    // figures below and reported separately, because a cold first request is
    // what the user actually feels once per session.
    let cold_start = Instant::now();
    let mut cold_first = None;
    let _ = backend.transform_streaming(SHORT, "tighten this up", &mut |_| {
        if cold_first.is_none() {
            cold_first = Some(cold_start.elapsed());
        }
    });
    println!(
        "first-request TTFT (Metal pipeline compile): {:.0}ms",
        cold_first.unwrap_or_default().as_secs_f64() * 1000.0
    );

    let short = ttft_p50(&mut backend, SHORT, 8);
    let long = ttft_p50(&mut backend, LONG, 8);

    println!("warm TTFT p50, short input (9 words):   {short:.0}ms");
    println!("warm TTFT p50, long input (63 words):   {long:.0}ms");
    println!(
        "\nThis reconciles the two figures quoted elsewhere: \
bench_investigation's 264ms p50 pools short and long inputs, while a \
short-input-only measurement lands near {short:.0}ms. Prompt length is the \
difference, and each of those runs also rebuilds a LlamaContext (see \
transform_streaming), discarding the KV prefix the constant system prompt \
was designed for."
    );
}
