//! Write coalescing and backpressure.
//!
//! Two rate problems, one gate:
//!
//! - **Coalescing.** Every transport write is synchronous IPC into another
//!   process (docs/latency.md: ~264us warm AX write, milliseconds cold or
//!   in Chrome). Partials can arrive every ~30-150ms, and per-partial
//!   writes make the field stutter. The UX doc mandates a ~80ms write tick,
//!   so this gate refuses to release more than once per interval and always
//!   releases the *latest* pending state, folding everything that arrived
//!   in between into one write.
//! - **Backpressure.** If a write takes longer than the partial interval
//!   (a spinning Electron renderer, Chrome's 20x-slower AX server), a queue
//!   would grow without bound and every entry in it would already be stale
//!   by the time it lands. Stale text is worse than late text, so this
//!   gate keeps *no queue at all*: capacity is exactly one pending state,
//!   and each newcomer overwrites the last. Dropped intermediates were
//!   never going to be seen anyway, since their successors supersede them.
//!
//! The gate is a pure state machine over an injected clock (`Instant`s are
//! passed in, never taken), so every timing behaviour is unit-testable
//! without sleeping.

use std::time::{Duration, Instant};

/// Default write cadence per the UX doc ("writes batch on a ~80ms tick").
pub const DEFAULT_WRITE_INTERVAL: Duration = Duration::from_millis(80);

/// Rate-limits releases and collapses everything between them.
///
/// `T` is whatever the caller writes (typically the full desired field
/// text; the caller diffs against the last written state at release time,
/// which is what makes dropping intermediates safe: diffs are computed
/// between *actually written* states, never between dropped ones).
#[derive(Debug)]
pub struct Coalescer<T> {
    interval: Duration,
    /// The single pending slot. Newer values overwrite older ones; that
    /// overwrite *is* the backpressure policy.
    pending: Option<T>,
    /// When the last release happened. `None` before the first, so the
    /// first write goes out immediately (first-character latency is the
    /// whole game; delaying the opening write 80ms would be self-harm).
    last_release: Option<Instant>,
    /// True while the caller is inside a transport write. Offers made
    /// during a write park in `pending` and release after it finishes,
    /// which is how a slow write causes intermediates to fold together.
    in_flight: bool,
}

impl<T> Coalescer<T> {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            pending: None,
            last_release: None,
            in_flight: false,
        }
    }

    /// Offer a new desired state. Returns the state to write *now* if the
    /// interval has elapsed and no write is in flight; otherwise stores it
    /// (replacing anything already stored) and returns `None`.
    pub fn offer(&mut self, value: T, now: Instant) -> Option<T> {
        self.pending = Some(value);
        self.try_release(now)
    }

    /// Release the pending state if the cadence and in-flight rules allow.
    /// Called by the driver on its tick to flush states that were parked
    /// by an earlier `offer`.
    pub fn try_release(&mut self, now: Instant) -> Option<T> {
        if self.in_flight || self.pending.is_none() {
            return None;
        }
        let due = match self.last_release {
            None => true,
            Some(prev) => now.duration_since(prev) >= self.interval,
        };
        if !due {
            return None;
        }
        self.last_release = Some(now);
        self.in_flight = true;
        self.pending.take()
    }

    /// The caller finished (or failed) the transport write it took from
    /// `try_release`. Until this is called, no further state is released,
    /// which is what turns a slow write into dropped intermediates instead
    /// of a queue.
    pub fn write_done(&mut self, now: Instant) {
        self.in_flight = false;
        // A slow write consumes its own interval: the moment it finishes
        // counts as the last release, so the next write still waits a full
        // interval and a pathological transport is never hammered.
        self.last_release = Some(now);
    }

    /// When the earliest next release could happen, for schedulers that
    /// want to sleep exactly until then. `None` means nothing is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.pending.is_none() || self.in_flight {
            return None;
        }
        Some(match self.last_release {
            None => Instant::now(),
            Some(prev) => prev + self.interval,
        })
    }

    /// Discard any pending state and take it, ignoring the cadence. Used at
    /// end of utterance: the final commit must not wait out a tick.
    pub fn flush(&mut self, now: Instant) -> Option<T> {
        if self.in_flight {
            return None;
        }
        let v = self.pending.take();
        if v.is_some() {
            self.last_release = Some(now);
            self.in_flight = true;
        }
        v
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn first_offer_releases_immediately() {
        let mut c = Coalescer::new(DEFAULT_WRITE_INTERVAL);
        assert_eq!(c.offer(1, t0()), Some(1));
    }

    #[test]
    fn offers_within_the_interval_are_parked() {
        let now = t0();
        let mut c = Coalescer::new(DEFAULT_WRITE_INTERVAL);
        assert_eq!(c.offer(1, now), Some(1));
        c.write_done(now);
        // 30ms later: too soon, parked.
        assert_eq!(c.offer(2, now + Duration::from_millis(30)), None);
        // 79ms: still too soon even for the updated value.
        assert_eq!(c.offer(3, now + Duration::from_millis(79)), None);
        // 80ms: released, and it is the LATEST value, others were folded.
        assert_eq!(c.offer(4, now + Duration::from_millis(80)), Some(4));
    }

    #[test]
    fn slow_write_drops_intermediates_instead_of_queueing() {
        let now = t0();
        let mut c = Coalescer::new(DEFAULT_WRITE_INTERVAL);
        assert_eq!(c.offer(1, now), Some(1));
        // The write is stuck (Chrome...). Partials keep arriving well past
        // the interval; none may be released while in flight.
        assert_eq!(c.offer(2, now + Duration::from_millis(100)), None);
        assert_eq!(c.offer(3, now + Duration::from_millis(200)), None);
        assert_eq!(c.offer(4, now + Duration::from_millis(300)), None);
        // Write finally completes at 400ms.
        let done = now + Duration::from_millis(400);
        c.write_done(done);
        // Only the newest survives, and it still waits a full interval
        // from write completion so the slow transport gets breathing room.
        assert_eq!(c.try_release(done), None);
        assert_eq!(c.try_release(done + DEFAULT_WRITE_INTERVAL), Some(4));
    }

    #[test]
    fn releases_never_exceed_one_per_interval() {
        let now = t0();
        let mut c = Coalescer::new(Duration::from_millis(80));
        let mut releases = Vec::new();
        // Partials every 10ms for a second.
        for i in 0..100u32 {
            let t = now + Duration::from_millis(10 * u64::from(i));
            if let Some(v) = c.offer(i, t) {
                releases.push((t, v));
                c.write_done(t);
            }
        }
        for pair in releases.windows(2) {
            assert!(pair[1].0 - pair[0].0 >= Duration::from_millis(80));
        }
        assert!(releases.len() <= 13, "1s / 80ms plus the immediate first");
    }

    #[test]
    fn flush_ignores_cadence_but_respects_in_flight() {
        let now = t0();
        let mut c = Coalescer::new(DEFAULT_WRITE_INTERVAL);
        assert_eq!(c.offer(1, now), Some(1));
        assert_eq!(c.offer(2, now), None); // parked: in flight
        assert_eq!(c.flush(now), None, "cannot flush during a write");
        c.write_done(now);
        assert_eq!(c.offer(3, now), None); // parked: cadence
        assert_eq!(c.flush(now), Some(3), "flush skips the cadence wait");
    }

    #[test]
    fn deadline_reports_when_parked_state_becomes_due() {
        let now = t0();
        let mut c = Coalescer::new(DEFAULT_WRITE_INTERVAL);
        assert_eq!(c.next_deadline(), None);
        assert_eq!(c.offer(1, now), Some(1));
        c.write_done(now);
        c.offer(2, now + Duration::from_millis(10));
        assert_eq!(c.next_deadline(), Some(now + DEFAULT_WRITE_INTERVAL));
    }
}
