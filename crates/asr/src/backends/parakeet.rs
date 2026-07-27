//! Parakeet TDT 0.6b v2 backend: stub, wired for the finalizer role.
//!
//! ## Why this model
//!
//! Parakeet TDT 0.6b v2 tops the Open ASR Leaderboard among open models at
//! **6.05% average WER** with native punctuation, capitalization, and word
//! timestamps (research §1.4). It is the roadmap's chosen finalizer (R-03):
//! a 5s utterance must finalize in ≤200ms on an M1 Pro.
//!
//! ## Integration plan (why this is a stub today)
//!
//! The ONNX path runs through `sherpa-onnx` (Apache-2.0) or a direct `ort`
//! integration against the community export. Both pull ONNX Runtime, a
//! ~50MB native dependency with its own build story, which deserves its own
//! change with benchmarks rather than riding along here. The trait seam
//! means landing it later touches only this file.
//!
//! ## Facts a future implementer needs
//!
//! - **Weights:** <https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2>
//!   (CC-BY-4.0, commercial OK with attribution; NOT MIT, ship as a
//!   download, never vendored). ONNX export:
//!   <https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx>
//!   (`parakeet-tdt-0.6b-v2-onnx` in [`crate::models::registry`]).
//! - **Expected RTF:** RTFx ~30-60 on Apple Silicon via MLX; the int8 ONNX
//!   on CPU lands roughly RTFx 5-15 on an M1 Pro class machine, still
//!   comfortably inside the 200ms budget for a 5s utterance.
//! - **Memory:** ~600MB int8 ONNX on disk, ~1.5-2GB resident fp16.
//! - **Streaming:** batch-first here. Feed buffers; finalize transcribes.
//!   NeMo's cache-aware streaming variants exist if this ever needs to be
//!   the streamer, but that is not its job in the two-stage design.

use crate::{Partial, Recognizer, Transcript};

/// Stub: buffers audio, fails loudly at finalize. Exists so the pipeline,
/// settings UI, and model manager can be built and tested against the real
/// shape of the backend before ONNX Runtime lands.
pub struct ParakeetRecognizer {
    buffered: Vec<f32>,
}

impl ParakeetRecognizer {
    /// `model_path` will point at the ONNX export fetched via
    /// [`crate::models::fetch`]. Accepted now so call sites are already
    /// correct when inference lands.
    pub fn new(_model_path: &std::path::Path) -> anyhow::Result<Self> {
        Ok(Self {
            buffered: Vec::new(),
        })
    }
}

impl Recognizer for ParakeetRecognizer {
    fn feed(&mut self, samples: &[f32]) -> Option<Partial> {
        // Batch engine: no partials, just accumulate for finalize.
        self.buffered.extend_from_slice(samples);
        None
    }

    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        self.buffered.clear();
        anyhow::bail!(
            "parakeet backend not yet implemented: needs ONNX Runtime integration \
             (see crates/asr/src/backends/parakeet.rs for the integration plan)"
        )
    }

    fn name(&self) -> &'static str {
        "parakeet-tdt-0.6b-v2"
    }
}
