//! whisper.cpp backend: the multilingual finalizer, and the only recognizer
//! that runs off macOS.
//!
//! ## Why whisper.cpp at all
//!
//! Parakeet v2 is English-only. Whisper covers ~99 languages, is MIT (code
//! and weights), runs Metal + Core ML on Apple Silicon, and has the best
//! cross-platform C API of any engine (research §1.1). It is the
//! multilingual fallback (R-04), not the primary: its 30s encoder window
//! makes true streaming impossible, so it only ever fills the finalizer slot
//! in the two-stage pipeline.
//!
//! It is also what makes Windows and Linux possible at all. Apple's
//! `SpeechTranscriber` is macOS-only, so without this every other platform
//! captures audio and then has nothing to transcribe with.
//!
//! ## Building
//!
//! Behind the `whisper` feature, off by default: `whisper-rs` compiles
//! whisper.cpp from source and needs cmake plus a C++ toolchain. Turning that
//! on by default would break `cargo build` for anyone who only wants to
//! typecheck the workspace.
//!
//! ```text
//! cargo build -p asr --features whisper
//! ```
//!
//! ## Facts a maintainer needs
//!
//! - **Weights (ggml format, MIT):**
//!   <https://huggingface.co/ggerganov/whisper.cpp> — `ggml-base.en.bin`
//!   142MiB / ~388MB RAM, `ggml-small.en.bin` 466MiB / ~852MB RAM,
//!   `ggml-large-v3-turbo.bin` ~1.6GiB / ~2GB RAM. `whisper-base.en` is
//!   already in [`crate::models::registry`].
//! - **Expected RTF:** on M-series with Metal, small runs 30x+ real time,
//!   large-v3-turbo roughly 8-15x. A 5s utterance with small.en finalizes in
//!   ~150-300ms, borderline for the 200ms budget; base.en is safely inside it
//!   at lower accuracy (~8.6% vs ~7.4% WER class).
//! - **WER:** large-v3 ~7.4% Open-ASR avg, small ~8.6%, base ~10%.
//! - **Streaming:** pseudo only (re-decode sliding window). Do not put this
//!   backend in the streamer slot; that is Moonshine/Zipformer territory.

use crate::{Partial, Recognizer, Transcript};

/// Sample rate whisper.cpp requires. Audio arriving at any other rate is a
/// caller bug rather than something to resample here: the capture layer
/// already normalises, and silently resampling would hide a mismatch that
/// shows up as garbled recognition.
pub const REQUIRED_SAMPLE_RATE: u32 = 16_000;

/// Utterances longer than this are truncated at finalize.
///
/// Whisper's encoder takes a fixed 30-second window. Feeding more does not
/// error, it silently keeps only part, so the truncation is done here where
/// it can be explained rather than left as mysterious missing words. The
/// hot-mic timeout closes capture well before this in normal use.
pub const MAX_UTTERANCE_SECS: f32 = 30.0;

/// Number of samples in whisper's encoder window.
///
/// A function rather than an inline expression at the one call site so the
/// truncation point can be asserted without a model, which is the only part
/// of this backend testable in the default build.
pub fn window_samples() -> usize {
    (MAX_UTTERANCE_SECS * REQUIRED_SAMPLE_RATE as f32) as usize
}

/// Route whisper.cpp's own logging through the `log` crate instead of stderr.
///
/// The `set_print_*` params only govern transcription output. The library
/// separately logs model load progress, decoder scores and seek positions
/// from its C layer, which those params cannot reach: a five-second utterance
/// printed roughly forty lines. In a terminal that buries our diagnostics,
/// and in a menu-bar app launched from Finder it goes nowhere while still
/// costing the formatting work.
///
/// `install_logging_hooks` sends them to `log` instead, where a host that
/// wants them can subscribe and a host that does not simply drops them.
///
/// Installed once: the hook is global to the C library, and the crate
/// documents repeat calls as no-ops.
#[cfg(feature = "whisper")]
fn silence_whisper_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(whisper_rs::install_logging_hooks);
}

/// Buffers an utterance and transcribes it in one pass at finalize.
///
/// One pass rather than incremental: whisper re-decodes its whole window on
/// every call, so "streaming" means running the full model repeatedly and
/// throwing most of the work away. The two-stage pipeline exists precisely so
/// a real streaming model can fill the partial slot instead.
pub struct WhisperCppRecognizer {
    buffered: Vec<f32>,
    #[cfg(feature = "whisper")]
    ctx: whisper_rs::WhisperContext,
    #[cfg(not(feature = "whisper"))]
    _model_path: std::path::PathBuf,
}

impl WhisperCppRecognizer {
    /// `model_path` points at a ggml `.bin` fetched via
    /// [`crate::models::fetch`].
    pub fn new(model_path: &std::path::Path) -> anyhow::Result<Self> {
        #[cfg(feature = "whisper")]
        {
            // Checked here rather than left to the C layer: whisper.cpp
            // reports a missing file as a generic init failure, and "model
            // not found at <path>" is the difference between a user fixing it
            // in a second and filing a bug.
            if !model_path.exists() {
                anyhow::bail!(
                    "whisper model not found at {}: fetch it with the model manager \
                     (see crates/asr/src/models.rs) or point at a ggml .bin",
                    model_path.display()
                );
            }
            silence_whisper_logging();
            let ctx = whisper_rs::WhisperContext::new_with_params(
                model_path,
                whisper_rs::WhisperContextParameters::default(),
            )
            .map_err(|e| anyhow::anyhow!("whisper failed to load {}: {e}", model_path.display()))?;
            Ok(Self {
                buffered: Vec::new(),
                ctx,
            })
        }
        #[cfg(not(feature = "whisper"))]
        {
            Ok(Self {
                buffered: Vec::new(),
                _model_path: model_path.to_path_buf(),
            })
        }
    }
}

impl Recognizer for WhisperCppRecognizer {
    fn feed(&mut self, samples: &[f32]) -> Option<Partial> {
        self.buffered.extend_from_slice(samples);
        // No partials by design. Emitting one would mean re-running the whole
        // model per chunk, which costs more than the utterance is worth and
        // still lags a real streaming recognizer.
        None
    }

    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        let audio: Vec<f32> = std::mem::take(&mut self.buffered);
        let audio_secs = audio.len() as f32 / REQUIRED_SAMPLE_RATE as f32;

        #[cfg(not(feature = "whisper"))]
        {
            let _ = (audio, audio_secs);
            anyhow::bail!(
                "this build has no whisper backend: rebuild with \
                 `--features whisper` (needs cmake and a C++ toolchain)"
            )
        }

        #[cfg(feature = "whisper")]
        {
            if audio.is_empty() {
                return Ok(Transcript {
                    text: String::new(),
                    words: Vec::new(),
                    audio_secs: 0.0,
                });
            }

            // Truncate to the encoder window, keeping the START. The
            // alternative, keeping the end, would silently drop the beginning
            // of a long sentence, and a transcript that begins mid-thought
            // reads as a recognition failure rather than a length limit.
            let max = window_samples();
            let audio = if audio.len() > max {
                eprintln!(
                    "asr: utterance is {audio_secs:.1}s, longer than whisper's \
                     {MAX_UTTERANCE_SECS:.0}s window; transcribing the first \
                     {MAX_UTTERANCE_SECS:.0}s"
                );
                &audio[..max]
            } else {
                &audio[..]
            };

            let mut params =
                whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
            // Quiet: the C library prints progress to stdout/stderr
            // otherwise, which in a menu-bar app goes nowhere useful and in a
            // terminal buries our own diagnostics.
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            // Single segment: this is one utterance, not a recording to be
            // split into subtitle lines.
            params.set_single_segment(true);
            // Threads: leave one core for capture and the UI, so a long
            // finalize cannot starve the audio callback and drop samples.
            let threads = std::thread::available_parallelism()
                .map(|n| (n.get().saturating_sub(1)).max(1))
                .unwrap_or(1);
            params.set_n_threads(threads as i32);

            let mut state = self
                .ctx
                .create_state()
                .map_err(|e| anyhow::anyhow!("whisper could not create state: {e}"))?;
            state
                .full(params, audio)
                .map_err(|e| anyhow::anyhow!("whisper transcription failed: {e}"))?;

            let n = state.full_n_segments();
            let mut text = String::new();
            for i in 0..n {
                // `to_str_lossy` rather than `to_str`: whisper can emit bytes
                // that are not valid UTF-8 mid-token, and losing a character
                // is better than losing the sentence.
                if let Some(seg) = state.get_segment(i) {
                    if let Ok(part) = seg.to_str_lossy() {
                        text.push_str(&part);
                    }
                }
            }

            Ok(Transcript {
                // Whisper pads with a leading space and often a trailing one.
                // Trimming here rather than downstream because every consumer
                // would otherwise have to know that, and one forgetting is a
                // stray space in the user's document.
                text: text.trim().to_string(),
                // Word timings need token-level timestamps, which cost
                // another decode pass. Empty is the documented "not
                // available" value, and no consumer requires them.
                words: Vec::new(),
                audio_secs,
            })
        }
    }

    fn name(&self) -> &'static str {
        "whisper.cpp"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Buffering is the half that needs no model, so it is the half that can
    /// be tested in the default configuration, which is the one CI builds.
    ///
    /// Constructing a recognizer needs a real model file with the feature on,
    /// so this exercises the arithmetic directly rather than faking a context.
    #[test]
    fn buffered_secs_tracks_the_sample_rate() {
        // 1600 samples at 16kHz is 0.1s. Getting this wrong would misreport
        // every utterance length and silently change where the 30s window
        // truncates.
        let secs = |n: usize| n as f32 / REQUIRED_SAMPLE_RATE as f32;
        assert!((secs(1600) - 0.1).abs() < 1e-6);
        assert!((secs(16_000) - 1.0).abs() < 1e-6);

        // The truncation point the finalizer uses. Wrong by a factor of the
        // sample rate and a 30s cap becomes a 0.03s one, which would look
        // like catastrophic recognition failure rather than a bad constant.
        assert_eq!(window_samples(), 480_000, "30s at 16kHz");
    }

    /// A missing model must say so with the path, not fail generically.
    #[test]
    fn a_missing_model_names_the_path() {
        let err = WhisperCppRecognizer::new(std::path::Path::new("/nonexistent/model.bin"));
        #[cfg(feature = "whisper")]
        {
            // `unwrap_err` needs Debug on the Ok type, which a whisper
            // context cannot provide; match instead.
            let msg = match err {
                Ok(_) => panic!("a nonexistent model must not load"),
                Err(e) => e.to_string(),
            };
            assert!(msg.contains("/nonexistent/model.bin"), "got: {msg}");
        }
        // Without the feature, construction succeeds and finalize is what
        // explains the situation; that contract is asserted below.
        #[cfg(not(feature = "whisper"))]
        assert!(err.is_ok());
    }

    /// Without the feature, the failure must name the fix.
    #[cfg(not(feature = "whisper"))]
    #[test]
    fn a_build_without_the_feature_says_how_to_get_one() {
        let mut r = WhisperCppRecognizer::new(std::path::Path::new("/any")).unwrap();
        let msg = r.finalize().unwrap_err().to_string();
        assert!(msg.contains("--features whisper"), "got: {msg}");
    }
}
