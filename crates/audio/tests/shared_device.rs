//! Capturing while another process holds the same input device.
//!
//! Dictating into a Discord or FaceTime call is a normal thing to want. It
//! works because CoreAudio shares input devices rather than granting one
//! process exclusive ownership, but "works" here is a property of how we open
//! the device, not a law of nature. Two mistakes would break it:
//!
//! - Setting `kAudioDevicePropertyHogMode`, which takes the device
//!   exclusively and would knock every call app off the microphone the moment
//!   a user pressed the dictation hotkey.
//! - Demanding a specific sample rate instead of accepting the device's
//!   current format, which can force a reconfiguration that interrupts
//!   whoever was already capturing.
//!
//! Neither mistake shows up in an ordinary test, because a single-process test
//! captures perfectly well either way. So this file opens the device twice at
//! once and checks that both sides still receive audio.
//!
//! Every test here skips rather than fails when the machine has no microphone,
//! since that is the normal state of a CI runner and is not a regression.

// `capture` gates the only module under test here; without it there is no
// audio backend to share, and the crate deliberately builds without one.
#![cfg(all(target_os = "macos", feature = "capture"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::capture::{start_capture, CaptureEvent, CaptureHandle};
use audio::ring::{ring, Consumer};

/// Ring capacity in samples: a couple of seconds at 16kHz, so a slow drain
/// loop cannot make the test flaky by overflowing.
const RING_CAPACITY: usize = 32_768;

/// Samples a stream must deliver before we believe it is really capturing.
///
/// A silent room still produces frames, because CoreAudio delivers zeroed
/// buffers, so a stream that receives nothing was starved rather than quiet.
const MIN_SAMPLES: usize = 2_000;

/// How long to wait for that before calling it a failure.
const CAPTURE_WINDOW: Duration = Duration::from_secs(5);

/// Two capture streams on the same device must both receive audio.
///
/// If the second one cannot start, or starts but is starved, we are taking the
/// device exclusively and every call application on the machine would lose its
/// microphone the moment a user pressed the dictation hotkey.
#[test]
fn two_concurrent_captures_both_receive_audio() {
    if !has_input_device() {
        eprintln!("SKIP: no input device on this machine");
        return;
    }

    let first = Capture::start();
    let second = Capture::start();

    let deadline = Instant::now() + CAPTURE_WINDOW;
    while Instant::now() < deadline {
        if first.drained() >= MIN_SAMPLES && second.drained() >= MIN_SAMPLES {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let (a, b) = (first.drained(), second.drained());
    let (a_failed, b_failed) = (first.failed(), second.failed());
    first.stop();
    second.stop();

    assert!(
        !a_failed && !b_failed,
        "a capture stream reported an error while sharing the device \
         (first failed: {a_failed}, second failed: {b_failed}); \
         this is what exclusive access looks like from the loser's side"
    );
    assert!(
        a >= MIN_SAMPLES && b >= MIN_SAMPLES,
        "both streams should receive audio while sharing the device, got {a} \
         and {b} samples (expected >= {MIN_SAMPLES} each). A near-zero count \
         means one stream starved the other, so check for hog mode or a \
         forced sample rate."
    );
}

/// Opening capture must not change the format the device is already running.
///
/// We accept `default_input_config()` and resample to 16kHz ourselves. If that
/// is ever "simplified" into requesting 16kHz from the device, this notices:
/// the reported rate would become the one we demanded, and the reconfiguration
/// is exactly what interrupts an application already in a call.
#[test]
fn capture_does_not_reconfigure_the_device() {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        eprintln!("SKIP: no input device on this machine");
        return;
    };
    let Ok(before) = device.default_input_config() else {
        eprintln!("SKIP: device reported no default config");
        return;
    };

    let capture = Capture::start();
    std::thread::sleep(Duration::from_millis(750));
    let during = device.default_input_config();
    capture.stop();

    let Ok(during) = during else {
        panic!("device stopped reporting a config while we were capturing");
    };

    assert_eq!(
        before.sample_rate(),
        during.sample_rate(),
        "opening capture changed the device sample rate from {:?} to {:?}. \
         Accept default_input_config() and resample, rather than requesting a \
         rate, so an app already using the microphone is not interrupted.",
        before.sample_rate(),
        during.sample_rate()
    );
}

fn has_input_device() -> bool {
    use cpal::traits::HostTrait;
    cpal::default_host().default_input_device().is_some()
}

/// A running capture, with a thread draining its ring so the producer never
/// blocks and the samples can be counted.
struct Capture {
    handle: CaptureHandle,
    drained: Arc<std::sync::atomic::AtomicUsize>,
    failed: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    drain_thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    fn start() -> Capture {
        let (producer, consumer) = ring(RING_CAPACITY);

        // Capture errors matter as much as sample counts here: a stream that
        // is refused the device reports through this channel rather than by
        // failing to construct.
        let failed = Arc::new(AtomicBool::new(false));
        let failed_sink = Arc::clone(&failed);
        let handle = start_capture(producer, move |event| {
            if matches!(event, CaptureEvent::Error { .. }) {
                failed_sink.store(true, Ordering::SeqCst);
            }
        });

        let drained = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let draining = Arc::new(AtomicBool::new(true));
        let drain_thread = spawn_drain(consumer, Arc::clone(&drained), Arc::clone(&draining));

        Capture {
            handle,
            drained,
            failed,
            draining,
            drain_thread: Some(drain_thread),
        }
    }

    fn drained(&self) -> usize {
        self.drained.load(Ordering::Relaxed)
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    fn stop(mut self) {
        self.draining.store(false, Ordering::SeqCst);
        if let Some(thread) = self.drain_thread.take() {
            let _ = thread.join();
        }
        self.handle.stop();
    }
}

fn spawn_drain(
    consumer: Consumer,
    drained: Arc<std::sync::atomic::AtomicUsize>,
    draining: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = vec![0.0f32; 1024];
        while draining.load(Ordering::SeqCst) {
            let n = consumer.pop(&mut buf);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            drained.fetch_add(n, Ordering::Relaxed);
        }
    })
}
