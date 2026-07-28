//! Measure how long a capture stream takes to deliver its first sample.
//!
//! WHY this matters here specifically: the daemon opens the microphone on
//! key-down and closes it on commit (see `crates/outloud/src/mic.rs`), so this
//! cost is paid on *every* utterance rather than once at startup. Whatever it
//! is, it lands between the user pressing the key and the first audio the
//! recognizer can see.
//!
//! The segmenter keeps a 150ms pre-roll ring, so it can recover a word onset
//! that arrives late *within the stream*. It cannot recover audio that the
//! device never delivered because the stream had not started yet. Those are
//! different failures and only the second one clips words.
//!
//! Run against whatever device is currently default:
//!     cargo run --release -p audio --example first_sample_latency

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::capture::start_capture;
use audio::ring::ring;

/// How many open/close cycles to time. Enough to see spread without making
/// the operator sit through a long run.
const RUNS: usize = 10;

/// Give up on a single run after this. A Bluetooth device negotiating a
/// profile switch can take a while, but not this long.
const TIMEOUT: Duration = Duration::from_secs(5);

fn main() {
    let device = current_device_name();
    println!("device: {device}");
    println!("running {RUNS} open -> first-sample cycles\n");

    let mut samples = Vec::with_capacity(RUNS);
    for i in 1..=RUNS {
        match time_first_sample() {
            Some(elapsed) => {
                println!("  run {i:2}: {:>7.1}ms", elapsed.as_secs_f64() * 1000.0);
                samples.push(elapsed);
            }
            None => println!("  run {i:2}:  TIMEOUT (no audio within {TIMEOUT:?})"),
        }
        // Let the device settle so each run measures a cold open, which is
        // what the daemon actually does, rather than a warm reopen.
        std::thread::sleep(Duration::from_millis(400));
    }

    if samples.is_empty() {
        println!("\nno successful runs; is a microphone connected and permitted?");
        return;
    }

    samples.sort();
    let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
    let p = |q: f64| ms(&samples[((samples.len() - 1) as f64 * q).round() as usize]);

    println!(
        "\n  n={}  min={:.1}ms  p50={:.1}ms  p90={:.1}ms  max={:.1}ms",
        samples.len(),
        ms(&samples[0]),
        p(0.5),
        p(0.9),
        ms(samples.last().unwrap())
    );

    // The judgement, not just the numbers. 150ms is the segmenter's pre-roll:
    // below that, a late first sample is invisible to the user because the
    // ring still holds the whole word onset.
    let p50 = p(0.5);
    println!();
    if p50 < 150.0 {
        println!("VERDICT: fine. First audio arrives inside the 150ms pre-roll window,");
        println!("so a word begun immediately after key-down is still captured whole.");
    } else if p50 < 400.0 {
        println!("VERDICT: marginal. First audio arrives after the 150ms pre-roll, so a");
        println!("user who starts speaking instantly will lose the first syllable.");
        println!("Mitigation: hold the stream open across utterances for this device,");
        println!("or widen pre-roll. Both trade privacy or memory for onset safety.");
    } else {
        println!("VERDICT: bad. Over 400ms before any audio exists. Every utterance");
        println!("loses its opening word unless the stream is kept warm.");
    }
}

/// Open a stream, wait for the first non-empty read, return the delay.
fn time_first_sample() -> Option<Duration> {
    let (producer, consumer) = ring(32_768);
    let started = Instant::now();

    let failed = Arc::new(AtomicBool::new(false));
    let failed_sink = Arc::clone(&failed);
    let handle = start_capture(producer, move |event| {
        if matches!(event, audio::capture::CaptureEvent::Error { .. }) {
            failed_sink.store(true, Ordering::SeqCst);
        }
    });

    let mut buf = vec![0.0f32; 1024];
    let deadline = started + TIMEOUT;
    let result = loop {
        if failed.load(Ordering::SeqCst) || Instant::now() > deadline {
            break None;
        }
        if consumer.pop(&mut buf) > 0 {
            break Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(1));
    };

    handle.stop();
    result
}

fn current_device_name() -> String {
    // Reuse the crate's own enumeration so this reports the same name the
    // daemon logs, rather than a second spelling of the same device.
    audio::capture::input_devices()
        .ok()
        .and_then(|ds| ds.into_iter().find(|d| d.is_default))
        .map(|d| d.name)
        .unwrap_or_else(|| "(none)".into())
}
