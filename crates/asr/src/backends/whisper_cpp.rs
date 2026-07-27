//! whisper.cpp backend: stub, wired for the finalizer/fallback role.
//!
//! ## Why whisper.cpp at all
//!
//! Parakeet v2 is English-only. Whisper covers ~99 languages, is MIT
//! (code and weights), runs Metal + Core ML on Apple Silicon, and has the
//! best cross-platform C API of any engine (research §1.1). It is the
//! multilingual fallback (R-04), not the primary: its 30s encoder window
//! makes true streaming impossible, so it only ever fills the finalizer
//! slot in the two-stage pipeline.
//!
//! ## Integration plan (why this is a stub today)
//!
//! The clean route is the `whisper-rs` crate (bindings to whisper.cpp,
//! builds the C library from source, needs cmake). Like ONNX Runtime for
//! Parakeet, that native-build dependency deserves its own change with CI
//! coverage. The trait seam confines the landing to this file.
//!
//! ## Facts a future implementer needs
//!
//! - **Weights (ggml format, MIT):**
//!   <https://huggingface.co/ggerganov/whisper.cpp> — `ggml-base.en.bin`
//!   142MiB / ~388MB RAM, `ggml-small.en.bin` 466MiB / ~852MB RAM,
//!   `ggml-large-v3-turbo.bin` ~1.6GiB / ~2GB RAM. `whisper-base.en` is
//!   already in [`crate::models::registry`].
//! - **Expected RTF:** on M-series with Metal, small runs 30x+ real time,
//!   large-v3-turbo roughly 8-15x. A 5s utterance with small.en finalizes
//!   in ~150-300ms, borderline for the 200ms budget; base.en is safely
//!   inside it at lower accuracy (~8.6% vs ~7.4% WER class).
//! - **WER:** large-v3 ~7.4% Open-ASR avg, small ~8.6%, base ~10%.
//! - **Streaming:** pseudo only (re-decode sliding window). Do not put this
//!   backend in the streamer slot; that is Moonshine/Zipformer territory.

use crate::{Partial, Recognizer, Transcript};

/// Stub: buffers audio, fails loudly at finalize (same contract shape as
/// the Parakeet stub, see there for rationale).
pub struct WhisperCppRecognizer {
    buffered: Vec<f32>,
}

impl WhisperCppRecognizer {
    /// `model_path` points at a ggml `.bin` fetched via
    /// [`crate::models::fetch`].
    pub fn new(_model_path: &std::path::Path) -> anyhow::Result<Self> {
        Ok(Self {
            buffered: Vec::new(),
        })
    }
}

impl Recognizer for WhisperCppRecognizer {
    fn feed(&mut self, samples: &[f32]) -> Option<Partial> {
        self.buffered.extend_from_slice(samples);
        None
    }

    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        self.buffered.clear();
        anyhow::bail!(
            "whisper.cpp backend not yet implemented: needs whisper-rs integration \
             (see crates/asr/src/backends/whisper_cpp.rs for the integration plan)"
        )
    }

    fn name(&self) -> &'static str {
        "whisper.cpp"
    }
}
