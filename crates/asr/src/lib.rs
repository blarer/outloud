//! Speech recognition: the trait, the backends, and the two-stage pipeline.
//!
//! ## Why the trait looks the way it does
//!
//! The research (`../aqua-voice-research/02-local-asr-tech.md` §2, §5) is
//! unambiguous: perceived dictation latency is set by *partials*, not by
//! final accuracy. The winning architecture is two-stage: a fast streaming
//! recognizer paints ghost text within ~250ms of speech onset, and an
//! accurate finalizer re-transcribes the whole utterance once the VAD
//! endpoint fires. So the core trait is streaming-first:
//!
//! - [`Recognizer::feed`] accepts incremental 16kHz mono audio and may
//!   return an updated [`Partial`] at any time.
//! - [`Recognizer::finalize`] closes the utterance and returns the
//!   [`Transcript`] the caller should trust.
//!
//! Batch-only engines (whisper.cpp without its streaming hack, Parakeet TDT
//! in single-pass mode) implement the same trait by buffering in `feed` and
//! doing all work in `finalize`. That asymmetry is deliberate: it lets the
//! [`pipeline::TwoStagePipeline`] hold *any* pair of recognizers, streaming
//! or batch, without callers knowing which is which. Swapping backends is a
//! constructor change, never a call-site change.
//!
//! ## Latency budget (of the ~750ms the recognizer owns)
//!
//! | Stage | Budget | Basis |
//! |---|---|---|
//! | VAD + segmenter (crates/audio) | ~30ms/frame decision, 300ms hangover | research §5 |
//! | First partial after onset | ≤ 250ms | Moonshine 73-107ms + chunking |
//! | Partial cadence | ≤ 150ms | R-05 |
//! | Endpoint → final transcript | ≤ 200ms | Parakeet RTFx 30-60 on M-series |
//! | Whole utterance end → final | ≤ 550ms (300 hangover + 200 finalize + 50 slack) | leaves ~200ms for LLM formatting |
//!
//! ## Module map
//!
//! - [`types`]: `Partial`, `Transcript`, `Word` timing info.
//! - [`backends::mock`]: deterministic recognizer for tests and CI.
//! - [`backends::apple`]: Apple SpeechTranscriber via a Swift helper
//!   process; zero model download on macOS 26+.
//! - [`backends::parakeet`], [`backends::whisper_cpp`]: documented stubs
//!   with model URLs, RTF, and memory expectations.
//! - [`pipeline`]: the two-stage arbitration seam.
//! - [`models`]: download/verify/cache manager for backend model files.

pub mod backends;
pub mod models;
pub mod pipeline;
pub mod types;

pub use types::{Partial, Transcript, Word};

/// A speech recognizer, streaming-first.
///
/// Contract:
/// - `feed` is called with successive chunks of 16kHz mono f32 audio from
///   one utterance. It may return `None` (no update yet) or a fresh
///   `Partial` that *replaces* any previous partial, never appends to it.
///   Whole-hypothesis replacement is the only convention that survives
///   recognizers revising earlier words (all of them do).
/// - `finalize` consumes the utterance state and returns the transcript to
///   commit. After `finalize`, the recognizer is reset and reusable for the
///   next utterance. This mirrors push-to-talk and VAD endpoint semantics.
/// - Implementations must be `Send` so the pipeline can run them off the
///   audio thread.
pub trait Recognizer: Send {
    /// Feed incremental utterance audio; possibly get an updated hypothesis.
    fn feed(&mut self, samples: &[f32]) -> Option<Partial>;

    /// End the utterance and return the final transcript, resetting state.
    fn finalize(&mut self) -> anyhow::Result<Transcript>;

    /// Human-readable backend name, for logs and the settings UI.
    fn name(&self) -> &'static str;
}
