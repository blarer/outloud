//! What RMS your voice actually produces, at whatever distance you sit.
//!
//! The segmenter decides "is this speech" with an RMS gate whose knee comes
//! from `microphone.sensitivity`. Anything quieter than the knee is treated
//! as silence and never reaches the recognizer, so a threshold set too high
//! is indistinguishable from a microphone that cannot hear you.
//!
//! This reports what your microphone really delivers, and which sensitivity
//! setting would capture it, so the setting is chosen from a measurement
//! instead of a guess.
//!
//!     cargo run --release -p audio --example mic_level
//!
//! Speak the way you normally do, then lean back and speak again. Ctrl-C
//! when done: the summary prints either way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::capture::start_capture;
use audio::ring::ring;
use audio::vad::{EnergyVad, SENSITIVITY_STEPS};

/// 30ms at 16kHz, the frame the segmenter scores.
const FRAME: usize = 480;

/// How long to listen before summarizing on our own.
const RUN: Duration = Duration::from_secs(12);

fn main() {
    // Read the live default rather than restating a number: a hardcoded
    // copy here once reported the OLD threshold after the default moved,
    // labelling audible speech as silence and making a working microphone
    // look deaf.
    let default_knee = EnergyVad::new().knee();

    println!("Listening. Speak normally, then lean back and speak again.");
    println!("Ctrl-C when you are done.\n");
    println!("current default (Normal) hears anything above {default_knee:.5} RMS\n");

    let (producer, consumer) = ring(16_000 * 20);
    let failed = Arc::new(AtomicBool::new(false));
    let sink = Arc::clone(&failed);
    let handle = start_capture(producer, move |ev| {
        if let audio::capture::CaptureEvent::Error { message } = ev {
            eprintln!("capture error: {message}");
            sink.store(true, Ordering::SeqCst);
        }
    });

    // Ctrl-C must still summarize: the summary is the entire point, and
    // the natural way to end "speak until you are done" is to interrupt.
    install_sigint_handler();

    let mut frame = vec![0.0f32; FRAME];
    let mut loud: Vec<f32> = Vec::new();
    let deadline = Instant::now() + RUN;
    let mut last_print = Instant::now();

    while Instant::now() < deadline
        && !INTERRUPTED.load(Ordering::SeqCst)
        && !failed.load(Ordering::SeqCst)
    {
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

        if last_print.elapsed() > Duration::from_millis(120) {
            let bars = ((rms / 0.02) * 30.0).min(30.0) as usize;
            let label = if rms >= default_knee {
                "SPEECH"
            } else {
                "      "
            };
            println!("{rms:>8.5}  {label} {}", "#".repeat(bars));
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

    println!(
        "\n--- your microphone, {} frames with content ---",
        loud.len()
    );
    println!("  quietest 10%  {p10:.5}");
    println!("  median        {p50:.5}");
    println!("  loudest 10%   {p90:.5}");

    // What each setting would actually capture, which is the decision the
    // user is trying to make. A percentage of *your* frames is a far more
    // useful answer than an RMS threshold in the abstract.
    println!("\n  how much of that each setting would hear:");
    let mut recommended = None;
    for (value, name) in SENSITIVITY_STEPS {
        let knee = EnergyVad::from_sensitivity(value).knee();
        let heard = loud.iter().filter(|&&r| r >= knee).count();
        let pct_heard = 100.0 * heard as f32 / loud.len() as f32;
        let marker = if value == 50 { " (current)" } else { "" };
        println!("    {name:<22} {pct_heard:5.0}% of your speech{marker}");
        // First setting that captures the quiet tail, since the missing
        // words are always the quietest frames, not the average ones.
        if recommended.is_none() && pct_heard >= 90.0 {
            recommended = Some(name);
        }
    }

    match recommended {
        Some(name) => println!("\n  recommended: {name}"),
        None => println!(
            "\n  even Very High misses some of this. Move closer, or check\n  \
             that the right input device is selected."
        ),
    }
}

/// Minimal SIGINT hook so the summary survives Ctrl-C, without pulling in a
/// dependency for one signal in one example.
///
/// A plain static AtomicBool, because a signal handler may only touch
/// async-signal-safe state: no allocation, no closures, no locks.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn install_sigint_handler() {
    // SAFETY: `on_sigint` only stores to a static atomic, which is
    // async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}
