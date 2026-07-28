//! What a slow input device does to the dictation pipeline.
//!
//! The built-in microphone delivers its first sample ~71ms after the stream
//! opens (`examples/first_sample_latency.rs`). Bluetooth is a different animal:
//! opening a capture stream on an AirPod forces the headset into its hands-free
//! profile, which is a negotiated link change, not just a buffer allocation.
//! That can take several hundred milliseconds, and it is paid on every
//! utterance because the daemon opens the microphone on key-down.
//!
//! The question this answers is not "is it slow" but "what breaks". There are
//! two distinct failures and only one of them is visible to a user:
//!
//! 1. Audio arrives late but intact. The segmenter's 150ms pre-roll ring
//!    absorbs this: the word onset is still in the buffer when speech is
//!    detected, so nothing is lost.
//! 2. Audio does not exist yet. The device had not started when the user began
//!    speaking, so the opening syllable was never captured by anyone. No
//!    amount of buffering downstream can recover it.
//!
//! This simulates case 2 at a range of delays and reports where the transcript
//! actually starts losing words, which is the number that decides whether a
//! device needs the stream held open across utterances.
//!
//!     cargo run --release -p audio --example latency_impact

use audio::segment::{SegmenterConfig, SpeechEvent, SpeechSegmenter};
use audio::vad::EnergyVad;
use audio::SAMPLE_RATE;

/// One 30ms frame, the unit the segmenter consumes.
const FRAME: usize = SAMPLE_RATE as usize / 1000 * 30;

fn main() {
    println!("Simulating a device that starts late, then speech begins immediately.\n");
    println!("  A user presses the hotkey and starts talking at once. Whatever the");
    println!("  device had not started capturing yet is gone. The pre-roll ring holds");
    println!("  {}ms.\n", pre_roll_ms());

    println!(
        "  {:>10}  {:>12}  {:>12}  outcome",
        "device lag", "speech kept", "speech lost"
    );
    println!("  {}", "-".repeat(62));

    for lag_ms in [0, 50, 71, 100, 150, 200, 300, 500, 800] {
        let (kept_ms, lost_ms) = simulate(lag_ms);
        let outcome = if lost_ms == 0 {
            "clean".to_string()
        } else if lost_ms <= 80 {
            format!("clipped onset (~{lost_ms}ms)")
        } else {
            format!("LOST FIRST WORD (~{lost_ms}ms)")
        };
        println!("  {lag_ms:>8}ms  {kept_ms:>10}ms  {lost_ms:>10}ms  {outcome}");
    }

    println!();
    println!("Reading this: 'device lag' is how long after the keypress the stream");
    println!("delivers anything. Built-in mic measures ~71ms. Bluetooth hands-free");
    println!("negotiation is typically 200-600ms, which is where words start dying.");
}

fn pre_roll_ms() -> usize {
    SegmenterConfig::default().pre_roll_frames * 30
}

/// Model one utterance where the device starts `lag_ms` late.
///
/// Returns (ms of speech the segmenter emitted, ms of speech destroyed before
/// capture began). The second number is the one that matters: it is audio no
/// component downstream ever had a chance to see.
fn simulate(lag_ms: usize) -> (usize, usize) {
    // A 600ms utterance beginning the instant the key goes down.
    let speech_ms = 600;
    let lost_ms = lag_ms.min(speech_ms);
    let surviving_ms = speech_ms - lost_ms;

    // Feed only the speech that physically reached the stream. Anything the
    // device missed is not silence to be trimmed, it never existed.
    let mut seg = SpeechSegmenter::new(EnergyVad::default(), SegmenterConfig::default());
    let mut emitted = 0usize;

    for _ in 0..(surviving_ms / 30) {
        for ev in seg.push(&loud_frame()) {
            if let SpeechEvent::SpeechStart { audio } = ev {
                emitted += audio.len() * 1000 / SAMPLE_RATE as usize;
            }
        }
    }
    // Trailing silence so the segmenter closes the utterance.
    for _ in 0..15 {
        for ev in seg.push(&quiet_frame()) {
            if let SpeechEvent::SpeechEnd { audio, .. } = ev {
                emitted = audio.len() * 1000 / SAMPLE_RATE as usize;
            }
        }
    }
    if let Some(SpeechEvent::SpeechEnd { audio, .. }) = seg.flush() {
        emitted = audio.len() * 1000 / SAMPLE_RATE as usize;
    }

    (emitted, lost_ms)
}

/// A frame the energy VAD scores as speech.
fn loud_frame() -> Vec<f32> {
    (0..FRAME).map(|i| (i as f32 * 0.15).sin() * 0.4).collect()
}

/// A frame quiet enough to count as silence.
fn quiet_frame() -> Vec<f32> {
    vec![0.0; FRAME]
}
