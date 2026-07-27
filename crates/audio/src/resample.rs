//! Downmix and resample arbitrary capture formats to 16kHz mono f32.
//!
//! Why linear interpolation and not a windowed-sinc library: the consumer is
//! an ASR model, not a human ear. Speech models are trained on augmented,
//! band-limited, codec-mangled audio; the aliasing image from linear
//! interpolation at 48k→16k sits far below the noise floors those models
//! shrug off, and it costs two multiplies per sample with no dependency.
//! If a future eval shows measurable WER impact we can swap in `rubato`
//! behind the same function signature.

/// Average interleaved multi-channel samples down to mono.
///
/// Averaging (not "take channel 0") because some devices, notably certain
/// USB interfaces, deliver the microphone on the second channel.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    assert!(channels > 0, "channels must be nonzero");
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Streaming linear resampler from `from_rate` to `to_rate`.
///
/// Stateful so it can be fed arbitrary chunk sizes from the audio callback
/// without losing the fractional read position between calls.
pub struct Resampler {
    ratio: f64,
    /// Fractional position into the *next* input chunk, carried across calls.
    pos: f64,
    /// Last sample of the previous chunk, needed to interpolate across the
    /// chunk boundary.
    carry: Option<f32>,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        assert!(from_rate > 0 && to_rate > 0);
        Self {
            ratio: from_rate as f64 / to_rate as f64,
            pos: 0.0,
            carry: None,
        }
    }

    /// True when input and output rates match (process becomes a copy).
    pub fn is_identity(&self) -> bool {
        self.ratio == 1.0
    }

    /// Resample one chunk, returning the output samples it produces.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.is_identity() {
            return input.to_vec();
        }
        if input.is_empty() {
            return Vec::new();
        }
        // Build a virtual input of [carry, input...] so interpolation spans
        // chunk boundaries. Index -1 refers to the carry sample.
        let get = |i: i64| -> f32 {
            if i < 0 {
                self.carry.unwrap_or(input[0])
            } else {
                input[i as usize]
            }
        };
        let mut out = Vec::with_capacity((input.len() as f64 / self.ratio) as usize + 2);
        // self.pos is measured relative to input[0]; it may start negative
        // (interpolating between carry and input[0]).
        let mut pos = self.pos - if self.carry.is_some() { 1.0 } else { 0.0 };
        let last = input.len() as f64 - 1.0;
        while pos <= last {
            let i = pos.floor();
            let frac = (pos - i) as f32;
            let a = get(i as i64);
            let b = if i + 1.0 <= last {
                get(i as i64 + 1)
            } else {
                a
            };
            out.push(a + (b - a) * frac);
            pos += self.ratio;
        }
        // Carry state into the next call.
        self.pos = pos - last;
        self.carry = Some(input[input.len() - 1]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        let stereo = [1.0, 3.0, 2.0, 4.0];
        assert_eq!(downmix(&stereo, 2), vec![2.0, 3.0]);
    }

    #[test]
    fn identity_rate_is_passthrough() {
        let mut r = Resampler::new(16_000, 16_000);
        assert_eq!(r.process(&[0.1, 0.2]), vec![0.1, 0.2]);
    }

    #[test]
    fn halves_sample_count_at_2x_downsample() {
        let mut r = Resampler::new(32_000, 16_000);
        let input: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let out = r.process(&input);
        // Ramp input must stay a ramp after linear interpolation.
        assert!((out.len() as i64 - 500).abs() <= 1, "got {}", out.len());
        for pair in out.windows(2) {
            assert!((pair[1] - pair[0] - 2.0).abs() < 1e-3);
        }
    }

    #[test]
    fn chunked_equals_whole_at_48k_to_16k() {
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut whole = Resampler::new(48_000, 16_000);
        let expected = whole.process(&input);

        let mut chunked = Resampler::new(48_000, 16_000);
        let mut got = Vec::new();
        // Awkward chunk sizes to exercise the carry logic.
        for chunk in input.chunks(377) {
            got.extend(chunked.process(chunk));
        }
        assert_eq!(expected.len(), got.len());
        for (a, b) in expected.iter().zip(&got) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn output_duration_tracks_input_duration() {
        // 1 second of 44.1kHz must become ~1 second of 16kHz.
        let input = vec![0.5_f32; 44_100];
        let mut r = Resampler::new(44_100, 16_000);
        let out = r.process(&input);
        assert!((out.len() as i64 - 16_000).abs() <= 2, "got {}", out.len());
    }
}
