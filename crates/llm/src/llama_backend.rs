//! Real backend: llama.cpp via the `llama-cpp-2` bindings, GGUF models.
//!
//! Why llama.cpp and not MLX: MLX is faster on Apple Silicon for some
//! workloads and mlx-lm is excellent, but it is Python-first (the Swift/C
//! APIs lag) and macOS-only, while this workspace targets Windows and Linux
//! later. llama.cpp gives one Rust-bindable C library with Metal on macOS
//! and CUDA/Vulkan/CPU elsewhere, matching the decision already made for
//! whisper.cpp on the ASR side. The `metal` cargo feature is enabled in this
//! crate's manifest, so all layers offload to the GPU on Apple Silicon.
//! docs/llm.md records the trade-off in full.
//!
//! This module is behind the `llama` feature because it compiles llama.cpp
//! from source and is useless without a downloaded model. No test in the
//! workspace requires it; the `bench_freeform` example is the manual harness.

use std::num::NonZeroU32;
use std::path::Path;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;

use crate::prompt;
use crate::{TransformError, Transformer};

/// Hard cap on generated tokens. The guardrails bound output length after
/// the fact; this bounds *cost* during generation, because a looping model
/// should stop burning the GPU long before the length-ratio check sees it.
const MAX_OUTPUT_TOKENS: usize = 1024;

/// A resident llama.cpp model. Construct once at startup (or on first
/// freeform request) and keep alive: model load is seconds, while a warm
/// request is tens of milliseconds to first token.
pub struct LlamaTransformer {
    backend: LlamaBackend,
    model: LlamaModel,
    n_ctx: NonZeroU32,
}

impl LlamaTransformer {
    /// Load a GGUF model with full GPU offload. `n_ctx` bounds the prompt
    /// plus output; 4096 fits any realistic dictation field slice.
    pub fn load(gguf_path: &Path) -> Result<Self, TransformError> {
        let backend = LlamaBackend::init().map_err(|e| TransformError::Backend(e.to_string()))?;
        // 1_000_000 gpu layers = "all of them"; llama.cpp clamps internally.
        let params = LlamaModelParams::default().with_n_gpu_layers(1_000_000);
        let model = LlamaModel::load_from_file(&backend, gguf_path, &params)
            .map_err(|e| TransformError::Backend(e.to_string()))?;
        Ok(Self {
            backend,
            model,
            n_ctx: NonZeroU32::new(4096).unwrap(),
        })
    }

    /// Render the chat-templated prompt for one request. Qwen3 supports the
    /// `/no_think` soft switch; we put it in the system prompt so the model
    /// skips its thinking block, which would otherwise cost hundreds of
    /// tokens of latency before the first visible character.
    fn render_prompt(&self, original: &str, instruction: &str) -> Result<String, TransformError> {
        let template = self
            .model
            .chat_template(None)
            .map_err(|e| TransformError::Backend(format!("no chat template: {e}")))?;
        let messages = vec![
            LlamaChatMessage::new(
                "system".into(),
                format!("{} /no_think", prompt::SYSTEM_PROMPT),
            )
            .map_err(|e| TransformError::Backend(e.to_string()))?,
            LlamaChatMessage::new("user".into(), prompt::user_prompt(original, instruction))
                .map_err(|e| TransformError::Backend(e.to_string()))?,
        ];
        self.model
            .apply_chat_template(&template, &messages, true)
            .map_err(|e| TransformError::Backend(e.to_string()))
    }
}

impl Transformer for LlamaTransformer {
    fn transform_streaming(
        &mut self,
        original: &str,
        instruction: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<String, TransformError> {
        let prompt_text = self.render_prompt(original, instruction)?;

        let ctx_params = LlamaContextParams::default().with_n_ctx(Some(self.n_ctx));
        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| TransformError::Backend(e.to_string()))?;

        let tokens = self
            .model
            .str_to_token(&prompt_text, AddBos::Never)
            .map_err(|e| TransformError::Backend(e.to_string()))?;
        let n_prompt = tokens.len();
        if n_prompt as u32 + 8 >= self.n_ctx.get() {
            return Err(TransformError::Backend(format!(
                "prompt of {n_prompt} tokens does not fit context {}",
                self.n_ctx
            )));
        }

        // Prefill the whole prompt in one batch; only the last position
        // needs logits.
        let mut batch = LlamaBatch::new(self.n_ctx.get() as usize, 1);
        let last = n_prompt - 1;
        for (i, tok) in tokens.iter().enumerate() {
            batch
                .add(*tok, i as i32, &[0], i == last)
                .map_err(|e| TransformError::Backend(e.to_string()))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| TransformError::Backend(e.to_string()))?;

        // Low temperature on purpose: this is an editing task, not creative
        // writing. min_p trims the tail so a 1.7B model does not wander;
        // dist (seeded from entropy) keeps "try again" able to differ.
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(0.3),
            LlamaSampler::dist(rand_seed()),
        ]);

        let mut out = String::new();
        let mut pos = n_prompt as i32;
        let max_pos = (self.n_ctx.get() as i32 - 1).min(pos + MAX_OUTPUT_TOKENS as i32);
        loop {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if self.model.is_eog_token(token) || pos >= max_pos {
                break;
            }
            // `Special::Tokenize` keeps special tokens out of the text; a
            // leaked <|im_end|> in user-visible output is a formatting bug.
            let piece = self
                .model
                .token_to_str(token, Special::Tokenize)
                .unwrap_or_default();
            if !piece.is_empty() {
                on_token(&piece);
                out.push_str(&piece);
            }
            batch.clear();
            batch
                .add(token, pos, &[0], true)
                .map_err(|e| TransformError::Backend(e.to_string()))?;
            ctx.decode(&mut batch)
                .map_err(|e| TransformError::Backend(e.to_string()))?;
            pos += 1;
        }
        Ok(out)
    }
}

/// Seed from the OS clock: good enough for sampling variety, no rand dep.
fn rand_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}
