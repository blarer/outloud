//! Passive input-device startup-latency watchdog (docs/input-latency.md,
//! option 1).
//!
//! The microphone opens on key-down and closes on commit, so whatever a
//! device takes to deliver its first sample lands between the keypress and
//! the first audio anything can see. Audio the device never captured cannot
//! be recovered by any downstream buffer: on a slow device (Bluetooth
//! hands-free negotiation, hundreds of ms) the first word is not dropped
//! but *misrecognised* (measured: 200ms of lost head turned "quick" into
//! "Like"), which the user reads as bad recognition, not a slow headset.
//!
//! Handling is option 1 from the doc, the honest minimum: measure what the
//! device actually does, in vivo, and make the silent failure loud. Every
//! utterance the pipeline stamps open-time at key-down and first-chunk time
//! on arrival; when the gap exceeds the segmenter's 150ms pre-roll window
//! (the point past which onsets are genuinely lost), the user is told once
//! per device, with the measured number and the workaround.
//!
//! No extra stream open, no timers, no cost on the fast path beyond two
//! `Instant`s per utterance.

use std::time::{Duration, Instant};

/// First audio later than this after stream open clips word onsets: the
/// segmenter's pre-roll (150ms, `SegmenterConfig::pre_roll_frames`) can
/// only recover audio that was captured late, not audio never captured.
pub const PRE_ROLL_WINDOW: Duration = Duration::from_millis(150);

/// What one utterance's measurement concluded.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// First audio inside the pre-roll window (or no measurement ran).
    Fine,
    /// First audio arrived late enough to clip onsets, and this device has
    /// not been warned about yet. Carries the user-facing warning line.
    SlowFirstSample { message: String },
    /// Late again, but this device was already warned about. Logged by the
    /// caller, never re-surfaced: a warning per utterance is nagging.
    SlowAgain { latency: Duration },
}

/// Tracks open->first-sample latency per utterance and which devices have
/// already been warned about.
pub struct StartupWatch {
    opened_at: Option<Instant>,
    /// Devices already warned about, by name. A device that recovers (fast
    /// again after a warning) is not un-warned: hands-free profile latency
    /// varies call to call, and flapping warnings train users to ignore
    /// them.
    warned: Vec<String>,
    /// The device capture reported most recently (CaptureUp).
    device: Option<String>,
}

impl StartupWatch {
    pub fn new() -> StartupWatch {
        StartupWatch {
            opened_at: None,
            warned: Vec::new(),
            device: None,
        }
    }

    /// The stream was just opened (key-down).
    pub fn on_open(&mut self, now: Instant) {
        self.opened_at = Some(now);
    }

    /// Capture reported which device actually won.
    pub fn on_device(&mut self, name: &str) {
        if self.device.as_deref() != Some(name) {
            self.device = Some(name.to_string());
        }
    }

    /// The utterance ended without audio ever arriving; stop the clock so
    /// a stale open time cannot leak into the next utterance's measurement.
    pub fn on_close(&mut self) {
        self.opened_at = None;
    }

    /// First audio chunk of this utterance arrived. Returns the verdict;
    /// subsequent chunks are free (the `take` empties the slot).
    pub fn on_first_audio(&mut self, now: Instant) -> Verdict {
        let Some(opened) = self.opened_at.take() else {
            return Verdict::Fine;
        };
        let latency = now.duration_since(opened);
        if latency <= PRE_ROLL_WINDOW {
            return Verdict::Fine;
        }
        let device = self.device.clone().unwrap_or_else(|| "microphone".into());
        if self.warned.contains(&device) {
            return Verdict::SlowAgain { latency };
        }
        self.warned.push(device.clone());
        Verdict::SlowFirstSample {
            message: format!(
                "{device} takes {}ms to start capturing (pre-roll covers {}ms), so it \
                 can clip the first word -> hold the key a beat before speaking",
                latency.as_millis(),
                PRE_ROLL_WINDOW.as_millis()
            ),
        }
    }
}

impl Default for StartupWatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Instant {
        Instant::now()
    }

    #[test]
    fn fast_device_stays_quiet() {
        let mut w = StartupWatch::new();
        let t0 = t();
        w.on_open(t0);
        assert_eq!(
            w.on_first_audio(t0 + Duration::from_millis(71)),
            Verdict::Fine
        );
    }

    #[test]
    fn slow_device_warns_once_with_the_number() {
        let mut w = StartupWatch::new();
        w.on_device("AirPods Pro");
        let t0 = t();
        w.on_open(t0);
        match w.on_first_audio(t0 + Duration::from_millis(320)) {
            Verdict::SlowFirstSample { message } => {
                assert!(message.contains("AirPods Pro"), "{message}");
                assert!(message.contains("320ms"), "{message}");
                assert!(message.contains("hold the key"), "{message}");
            }
            v => panic!("expected a warning, got {v:?}"),
        }
        // Second slow utterance on the same device: logged, not re-surfaced.
        let t1 = t();
        w.on_open(t1);
        assert!(matches!(
            w.on_first_audio(t1 + Duration::from_millis(400)),
            Verdict::SlowAgain { .. }
        ));
    }

    #[test]
    fn a_new_device_gets_its_own_warning() {
        let mut w = StartupWatch::new();
        w.on_device("AirPods");
        let t0 = t();
        w.on_open(t0);
        assert!(matches!(
            w.on_first_audio(t0 + Duration::from_millis(300)),
            Verdict::SlowFirstSample { .. }
        ));
        w.on_device("Studio Display Microphone");
        let t1 = t();
        w.on_open(t1);
        assert!(matches!(
            w.on_first_audio(t1 + Duration::from_millis(200)),
            Verdict::SlowFirstSample { .. }
        ));
    }

    #[test]
    fn close_without_audio_discards_the_clock() {
        let mut w = StartupWatch::new();
        let t0 = t();
        w.on_open(t0);
        w.on_close();
        // Next utterance's first chunk must not be measured against the
        // previous utterance's open.
        assert_eq!(w.on_first_audio(t0 + Duration::from_secs(5)), Verdict::Fine);
    }

    #[test]
    fn chunks_without_an_open_are_ignored() {
        // File-driven runs (--wav) feed chunks with no mic open at all.
        let mut w = StartupWatch::new();
        assert_eq!(w.on_first_audio(t()), Verdict::Fine);
    }
}
