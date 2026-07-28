//! Real-time level + spectrum feed for UI animation (the overlay's jaw).
//!
//! The overlay needs a signal that *looks like speech*: fast on the attack
//! so a plosive opens the jaw on the same frame, slow on the release so the
//! jaw does not chatter at the chunk rate. Raw per-chunk RMS (what the
//! overlay got before this module) fails both ways: it jumps to zero in any
//! 30ms pause and it aliases against the render loop's own cadence.
//!
//! Placement is the whole design: the meter runs in the *pipeline* task on
//! the chunks that are already flowing to the recognizer, never in the cpal
//! callback. It therefore adds zero work to the realtime audio thread and
//! zero latency to the recognition path; it is a passive observer of data
//! that was going to exist anyway.
//!
//! Publication is a single `AtomicU64` ([`MeterShared`]): the writer packs
//! level + four bands into one word, the render thread unpacks at whatever
//! frame rate it likes. No lock, no allocation, no channel, and a stalled
//! reader can at worst render a stale frame. Quantization is 12 bits per
//! channel, far below what any animation can show.
//!
//! Smoothing is asymmetric by agreement with the overlay: `level` carries
//! an attack/release envelope (it drives eye glow and the reduced-motion
//! gauge, where chatter is the failure), while `bands` are raw per-chunk
//! RMS because the skull's animator applies its own envelope and
//! double-smoothing kills the jaw's snap.
//!
//! The four bands are one-pole crossovers at 300 / 1000 / 3000 Hz. Speech
//! jaw energy lives in the low/mid bands; sibilance in the top. One-pole
//! IIR costs one multiply-add per sample per crossover, no FFT, no
//! dependency, and no block-size constraint.

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::SAMPLE_RATE;

/// Number of frequency bands published alongside the level.
pub const BANDS: usize = 4;

/// Crossover frequencies between adjacent bands, in Hz.
/// Bands: 0..300, 300..1k, 1k..3k, 3k..Nyquist.
const CROSSOVERS_HZ: [f32; BANDS - 1] = [300.0, 1000.0, 3000.0];

/// Envelope attack time constant. Short enough that a word onset registers
/// within one chunk; the eye reads anything under ~20ms as instant.
const ATTACK_S: f32 = 0.015;
/// Envelope release. Long enough to bridge the 30ms chunk cadence and
/// inter-syllable gaps without the jaw snapping shut, short enough that the
/// end of speech visibly lands.
const RELEASE_S: f32 = 0.12;

/// RMS at which the normalized output reaches 1.0. Matches the pipeline's
/// historical `rms / 0.1` mapping so the overall level is continuous with
/// what the overlay tuned against.
const FULL_SCALE_RMS: f32 = 0.1;

/// One published meter state. All values in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeterFrame {
    /// Overall smoothed loudness (attack/release-shaped normalized RMS).
    pub level: f32,
    /// Per-band normalized RMS, raw (consumer smooths), low to high:
    /// `[0-300Hz, 300-1kHz, 1k-3kHz, 3k-8kHz]`.
    pub bands: [f32; BANDS],
}

/// Lock-free single-word publication slot. Clone freely; all clones read
/// and write the same slot. Writer is the pipeline task, readers are
/// render threads at their own cadence.
#[derive(Clone, Default)]
pub struct MeterShared {
    slot: Arc<AtomicU64>,
}

/// 12 bits of precision per channel: 5 channels * 12 = 60 bits packed into
/// the one atomic word.
const QUANT: u64 = (1 << 12) - 1;

fn pack(f: &MeterFrame) -> u64 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * QUANT as f32) as u64;
    let mut w = q(f.level);
    for (i, b) in f.bands.iter().enumerate() {
        w |= q(*b) << (12 * (i + 1));
    }
    w
}

fn unpack(w: u64) -> MeterFrame {
    let d = |s: u32| ((w >> s) & QUANT) as f32 / QUANT as f32;
    MeterFrame {
        level: d(0),
        bands: core::array::from_fn(|i| d(12 * (i as u32 + 1))),
    }
}

impl MeterShared {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a frame. Relaxed ordering: readers only ever want "a recent
    /// frame", and the word is written atomically so no torn state exists.
    pub fn publish(&self, frame: &MeterFrame) {
        self.slot.store(pack(frame), Ordering::Relaxed);
    }

    /// Read the most recently published frame.
    pub fn read(&self) -> MeterFrame {
        unpack(self.slot.load(Ordering::Relaxed))
    }
}

/// One-pole low-pass, the cheapest filter that exists.
#[derive(Debug, Clone, Copy)]
struct OnePole {
    a: f32,
    y: f32,
}

impl OnePole {
    fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        // Standard one-pole coefficient from the RC analogy.
        let a = 1.0 - (-TAU * cutoff_hz / sample_rate).exp();
        Self { a, y: 0.0 }
    }

    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        self.y += self.a * (x - self.y);
        self.y
    }
}

/// Attack/release envelope follower over per-chunk values.
#[derive(Debug, Clone, Copy, Default)]
struct Envelope {
    y: f32,
}

impl Envelope {
    /// Advance by `dt` seconds toward `target`.
    fn step(&mut self, target: f32, dt: f32) -> f32 {
        let tau = if target > self.y { ATTACK_S } else { RELEASE_S };
        let coef = 1.0 - (-dt / tau).exp();
        self.y += coef * (target - self.y);
        self.y
    }
}

/// The processor: feed it the 16kHz chunks already headed to the
/// recognizer, it publishes smoothed level + bands to its [`MeterShared`].
///
/// Not `Sync` and not meant to be: exactly one task owns it and calls
/// [`AudioMeter::process`]; everyone else holds a `MeterShared`.
pub struct AudioMeter {
    shared: MeterShared,
    crossovers: [OnePole; BANDS - 1],
    level_env: Envelope,
}

impl AudioMeter {
    pub fn new(shared: MeterShared) -> Self {
        Self {
            shared,
            crossovers: CROSSOVERS_HZ.map(|hz| OnePole::new(hz, SAMPLE_RATE as f32)),
            level_env: Envelope::default(),
        }
    }

    /// Reset envelopes and filters, e.g. between utterances so the next
    /// key-down starts from silence instead of decaying old speech.
    pub fn reset(&mut self) {
        for c in &mut self.crossovers {
            c.y = 0.0;
        }
        self.level_env = Envelope::default();
        self.shared.publish(&MeterFrame::default());
    }

    /// Feed one chunk of 16kHz mono audio; publishes the updated frame and
    /// returns it (the pipeline forwards `level` into the overlay frame).
    ///
    /// Allocation-free and O(n) in the chunk length: ~5 multiply-adds per
    /// sample.
    pub fn process(&mut self, samples: &[f32]) -> MeterFrame {
        if samples.is_empty() {
            return self.shared.read();
        }
        // Band split + accumulate power in one pass.
        let mut power = [0.0f32; BANDS];
        let mut total = 0.0f32;
        for &x in samples {
            total += x * x;
            let mut below = 0.0;
            for (i, lp) in self.crossovers.iter_mut().enumerate() {
                let y = lp.step(x);
                let band = y - below;
                power[i] += band * band;
                below = y;
            }
            let top = x - below;
            power[BANDS - 1] += top * top;
        }
        let n = samples.len() as f32;
        let dt = n / SAMPLE_RATE as f32;
        let norm = |p: f32| ((p / n).sqrt() / FULL_SCALE_RMS).clamp(0.0, 1.0);

        let frame = MeterFrame {
            level: self.level_env.step(norm(total), dt),
            // Raw, not enveloped: the overlay's animator smooths bands
            // itself, and stacking two envelopes kills the jaw's attack.
            bands: core::array::from_fn(|i| norm(power[i])),
        };
        self.shared.publish(&frame);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (TAU * hz * i as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    #[test]
    fn pack_roundtrips_within_quantization() {
        let f = MeterFrame {
            level: 0.7,
            bands: [0.1, 0.4, 0.9, 0.0],
        };
        let g = unpack(pack(&f));
        assert!((g.level - f.level).abs() < 1e-3);
        for (a, b) in g.bands.iter().zip(&f.bands) {
            assert!((a - b).abs() < 1e-3);
        }
    }

    #[test]
    fn low_tone_lands_in_the_low_band() {
        let shared = MeterShared::new();
        let mut m = AudioMeter::new(shared.clone());
        // Long enough for the envelope to settle.
        for chunk in tone(120.0, 0.3, 16_000).chunks(480) {
            m.process(chunk);
        }
        let f = shared.read();
        assert!(
            f.bands[0] > 2.0 * f.bands[2],
            "120Hz should dominate the 0-300 band: {:?}",
            f.bands
        );
        assert!(f.level > 0.5, "loud tone should read loud: {}", f.level);
    }

    #[test]
    fn high_tone_lands_in_the_top_band() {
        let shared = MeterShared::new();
        let mut m = AudioMeter::new(shared.clone());
        for chunk in tone(6000.0, 0.3, 16_000).chunks(480) {
            m.process(chunk);
        }
        let f = shared.read();
        assert!(
            f.bands[3] > 2.0 * f.bands[0],
            "6kHz should dominate the top band: {:?}",
            f.bands
        );
    }

    #[test]
    fn release_is_slower_than_attack() {
        let shared = MeterShared::new();
        let mut m = AudioMeter::new(shared.clone());
        let loud = tone(440.0, 0.5, 480);
        let quiet = vec![0.0f32; 480];
        let after_one_loud = m.process(&loud).level;
        // One 30ms chunk of speech-level audio must register strongly.
        assert!(after_one_loud > 0.5, "attack too slow: {after_one_loud}");
        let after_one_quiet = m.process(&quiet).level;
        // One 30ms silence must NOT drop the level to nothing (jaw chatter).
        assert!(
            after_one_quiet > 0.4 * after_one_loud,
            "release too fast: {after_one_loud} -> {after_one_quiet}"
        );
        // But sustained silence must decay to (near) zero.
        for _ in 0..40 {
            m.process(&quiet);
        }
        assert!(shared.read().level < 0.05);
    }

    #[test]
    fn reset_publishes_silence() {
        let shared = MeterShared::new();
        let mut m = AudioMeter::new(shared.clone());
        m.process(&tone(440.0, 0.5, 4800));
        assert!(shared.read().level > 0.0);
        m.reset();
        assert_eq!(shared.read(), MeterFrame::default());
    }
}

#[cfg(test)]
mod cost {
    use super::*;

    /// Not a benchmark, a budget check: one 30ms chunk must process in the
    /// low microseconds, since it runs on the supervisor's event loop.
    #[test]
    fn a_chunk_costs_microseconds_not_milliseconds() {
        let shared = MeterShared::new();
        let mut m = AudioMeter::new(shared);
        let chunk: Vec<f32> = (0..480).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
        // Warm up, then time a batch.
        for _ in 0..100 {
            m.process(&chunk);
        }
        let t0 = std::time::Instant::now();
        const N: u32 = 10_000;
        for _ in 0..N {
            m.process(&chunk);
        }
        let per_chunk = t0.elapsed() / N;
        eprintln!("meter: {per_chunk:?} per 30ms chunk");
        assert!(
            per_chunk < std::time::Duration::from_micros(200),
            "meter too slow for the event loop: {per_chunk:?}"
        );
    }
}
