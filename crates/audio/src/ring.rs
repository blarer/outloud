//! Single-producer single-consumer sample ring buffer.
//!
//! The producer is the cpal audio callback, which runs on a realtime thread
//! with hard rules: no locks held while blocking, no allocation, no
//! unbounded work. The consumer is the segmenter thread. A `Mutex` around a
//! fixed `VecDeque` satisfies those rules in practice because the critical
//! section is a bounded memcpy and the consumer never holds the lock while
//! doing recognition work. If profiling ever shows priority-inversion
//! stalls, this swaps for `rtrb` without changing the API; we start with the
//! simplest thing that cannot lose correctness.
//!
//! Overrun policy: drop *oldest* samples. For live dictation, stale audio is
//! worse than a small gap, because every retained stale second adds a second
//! of user-visible latency forever after. The drop counter is exposed so the
//! pipeline can report honest numbers instead of silently degrading.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Shared state between [`Producer`] and [`Consumer`].
struct Shared {
    buf: Mutex<std::collections::VecDeque<f32>>,
    capacity: usize,
    /// Total samples dropped due to overrun, for diagnostics.
    dropped: AtomicU64,
}

/// Writing half, owned by the audio callback.
pub struct Producer {
    shared: Arc<Shared>,
}

/// Reading half, owned by the segmenter thread.
pub struct Consumer {
    shared: Arc<Shared>,
}

/// Create a ring holding `capacity` samples (e.g. `16_000 * 10` for 10s).
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    let shared = Arc::new(Shared {
        buf: Mutex::new(std::collections::VecDeque::with_capacity(capacity)),
        capacity,
        dropped: AtomicU64::new(0),
    });
    (
        Producer {
            shared: Arc::clone(&shared),
        },
        Consumer { shared },
    )
}

impl Producer {
    /// Clone for *serial* reuse across capture-stream rebuilds.
    ///
    /// The ring is conceptually SPSC, so `Producer` deliberately does not
    /// implement `Clone`. The capture supervisor, however, replaces its
    /// stream over time (device hotplug) and each new stream needs a
    /// producer while the old one is being torn down. That is still one
    /// *active* writer at a time, which the mutex tolerates even if the
    /// teardown briefly overlaps. Named loudly so nobody mistakes it for
    /// multi-producer support.
    pub fn serial_clone(&self) -> Producer {
        Producer {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Append samples, discarding the oldest on overflow.
    pub fn push(&self, samples: &[f32]) {
        let mut buf = self.shared.buf.lock().expect("ring lock poisoned");
        let incoming = samples.len();
        let overflow = (buf.len() + incoming).saturating_sub(self.shared.capacity);
        if overflow > 0 {
            // Drop oldest: latency beats completeness for live dictation.
            let drop_n = overflow.min(buf.len());
            buf.drain(..drop_n);
            self.shared
                .dropped
                .fetch_add(overflow as u64, Ordering::Relaxed);
        }
        // If a single callback exceeds capacity, keep only the newest tail.
        let start = incoming.saturating_sub(self.shared.capacity);
        buf.extend(&samples[start..]);
    }
}

impl Consumer {
    /// Pop up to `out.len()` samples, returning how many were written.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let mut buf = self.shared.buf.lock().expect("ring lock poisoned");
        let n = out.len().min(buf.len());
        for slot in out.iter_mut().take(n) {
            *slot = buf.pop_front().expect("len checked");
        }
        n
    }

    /// Samples currently buffered.
    pub fn len(&self) -> usize {
        self.shared.buf.lock().expect("ring lock poisoned").len()
    }

    /// True when no samples are buffered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total samples lost to overrun since creation.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_samples_in_order() {
        let (px, cx) = ring(8);
        px.push(&[1.0, 2.0, 3.0]);
        let mut out = [0.0; 3];
        assert_eq!(cx.pop(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert!(cx.is_empty());
    }

    #[test]
    fn overrun_drops_oldest_and_counts() {
        let (px, cx) = ring(4);
        px.push(&[1.0, 2.0, 3.0, 4.0]);
        px.push(&[5.0, 6.0]); // 1.0 and 2.0 must go
        assert_eq!(cx.dropped(), 2);
        let mut out = [0.0; 4];
        assert_eq!(cx.pop(&mut out), 4);
        assert_eq!(out, [3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn giant_push_keeps_newest_tail() {
        let (px, cx) = ring(3);
        px.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let mut out = [0.0; 3];
        assert_eq!(cx.pop(&mut out), 3);
        assert_eq!(out, [3.0, 4.0, 5.0]);
    }
}
