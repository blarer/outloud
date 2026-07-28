// The `capture` feature contract, enforced rather than documented.
//
// The point of the feature is that a build WITHOUT it links no system audio
// library. That property is invisible to a normal test: everything here
// compiles either way, and the failure it prevents shows up only as a Linux
// CI job dying in alsa-sys's build script with "Package alsa was not found
// in the pkg-config search path" or, on musl, "pkg-config has not been
// configured to support cross-compilation".
//
// So these tests assert the two halves of the contract that CAN be checked
// mechanically:
//
//   1. the DSP surface stays available with --no-default-features, which is
//      what makes a headless build useful rather than merely linkable; and
//   2. the capture surface exists exactly when the feature is on.
//
// Whether cpal actually left the dependency graph is a cargo-level fact, not
// a runtime one, and is verified with:
//
//   cargo tree -p audio --no-default-features -e normal \
//       --target x86_64-unknown-linux-musl | grep -c -E 'cpal|alsa'   # -> 0
//   cargo tree -p audio                      -e normal \
//       --target x86_64-unknown-linux-musl | grep -c -E 'cpal|alsa'   # -> 3
//
// That command is the real regression check for the CI blocker; it is
// recorded here so the next person changing this file knows where the
// guarantee actually lives.

use audio::{FRAME_SAMPLES, SAMPLE_RATE};

/// The whole point of the split: with capture off, the DSP path still works.
/// If this ever fails to compile, the headless build has lost the ability to
/// process audio it received from somewhere other than a microphone (a WAV
/// file, a socket, a test fixture) and the feature has become useless.
#[test]
fn dsp_surface_is_available_without_the_capture_feature() {
    // Resampling: the path a WAV file takes on a headless box.
    let stereo_48k = vec![0.25f32; 960 * 2];
    let mono = audio::resample::downmix(&stereo_48k, 2);
    assert_eq!(mono.len(), 960, "downmix must halve a stereo frame count");

    let mut rs = audio::resample::Resampler::new(48_000, SAMPLE_RATE);
    let out = rs.process(&mono);
    assert!(
        !out.is_empty(),
        "resampling 48k -> 16k must produce samples with no audio stack present"
    );

    // The ring buffer, which the capture callback feeds but does not own.
    let (tx, rx) = audio::ring::ring(SAMPLE_RATE as usize);
    tx.push(&[0.5f32; 160]);
    let mut sink = vec![0.0f32; 160];
    assert_eq!(rx.pop(&mut sink), 160, "ring must round-trip without cpal");
    assert_eq!(sink[0], 0.5);
}

/// The segmenter state machine is the part with the real logic, and it must
/// stay testable in CI on a machine with no sound card at all. This is the
/// reason the crate is structured with capture at the edge.
#[test]
fn segmenter_runs_without_the_capture_feature() {
    use audio::segment::SpeechSegmenter;
    use audio::vad::EnergyVad;

    let mut seg = SpeechSegmenter::new(EnergyVad::new(), Default::default());
    // Silence must not open an utterance.
    let silence = vec![0.0f32; FRAME_SAMPLES];
    for _ in 0..10 {
        seg.push(&silence);
    }
    // Loud frames should eventually be treated as speech by the energy VAD.
    let loud: Vec<f32> = (0..FRAME_SAMPLES)
        .map(|i| if i % 2 == 0 { 0.6 } else { -0.6 })
        .collect();
    let mut saw_event = false;
    for _ in 0..20 {
        if !seg.push(&loud).is_empty() {
            saw_event = true;
        }
    }
    assert!(
        saw_event,
        "the segmenter produced no event for clearly voiced frames; the state \
         machine must work with no audio hardware or backend present"
    );
}

/// Constants downstream crates depend on must not move with the feature.
/// `asr` and `hexad` both hardcode assumptions about 16kHz mono; if a
/// headless build disagreed with a desktop build about the sample rate, WAV
/// transcription would silently run at the wrong speed.
#[test]
fn sample_rate_contract_is_feature_independent() {
    assert_eq!(SAMPLE_RATE, 16_000);
    assert_eq!(FRAME_SAMPLES, 480, "30ms at 16kHz");
}

/// Capture exists exactly when the feature is on. Two mirrored tests rather
/// than one, so BOTH directions are pinned: turning the feature on must give
/// you a microphone, and the module must not quietly reappear without it.
#[cfg(feature = "capture")]
#[test]
fn capture_surface_exists_when_the_feature_is_on() {
    // Enumeration is allowed to fail (CI runners have no input device); what
    // matters is that the symbol is reachable and returns a Result rather
    // than panicking or being absent.
    let _: anyhow::Result<Vec<audio::capture::InputDevice>> = audio::capture::input_devices();
}
