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
    /// Audio arrived, but the whole utterance was silence.
    ///
    /// The reported case: AirPods paired to this Mac while actually playing
    /// from an iPhone. A headset holds one audio link at a time, so macOS
    /// still lists the device, the stream still opens, and the samples are
    /// all zero. Every existing signal says success. From the user's side
    /// the key does nothing and no error appears, which is indistinguishable
    /// from the app being broken.
    ///
    /// Carries the user-facing line, because "no audio" without a probable
    /// cause just relocates the confusion.
    SilentCapture { message: String },
}

/// Peak amplitude below which a chunk is treated as digital silence.
///
/// Not zero: a live microphone in a quiet room still returns dither and
/// noise floor, typically above 1e-4. A stream that is genuinely not
/// capturing returns exact zeroes or values orders of magnitude smaller, so
/// this threshold separates "nobody spoke" from "nothing is connected"
/// without needing either to be calibrated.
const SILENCE_PEAK: f32 = 1e-5;

/// Tracks open->first-sample latency per utterance and which devices have
/// already been warned about.
///
/// Also remembers *which* devices measured slow, so the warm-hold can be
/// applied only to them. A device is judged by what it actually did on
/// this machine, not by its name or transport: a "Bluetooth" allowlist
/// would both miss slow USB interfaces and punish fast headsets.
pub struct StartupWatch {
    opened_at: Option<Instant>,
    /// Devices already warned about, by name. A device that recovers (fast
    /// again after a warning) is not un-warned: hands-free profile latency
    /// varies call to call, and flapping warnings train users to ignore
    /// them.
    warned: Vec<String>,
    /// The device capture reported most recently (CaptureUp).
    device: Option<String>,
    /// Devices measured slower than the pre-roll window at least once.
    slow: Vec<String>,
    /// Whether any chunk this utterance rose above the silence floor.
    heard_anything: bool,
    /// Devices already warned about for silence, kept separate from
    /// `warned` because the two faults are unrelated: a headset can be fast
    /// AND silent, and being told about one should not suppress the other.
    silent_warned: Vec<String>,
}

impl StartupWatch {
    pub fn new() -> StartupWatch {
        StartupWatch {
            opened_at: None,
            warned: Vec::new(),
            device: None,
            slow: Vec::new(),
            heard_anything: false,
            silent_warned: Vec::new(),
        }
    }

    /// The stream was just opened (key-down).
    pub fn on_open(&mut self, now: Instant) {
        self.opened_at = Some(now);
        // Per utterance, not per session: a headset that worked a minute ago
        // can be stolen by a phone between one keypress and the next.
        self.heard_anything = false;
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

    /// Observe one chunk of captured audio.
    ///
    /// Cheap by construction: a peak scan over a chunk already in cache,
    /// with an early exit once anything audible is found, so the common
    /// case (the user is talking) stops at the first non-trivial sample.
    pub fn on_audio(&mut self, samples: &[f32]) {
        if self.heard_anything {
            return;
        }
        if samples.iter().any(|s| s.abs() > SILENCE_PEAK) {
            self.heard_anything = true;
        }
    }

    /// The utterance ended. Was anything actually captured?
    ///
    /// Deliberately judged here rather than per chunk: real speech begins
    /// with silence while the speaker draws breath, so a chunk-level test
    /// would fire on every normal utterance. Only the complete utterance
    /// separates "they had not started yet" from "this device is not
    /// delivering audio at all".
    pub fn on_utterance_end(&mut self) -> Verdict {
        if self.heard_anything {
            return Verdict::Fine;
        }
        let device = self.device.clone().unwrap_or_else(|| "microphone".into());
        if self.silent_warned.iter().any(|d| d == &device) {
            // Already told them once. Repeating it every utterance is how a
            // useful warning becomes noise the user learns to ignore.
            return Verdict::Fine;
        }
        self.silent_warned.push(device.clone());
        Verdict::SilentCapture {
            message: format!(
                "no audio captured from \"{device}\" -> if this is a Bluetooth \
                 headset, it may be connected to another device (a phone or \
                 tablet); disconnect it there, or pick a different input in \
                 System Settings > Sound"
            ),
        }
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
        if !self.slow.contains(&device) {
            self.slow.push(device.clone());
        }
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

impl StartupWatch {
    /// Whether the current device has ever delivered its first sample
    /// later than the pre-roll window can cover.
    ///
    /// The warm-hold consults this so a fast device never pays the
    /// privacy cost: holding the stream open on a built-in microphone
    /// would light the recording indicator for no benefit at all.
    pub fn current_device_is_slow(&self) -> bool {
        self.device.as_ref().is_some_and(|d| self.slow.contains(d))
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

    /// The reported fault: AirPods paired to this Mac while playing from an
    /// iPhone. The stream opens, reports success, and delivers zeroes.
    #[test]
    fn an_utterance_of_pure_silence_is_reported_with_a_cause() {
        let mut w = StartupWatch::new();
        w.on_device("Jessie's AirPods");
        w.on_open(Instant::now());
        for _ in 0..50 {
            w.on_audio(&[0.0f32; 512]);
        }
        match w.on_utterance_end() {
            Verdict::SilentCapture { message } => {
                assert!(message.contains("AirPods"), "names the device: {message}");
                assert!(
                    message.contains("another device"),
                    "names the probable cause: {message}"
                );
            }
            other => panic!("expected SilentCapture, got {other:?}"),
        }
    }

    /// The false positive that would make this feature worse than useless.
    ///
    /// Real speech starts quiet: the speaker draws breath, the room has a
    /// noise floor. If a near-silent opening triggered the warning, it would
    /// fire on ordinary dictation and users would learn to ignore it, which
    /// costs more than never having built it.
    #[test]
    fn a_quiet_room_with_real_speech_is_not_silence() {
        let mut w = StartupWatch::new();
        w.on_device("MacBook Pro Microphone");
        w.on_open(Instant::now());
        // Several chunks of noise floor, then actual speech.
        for _ in 0..20 {
            w.on_audio(&[1e-6f32; 512]);
        }
        w.on_audio(&[0.02f32; 512]);
        assert_eq!(w.on_utterance_end(), Verdict::Fine);
    }

    /// Warn once per device, not once per utterance. A warning that repeats
    /// every time becomes noise the user filters out, taking the useful
    /// warnings with it.
    #[test]
    fn the_silence_warning_does_not_nag() {
        let mut w = StartupWatch::new();
        w.on_device("Some Headset");

        w.on_open(Instant::now());
        w.on_audio(&[0.0f32; 512]);
        assert!(matches!(
            w.on_utterance_end(),
            Verdict::SilentCapture { .. }
        ));

        w.on_open(Instant::now());
        w.on_audio(&[0.0f32; 512]);
        assert_eq!(
            w.on_utterance_end(),
            Verdict::Fine,
            "same device, second failure: already warned"
        );
    }

    /// State must reset per utterance: a headset can be stolen by a phone
    /// between one keypress and the next, so hearing audio once must not
    /// vouch for every later utterance.
    #[test]
    fn hearing_audio_once_does_not_vouch_for_the_next_utterance() {
        let mut w = StartupWatch::new();
        w.on_device("Some Headset");

        w.on_open(Instant::now());
        w.on_audio(&[0.05f32; 512]);
        assert_eq!(w.on_utterance_end(), Verdict::Fine);

        w.on_open(Instant::now());
        w.on_audio(&[0.0f32; 512]);
        assert!(
            matches!(w.on_utterance_end(), Verdict::SilentCapture { .. }),
            "the next utterance is judged on its own audio"
        );
    }
}
