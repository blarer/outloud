//! `finalize` must survive a recognizer that has stopped consuming.
//!
//! `pipeline::commit` calls `AudioFeed::finalize` inline on the async
//! event loop, with the microphone open. The original implementation used
//! a blocking send on a bounded channel, which is safe only while the
//! consumer keeps draining. A recognizer wedged inside `feed()` (an Apple
//! helper whose stdin pipe has filled, a stalled child process) leaves the
//! queue full, and the blocking send then stops the whole daemon: a hot
//! microphone plus a frozen UI, not merely a late commit.
//!
//! These tests drive the real `AudioFeed` against a deliberately stuck
//! consumer, because the property only exists under saturation and no unit
//! test of the happy path can observe it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use asr::Recognizer;
use outloud::recognize::spawn;

/// A recognizer that blocks forever inside `feed`, like a helper whose
/// stdin pipe is full.
struct WedgedRecognizer {
    /// Lets the test release the block so the thread can exit.
    released: Arc<AtomicBool>,
}

impl Recognizer for WedgedRecognizer {
    fn feed(&mut self, _samples: &[f32]) -> Option<asr::Partial> {
        while !self.released.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    fn finalize(&mut self) -> anyhow::Result<asr::Transcript> {
        Ok(asr::Transcript::empty())
    }

    fn name(&self) -> &'static str {
        "wedged"
    }
}

#[test]
fn finalize_returns_promptly_when_the_recognizer_is_wedged() {
    let released = Arc::new(AtomicBool::new(false));
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let feed = {
        let released = Arc::clone(&released);
        spawn(
            move || {
                Ok(Box::new(WedgedRecognizer {
                    released: Arc::clone(&released),
                }) as Box<dyn Recognizer>)
            },
            events_tx,
            ready_tx,
        )
    };
    // Wait for the worker to come up, so the wedge is in `feed` rather
    // than in construction.
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(ready_rx);

    // Saturate: far more chunks than the queue can hold, against a
    // consumer that will never take a second one.
    for _ in 0..2_000 {
        feed.push(vec![0.0f32; 480]);
    }

    // The property. A blocking send here waited on a queue that never
    // drains, which is unbounded in the worst case; the daemon must not.
    let started = Instant::now();
    feed.finalize();
    let elapsed = started.elapsed();

    released.store(true, Ordering::SeqCst);
    assert!(
        elapsed < Duration::from_millis(100),
        "finalize blocked for {elapsed:?} behind a wedged recognizer"
    );
}

#[test]
fn audio_is_still_bounded_when_the_recognizer_is_wedged() {
    // The unbounded channel must not become an unbounded memory leak: the
    // ceiling moved from the channel into `push`, and it has to still be
    // there.
    let released = Arc::new(AtomicBool::new(false));
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let feed = {
        let released = Arc::clone(&released);
        spawn(
            move || {
                Ok(Box::new(WedgedRecognizer {
                    released: Arc::clone(&released),
                }) as Box<dyn Recognizer>)
            },
            events_tx,
            ready_tx,
        )
    };
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(ready_rx);

    const SENT: u64 = 2_000;
    for _ in 0..SENT {
        feed.push(vec![0.0f32; 480]);
    }
    released.store(true, Ordering::SeqCst);

    // Most of that must have been refused rather than queued. The exact
    // figure depends on how many the worker consumed before wedging, so
    // this asserts the shape: the large majority dropped.
    let dropped = feed.dropped_chunks();
    assert!(
        dropped > SENT / 2,
        "only {dropped} of {SENT} chunks were dropped; the queue is not bounded"
    );
}
