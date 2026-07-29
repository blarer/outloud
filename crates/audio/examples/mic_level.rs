//! What RMS your voice actually produces, at whatever distance you sit.
//!
//! The segmenter decides "is this speech" with an RMS gate whose knee is
//! 0.01 by default: 0.5 probability exactly at the knee, and the segmenter
//! wants 0.5 or above. So anything quieter than ~0.01 RMS is treated as
//! silence and never reaches the recognizer.
//!
//! That number was tuned against synthetic audio, not against a person
//! leaning back in a chair. This prints what your microphone really
//! delivers so the threshold can be set from a measurement instead of a
//! guess.
//!
//!     cargo run --release -p audio --example mic_level
//!
//! Speak normally at your usual distance. Then try leaning back, and try
//! speaking quietly. The summary reports what a knee would have to be to
//! hear each case.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::capture::start_capture;
use audio::ring::ring;

/// 30ms at 16kHz, the frame the segmenter scores.
const FRAME: usize = 480;

/// How long to listen.
const RUN: Duration = Duration::from_secs(12);

/// The shipped default, for reference in the output.
const DEFAULT_KNEE: f32 = 0.01;

fn main() {
    println!(
        "Listening for {}s. Speak the way you normally would,",
        RUN.as_secs()
    );
    println!("then lean back and speak again, then try speaking quietly.\n");

    let (producer, consumer) = ring(16_000 * 20);
    let failed = Arc::new(AtomicBool::new(false));
    let sink = Arc::clone(&failed);
    let handle = start_capture(producer, move |ev| {
        if let audio::capture::CaptureEvent::Error { message } = ev {
            eprintln!("capture error: {message}");
            sink.store(true, Ordering::SeqCst);
        }
    });

    let mut frame = vec![0.0f32; FRAME];
    let mut loud: Vec<f32> = Vec::new();
    let deadline = Instant::now() + RUN;
    let mut last_print = Instant::now();

    while Instant::now() < deadline && !failed.load(Ordering::SeqCst) {
        if consumer.pop(&mut frame) < FRAME {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();

        // Only frames with some content are interesting: a room's noise
        // floor would otherwise dominate the percentiles and hide the
        // quiet-speech case this exists to find.
        if rms > 0.0005 {
            loud.push(rms);
        }

        // A live bar, so it is obvious the microphone is working and which
        // frames are being counted.
        if last_print.elapsed() > Duration::from_millis(120) {
            let bars = ((rms / 0.05) * 40.0).min(40.0) as usize;
            let heard = if rms >= DEFAULT_KNEE {
                "SPEECH"
            } else {
                "      "
            };
            println!("{:>8.5}  {heard} {}", rms, "#".repeat(bars));
            last_print = Instant::now();
        }
    }
    handle.stop();

    if loud.is_empty() {
        println!("\nNo audio above the noise floor. Is the microphone permitted and unmuted?");
        return;
    }

    loud.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |q: f64| loud[((loud.len() - 1) as f64 * q) as usize];
    let (p10, p50, p90) = (pct(0.10), pct(0.50), pct(0.90));
    let heard = loud.iter().filter(|&&r| r >= DEFAULT_KNEE).count();

    println!(
        "\n--- your microphone, {} frames with content ---",
        loud.len()
    );
    println!("  quietest 10%  {p10:.5}");
    println!("  median        {p50:.5}");
    println!("  loudest 10%   {p90:.5}");
    println!("\n  current knee  {DEFAULT_KNEE:.5}");
    println!(
        "  frames the segmenter would call speech: {heard}/{} ({:.0}%)",
        loud.len(),
        100.0 * heard as f32 / loud.len() as f32
    );

    // The recommendation is the point of the exercise. Sit it below the
    // quiet end of real speech, but comfortably above a room noise floor.
    let suggested = (p10 * 0.6).max(0.0005);
    println!("\n  suggested knee for this microphone and distance: {suggested:.5}");
    if suggested < DEFAULT_KNEE * 0.8 {
        println!("  (lower than the default: your voice is quieter than the tuning assumed,");
        println!("   which is exactly the leaning-back case)");
    }
}
