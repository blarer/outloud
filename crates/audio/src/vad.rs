//! Voice activity detection behind a swappable trait.
//!
//! The segmenter never asks "is this Silero or an energy gate", it asks for
//! a speech probability per 30ms frame. That keeps the state machine
//! testable with synthetic audio in CI (energy VAD on sine vs silence is
//! deterministic) and leaves the door open for semantic VAD (Kyutai-style
//! end-of-turn prediction) without touching the segmenter.

use crate::FRAME_SAMPLES;

/// Per-frame speech probability source.
pub trait VoiceDetector: Send {
    /// Probability in `[0, 1]` that `frame` (30ms of 16kHz mono) is speech.
    ///
    /// Implementations may keep internal state (Silero is an RNN), so this
    /// takes `&mut self`.
    fn speech_probability(&mut self, frame: &[f32]) -> f32;

    /// Reset internal state between utterances / device switches.
    fn reset(&mut self) {}
}

/// RMS-energy gate with a soft knee.
///
/// Not a serious VAD (it cannot tell speech from a hairdryer), but it is
/// dependency-free and fully deterministic, which is exactly what the
/// segmenter tests need: sine waves read as speech, silence reads as
/// silence, no model download in CI.
pub struct EnergyVad {
    /// RMS at which probability reaches 0.5. Default is tuned so normalized
    /// synthetic speech (amplitude ~0.1+) is confidently speech and typical
    /// room-noise floors (~0.001 RMS) are confidently silence.
    knee: f32,
}

impl EnergyVad {
    pub fn new() -> Self {
        Self { knee: 0.01 }
    }

    pub fn with_knee(knee: f32) -> Self {
        assert!(knee > 0.0);
        Self { knee }
    }
}

impl Default for EnergyVad {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceDetector for EnergyVad {
    fn speech_probability(&mut self, frame: &[f32]) -> f32 {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        // Squared-ratio soft knee: 0 at silence, 0.5 exactly at the knee,
        // asymptotically 1. Monotonic and cheap; no tuning cliffs.
        let r2 = (rms / self.knee).powi(2);
        r2 / (1.0 + r2)
    }
}

/// Silero VAD v5 via the `vad-rs` crate (ONNX Runtime).
///
/// ~2MB model, <1ms per frame on one CPU thread, trained on 6000+ languages.
/// This is the production detector; `EnergyVad` exists for tests and as a
/// last-resort fallback when the model file is missing.
#[cfg(feature = "silero")]
pub struct SileroVad {
    inner: vad_rs::Vad,
}

#[cfg(feature = "silero")]
impl SileroVad {
    /// Load the Silero ONNX model from `model_path`. Obtain the file via
    /// `asr::models` (`silero-vad` entry) or from
    /// <https://github.com/snakers4/silero-vad>.
    pub fn new(model_path: &std::path::Path) -> anyhow::Result<Self> {
        let inner = vad_rs::Vad::new(model_path, crate::SAMPLE_RATE as usize)
            .map_err(|e| anyhow::anyhow!("loading silero vad: {e}"))?;
        Ok(Self { inner })
    }
}

#[cfg(feature = "silero")]
impl VoiceDetector for SileroVad {
    fn speech_probability(&mut self, frame: &[f32]) -> f32 {
        match self.inner.compute(frame) {
            Ok(result) => result.prob,
            // On inference error, claim speech: the recognizer wasting cycles
            // on noise is recoverable, cutting off a user mid-sentence is not.
            Err(_) => 1.0,
        }
    }
}

/// Convenience: split a sample stream into VAD-sized frames, discarding a
/// final partial frame (the caller keeps its own tail buffer).
pub fn frames(samples: &[f32]) -> impl Iterator<Item = &[f32]> {
    samples.chunks_exact(FRAME_SAMPLES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME_SAMPLES;

    fn sine(amplitude: f32) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|i| amplitude * (i as f32 * 0.13).sin())
            .collect()
    }

    #[test]
    fn silence_is_not_speech() {
        let mut vad = EnergyVad::new();
        assert!(vad.speech_probability(&vec![0.0; FRAME_SAMPLES]) < 0.3);
    }

    #[test]
    fn loud_sine_is_speech() {
        let mut vad = EnergyVad::new();
        assert!(vad.speech_probability(&sine(0.3)) > 0.7);
    }

    #[test]
    fn probability_is_monotonic_in_level() {
        let mut vad = EnergyVad::new();
        let quiet = vad.speech_probability(&sine(0.001));
        let mid = vad.speech_probability(&sine(0.05));
        let loud = vad.speech_probability(&sine(0.5));
        assert!(quiet < mid && mid < loud);
    }
}
