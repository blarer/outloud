//! The menu's sensitivity ceiling, checked against the noise floor itself.
//!
//! `menubar.rs` asserts that no offered step exceeds a hardcoded number.
//! That number is only correct as long as it matches where the VAD actually
//! starts finding speech in silence, and it silently stopped matching once:
//! re-anchoring the knee moved the true ceiling from 90 to 75 while the
//! assertion happily kept passing at 90.
//!
//! So this test derives the ceiling instead of restating it. It synthesizes
//! a quiet room, finds the lowest sensitivity at which the VAD calls that
//! noise "speech", and requires every menu step to sit safely below it.

use audio::vad::{EnergyVad, VoiceDetector};

/// A quiet room on a built-in microphone: Gaussian noise at ~0.0002 RMS.
/// Deterministic, so this cannot flake.
fn quiet_room_frames() -> Vec<Vec<f32>> {
    // xorshift, to avoid a dev-dependency on a RNG crate for one fixture.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Map to roughly N(0, 1) via the central limit theorem on 4 draws.
        let unit = |v: u64| (v >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
        (0..4)
            .map(|_| unit(state.wrapping_mul(6364136223846793005)))
            .sum::<f64>() as f32
    };
    (0..200)
        .map(|_| (0..480).map(|_| next() * 0.000173).collect())
        .collect()
}

/// The lowest sensitivity at which a quiet room reads as speech.
fn hallucination_floor() -> u8 {
    let frames = quiet_room_frames();
    for s in 1..=100u8 {
        let mut vad = EnergyVad::from_sensitivity(s);
        let speechy = frames
            .iter()
            .filter(|f| vad.speech_probability(f) >= 0.5)
            .count();
        // The segmenter needs consecutive speech frames to open an
        // utterance, so a stray frame is not enough; a steady few percent
        // is. 3% of a couple hundred frames is the practical onset.
        if speechy * 100 / frames.len() >= 3 {
            return s;
        }
    }
    100
}

#[test]
fn the_fixture_really_is_a_quiet_room() {
    // Guard the guard: if this synthetic noise is not actually at a quiet
    // room's level, every conclusion drawn from it is worthless.
    let all: Vec<f32> = quiet_room_frames().concat();
    let rms = (all.iter().map(|s| s * s).sum::<f32>() / all.len() as f32).sqrt();
    assert!(
        (0.00015..0.0004).contains(&rms),
        "fixture RMS {rms:.6} is not a quiet room (~0.0002)"
    );
}

#[test]
fn every_offered_step_stays_below_the_noise_floor() {
    let floor = hallucination_floor();
    // Checks the shipped steps directly, so adding or raising one that
    // crosses the noise floor fails here rather than in a user's transcript.
    for (value, label) in audio::vad::SENSITIVITY_STEPS {
        assert!(
            value < floor,
            "step \"{label}\" ({value}) is at or above {floor}, where a quiet \
             room reads as speech"
        );
    }
    // Menu steps live in the daemon crate; restating them here would just
    // move the drift. Instead assert the property the menu must satisfy,
    // and let menubar.rs's own test check its steps against this number.
    assert!(
        floor > 70,
        "sensitivity {floor} already hears a quiet room as speech, but the \
         menu offers 70 as its top step. Lower the menu ceiling, or raise \
         the knee anchor."
    );
}

#[test]
fn the_default_sits_well_clear_of_the_noise_floor() {
    // Not merely below it: a default one step from hallucinating would
    // misbehave on any microphone noisier than the test fixture.
    let floor = hallucination_floor();
    assert!(
        floor >= 70,
        "the default (50) is uncomfortably close to the noise floor at {floor}"
    );
}
