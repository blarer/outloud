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

/// The sensitivity steps offered to users, quietest threshold last.
///
/// Defined here, next to the mapping they index into, so the menu bar and
/// the `mic_level` diagnostic cannot drift apart from each other or from
/// the knee anchor. They already did once: a hardcoded copy of the steps
/// kept recommending a setting the menu no longer offered.
///
/// The ceiling is a measurement, not a preference: above roughly 75 the
/// gate scores a quiet room's noise floor as speech, so the top step sits
/// one increment below that. `crates/audio/tests/noise_floor.rs` derives
/// the bound and fails if these steps cross it.
pub const SENSITIVITY_STEPS: [(u8, &str); 4] = [
    (25, "Low (noisy room)"),
    (40, "Below normal"),
    (50, "Normal"),
    (70, "High (sitting back)"),
];

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
        // Must agree with sensitivity 50, the schema default, or the dial's
        // midpoint would silently differ from the out-of-box behaviour.
        Self::from_sensitivity(50)
    }

    pub fn with_knee(knee: f32) -> Self {
        assert!(knee > 0.0);
        Self { knee }
    }

    /// Build from the user-facing 1-100 sensitivity dial.
    ///
    /// Sensitivity is the inverse of the knee: turning it *up* means a
    /// quieter voice still counts as speech, which means a *lower* RMS
    /// threshold. Users think in "more sensitive", not "0.0009 RMS".
    ///
    /// The mapping is geometric because loudness is. A linear dial would
    /// spend most of its travel in a range no microphone produces and then
    /// cross the entire useful band between two adjacent steps. Each step
    /// here is a constant *ratio*, so the dial feels the same at both ends.
    ///
    /// The anchor is the *quiet tail* of speech, not its median. Anchoring
    /// at the median looks right and fails in practice: half of all speech
    /// frames sit below the median by definition, and the quiet ones are
    /// not noise, they are word endings, trailing syllables, and the start
    /// of a sentence before the voice is at full volume. Measured against
    /// a real utterance, a median anchor dropped "A quick" off the front of
    /// "A quick brown fox..." while reporting healthy levels.
    ///
    /// So 50 sits at roughly the 10th percentile of measured speech
    /// (~0.0009 RMS), comfortably below ordinary words and still an order
    /// of magnitude above a quiet room's floor (~0.0002).
    pub fn from_sensitivity(sensitivity: u8) -> Self {
        let s = sensitivity.clamp(1, 100) as f32;
        const QUIET_TAIL: f32 = 0.0009;
        /// Multiplier per step away from 50, giving a ~20x span each way:
        /// from shouting into a headset to speaking softly across a room.
        const PER_STEP: f32 = 1.0625;
        Self {
            knee: QUIET_TAIL * PER_STEP.powf(50.0 - s),
        }
    }

    /// The RMS at which speech probability reaches 0.5.
    pub fn knee(&self) -> f32 {
        self.knee
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

#[cfg(test)]
mod sensitivity_tests {
    use super::*;

    /// 30ms of steady tone at a chosen RMS, the shape the segmenter scores.
    fn tone(rms: f32) -> Vec<f32> {
        let amp = rms * std::f32::consts::SQRT_2;
        (0..480).map(|i| amp * (i as f32 * 0.3).sin()).collect()
    }

    #[test]
    fn dial_is_monotonic_in_the_direction_users_expect() {
        // Higher sensitivity must mean a lower threshold, every step of the
        // way. A non-monotonic dial is worse than no dial: turning it up
        // would sometimes make things worse and destroy trust in the knob.
        let mut prev = f32::MAX;
        for s in 1..=100u8 {
            let knee = EnergyVad::from_sensitivity(s).knee();
            assert!(knee < prev, "sensitivity {s} did not lower the threshold");
            prev = knee;
        }
    }

    #[test]
    fn default_matches_the_schema_midpoint() {
        // If these drift apart, a user who explicitly writes the default
        // value into their config gets different behaviour from a user who
        // omits it. Same intent, different result: a real bug.
        assert_eq!(
            EnergyVad::new().knee(),
            EnergyVad::from_sensitivity(50).knee()
        );
    }

    #[test]
    fn measured_quiet_speech_is_heard_when_turned_up() {
        // 0.0025 RMS is the measured median of ordinary speech at a normal
        // seated distance. The old fixed 0.01 knee scored it as silence,
        // which is the leaning-away failure this dial exists to fix.
        let mut old = EnergyVad::with_knee(0.01);
        assert!(
            old.speech_probability(&tone(0.0025)) < 0.5,
            "precondition: the old threshold really did miss this"
        );

        let mut now = EnergyVad::new();
        assert!(
            now.speech_probability(&tone(0.0025)) >= 0.5,
            "the new default must hear ordinary seated speech"
        );
    }

    #[test]
    fn leaning_far_back_is_heard_at_high_sensitivity() {
        // Roughly a quarter of the median: sat back from the desk.
        let mut vad = EnergyVad::from_sensitivity(85);
        assert!(vad.speech_probability(&tone(0.0006)) >= 0.5);
    }

    #[test]
    fn the_default_hears_the_quiet_tail_of_ordinary_speech() {
        // The regression this anchor exists to prevent. These are real
        // measured frame levels from a user's microphone; the quiet ones
        // are word endings and sentence onsets, not silence. With the knee
        // anchored at the median, 30% of them scored as silence and the
        // recognizer lost the first two words of the sentence.
        let measured = [
            0.00148, 0.00325, 0.00168, 0.00302, 0.00174, 0.00183, 0.00100, 0.00137, 0.00171,
            0.00247, 0.00203, 0.00234, 0.00231, 0.00216, 0.00128, 0.00167, 0.00198, 0.00233,
        ];
        let mut vad = EnergyVad::new();
        for rms in measured {
            assert!(
                vad.speech_probability(&tone(rms)) >= 0.5,
                "{rms:.5} RMS is quiet speech, not silence, and must be heard \
                 at the default setting"
            );
        }
    }

    #[test]
    fn a_quiet_room_is_still_silence_at_the_default() {
        // The dial must not be bought by transcribing the noise floor.
        // ~0.0002 RMS is a quiet room on a built-in microphone.
        let mut vad = EnergyVad::new();
        assert!(vad.speech_probability(&tone(0.0002)) < 0.5);
    }

    #[test]
    fn out_of_range_values_clamp_rather_than_panic() {
        // Config is a text file a human edits; 0 and 255 will happen.
        assert_eq!(
            EnergyVad::from_sensitivity(0).knee(),
            EnergyVad::from_sensitivity(1).knee()
        );
        assert_eq!(
            EnergyVad::from_sensitivity(255).knee(),
            EnergyVad::from_sensitivity(100).knee()
        );
    }
}
