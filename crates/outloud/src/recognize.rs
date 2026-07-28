//! The recognizer worker: audio in over a bounded channel, partials and
//! finals out, with drop-not-block backpressure.
//!
//! The recognizer runs on its own OS thread (not a tokio task) because
//! `Recognizer::feed`/`finalize` are blocking calls into a child process or
//! a model, and because the Apple backend spawns per-utterance helpers whose
//! pipes must never share a reactor with the event loop.
//!
//! Backpressure: the audio channel is bounded (~10s of speech in 30ms
//! frames) and fed with `try_send`. When the recognizer falls behind, audio
//! is DROPPED and counted, never awaited, because the sender sits downstream
//! of the event tap and the capture callback: blocking there is the one
//! thing this design forbids (deliverable 1). A gap in the transcript is an
//! honest failure; a wedged hotkey is a broken product.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use asr::{Recognizer, Transcript};
use tokio::sync::mpsc::UnboundedSender;

/// Messages into the recognizer thread.
pub enum AudioMsg {
    /// One chunk of 16kHz mono utterance audio.
    Chunk(Vec<f32>),
    /// The utterance ended (key release / VAD endpoint): finalize and emit.
    Finalize,
}

/// Messages out of the recognizer thread, consumed by the supervisor.
#[derive(Debug)]
pub enum AsrEvent {
    /// A fresh whole-hypothesis partial.
    Partial(String),
    /// The committed transcript for the utterance just finalized.
    Final(anyhow::Result<Transcript>),
}

/// Sending half handed to the pipeline. Wraps the bounded channel with the
/// drop-and-count policy so no call site can accidentally block.
pub struct AudioFeed {
    tx: SyncSender<AudioMsg>,
    dropped_chunks: Arc<std::sync::atomic::AtomicU64>,
}

impl AudioFeed {
    /// Queue utterance audio. Drops (and counts) when the recognizer is
    /// behind; never blocks.
    pub fn push(&self, samples: Vec<f32>) {
        match self.tx.try_send(AudioMsg::Chunk(samples)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped_chunks
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Recognizer thread died; finalize will surface the error.
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Signal end of utterance. Uses a blocking send: `Finalize` must not be
    /// droppable (a lost finalize means a silently swallowed utterance), and
    /// by key-release time the audio producer has already stopped, so this
    /// send is off the capture-critical path.
    pub fn finalize(&self) {
        let _ = self.tx.send(AudioMsg::Finalize);
    }

    /// Chunks dropped because the recognizer fell behind, for honest
    /// diagnostics in the end-of-utterance report.
    pub fn dropped_chunks(&self) -> u64 {
        self.dropped_chunks
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Spawn the recognizer worker. `make_recognizer` is a *factory* called on
/// the worker thread once at startup (readiness probe, covered by the
/// ModelLoading state) and then once per utterance. Per-utterance
/// construction exists because the Apple backend's `finalize` ends its
/// helper process (closing stdin is the end-of-utterance signal), so the
/// instance is spent afterwards despite the trait's reset contract; the
/// worker hides the respawn cost by pre-warming the next instance right
/// after each finalize, while the user is reading their committed text.
///
/// Events are delivered on an unbounded tokio channel: partials are tiny
/// strings at ≤7Hz (30ms frames), so unbounded is safe, and the supervisor
/// must never miss a `Final`.
pub fn spawn(
    make_recognizer: impl Fn() -> anyhow::Result<Box<dyn Recognizer>> + Send + 'static,
    events: UnboundedSender<AsrEvent>,
    ready: tokio::sync::oneshot::Sender<anyhow::Result<&'static str>>,
) -> AudioFeed {
    // ~10s of audio in 30ms frames (333) rounded up. Deep enough that only a
    // genuinely stuck recognizer drops audio, shallow enough to bound memory.
    let (tx, rx) = std::sync::mpsc::sync_channel::<AudioMsg>(384);
    let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));

    std::thread::Builder::new()
        .name("outloud-asr".into())
        .spawn(move || worker(make_recognizer, rx, events, ready))
        .expect("spawning recognizer thread");

    AudioFeed {
        tx,
        dropped_chunks: dropped,
    }
}

fn worker(
    make_recognizer: impl Fn() -> anyhow::Result<Box<dyn Recognizer>>,
    rx: Receiver<AudioMsg>,
    events: UnboundedSender<AsrEvent>,
    ready: tokio::sync::oneshot::Sender<anyhow::Result<&'static str>>,
) {
    // First construction doubles as the readiness probe: it exercises the
    // helper spawn / model load path while the UI shows ModelLoading.
    let mut rec: Option<Box<dyn Recognizer>> = match make_recognizer() {
        Ok(r) => {
            let _ = ready.send(Ok(r.name()));
            Some(r)
        }
        Err(e) => {
            // Construction failed (helper missing, model absent). The
            // supervisor turns this into the Error state with a named next
            // action; this thread has nothing further to do.
            let _ = ready.send(Err(e));
            return;
        }
    };

    while let Ok(msg) = rx.recv() {
        match msg {
            AudioMsg::Chunk(samples) => {
                // Rebuild lazily if the pre-warm after the last utterance
                // failed; failing again here surfaces at finalize below.
                if rec.is_none() {
                    rec = make_recognizer().ok();
                }
                if let Some(r) = rec.as_mut() {
                    if let Some(partial) = r.feed(&samples) {
                        if events.send(AsrEvent::Partial(partial.text)).is_err() {
                            return; // supervisor gone: daemon shutting down
                        }
                    }
                }
            }
            AudioMsg::Finalize => {
                let result = match rec.take() {
                    // A fresh recognizer that heard no audio finalizes to an
                    // empty transcript, which the supervisor treats as the
                    // documented "empty (silence) result" path.
                    Some(mut r) => r.finalize(),
                    None => make_recognizer()
                        .map_err(|e| e.context("recognizer failed to start for this utterance"))
                        .and_then(|mut r| r.finalize()),
                };
                if events.send(AsrEvent::Final(result)).is_err() {
                    return;
                }
                // Pre-warm the next utterance's recognizer NOW, while the
                // user reads their committed text, so helper spawn latency
                // (60-220ms measured) never lands inside a dictation.
                rec = make_recognizer().ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asr::backends::mock::MockRecognizer;

    fn voiced(secs: f32) -> Vec<f32> {
        (0..(secs * 16_000.0) as usize)
            .map(|i| 0.3 * (i as f32 * 0.2).sin())
            .collect()
    }

    #[tokio::test]
    async fn feeds_and_finalizes_through_the_thread() {
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let feed = spawn(|| Ok(Box::new(MockRecognizer::new()) as _), etx, rtx);
        assert_eq!(rrx.await.unwrap().unwrap(), "mock");

        for chunk in voiced(3.0).chunks(1600) {
            feed.push(chunk.to_vec());
        }
        feed.finalize();

        let mut saw_partial = false;
        loop {
            match erx.recv().await.expect("worker closed early") {
                AsrEvent::Partial(_) => saw_partial = true,
                AsrEvent::Final(t) => {
                    let t = t.unwrap();
                    assert!(!t.text.is_empty(), "voiced audio must transcribe");
                    break;
                }
            }
        }
        assert!(saw_partial, "streamer must have emitted partials");
        assert_eq!(feed.dropped_chunks(), 0);
    }

    #[tokio::test]
    async fn constructor_failure_reports_via_ready() {
        let (etx, _erx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let _feed = spawn(|| anyhow::bail!("helper not found"), etx, rtx);
        let err = rrx.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("helper not found"));
    }
}
