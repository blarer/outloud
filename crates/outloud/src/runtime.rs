//! Facts the daemon learns at runtime that the menu bar has to report.
//!
//! The state machine in [`crate::state`] answers "what is it doing"; this
//! answers "what did it end up bound to, and to which device". Those are
//! not states: they change once, at startup or on a device change, and the
//! menu is the only surface that shows them at all. Without this, the menu
//! could only report what the *config file* asked for, which is precisely
//! the lie a user hits when the bind failed or capture fell back to a
//! different microphone.
//!
//! A mutex around a small struct, published by the pipeline thread and read
//! by the main thread once per frame, matching how
//! [`crate::state::StatusShared`] already works: locks held for nanoseconds,
//! and the reader never blocks the pipeline.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// What the daemon actually bound and opened.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Runtime {
    /// The chord the hotkey layer bound, as it displays it. `None` means the
    /// bind failed or has not happened yet.
    pub bound_hotkey: Option<String>,
    /// The input device capture is using right now.
    pub microphone: Option<String>,
    /// Capture could not open any device. Distinct from "no Accessibility":
    /// different pane, different fix.
    pub microphone_blocked: bool,
    /// The Accessibility grant is missing RIGHT NOW, as opposed to at
    /// launch.
    ///
    /// macOS revokes silently: a TCC reset, a re-sign, or an OS update can
    /// take the grant away from a running process, and the reverse is even
    /// more common because the quickstart tells people to grant it while the
    /// daemon is already running. Without a live check the daemon keeps
    /// believing whatever was true at startup: it degrades to clipboard
    /// paste while still showing a healthy glyph, or it stays broken after
    /// the user has just fixed it and concludes the fix did not work.
    pub accessibility_blocked: bool,
    /// Input Monitoring is missing right now.
    ///
    /// Separate from `accessibility_blocked` because they are separate
    /// grants with separate panes and separate consequences: without Input
    /// Monitoring the hotkey never fires at all, while without Accessibility
    /// the hotkey works and the text lands via clipboard paste. Treating
    /// them as one is what let a daemon report itself ready while its tap
    /// was dead.
    pub input_monitoring_blocked: bool,
}

/// Shared publication slot. Cloneable; every clone sees the same facts.
#[derive(Clone)]
pub struct RuntimeShared {
    inner: Arc<Mutex<Runtime>>,
    /// The master switch (`enabled` in config), read on the hotkey bridge's
    /// hot path.
    ///
    /// An atomic rather than a field in the mutex above deliberately: this
    /// is read on every key edge, and the hotkey bridge must never contend
    /// with the render loop for a lock. Paused means the key edge is
    /// dropped before any capture starts, so "paused" genuinely means the
    /// microphone is never opened rather than "recorded and discarded".
    enabled: Arc<AtomicBool>,
    /// `microphone.sensitivity`, as an atomic so a live config reload reaches
    /// the pipeline.
    ///
    /// The pipeline receives `Config` by value at startup, so anything read
    /// from that copy is frozen for the life of the process. Sensitivity was:
    /// the segmenter IS rebuilt at every key-down, but from the stale copy, so
    /// changing the setting appeared to do nothing until restart. This is the
    /// same "settings the process can adopt without a restart" channel the
    /// Pause switch uses.
    sensitivity: Arc<AtomicU8>,
    /// `silence-timeout-ms`, as an atomic for exactly the same reason as
    /// `sensitivity` above: the pipeline gets `Config` by value at startup,
    /// so a value read from that copy is frozen for the life of the process.
    ///
    /// This one is worth more than convenience. It is the safety net that
    /// force-closes the microphone when a tap-to-latch capture is never
    /// ended, and the generated config header promises edits apply live;
    /// verified on hardware that they did not, so shortening the timeout
    /// after noticing a hot microphone did nothing until a restart. The
    /// setting that limits how long we can be listening is the last one that
    /// should require restarting the thing that is listening.
    hot_mic_timeout_ms: Arc<AtomicU64>,
}

impl Default for RuntimeShared {
    fn default() -> RuntimeShared {
        RuntimeShared::new()
    }
}

impl RuntimeShared {
    pub fn new() -> RuntimeShared {
        RuntimeShared {
            inner: Arc::new(Mutex::new(Runtime::default())),
            // Armed by default. A daemon that starts paused because a field
            // defaulted to false would be indistinguishable from a broken
            // hotkey, which is the failure this whole surface exists to
            // prevent; the config's `enabled` overrides this at load.
            enabled: Arc::new(AtomicBool::new(true)),
            // Matches `Config::default().sensitivity`. Overridden at load,
            // and again on every reload.
            sensitivity: Arc::new(AtomicU8::new(50)),
            // Matches the schema default for `silence-timeout-ms`.
            // Overridden at load, and again on every reload.
            hot_mic_timeout_ms: Arc::new(AtomicU64::new(60_000)),
        }
    }

    pub fn snapshot(&self) -> Runtime {
        self.inner.lock().expect("runtime lock poisoned").clone()
    }

    pub fn set_bound_hotkey(&self, chord: Option<String>) {
        self.inner
            .lock()
            .expect("runtime lock poisoned")
            .bound_hotkey = chord;
    }

    /// Whether dictation is armed. Read on every key edge, so this is a
    /// relaxed atomic load and nothing else.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Publish the live Input Monitoring state. Positive sense and polled,
    /// for the same reason as accessibility: it is a level, and granting it
    /// while the daemon runs must clear the warning without a restart.
    pub fn set_input_monitoring(&self, granted: bool) {
        self.inner
            .lock()
            .expect("runtime lock poisoned")
            .input_monitoring_blocked = !granted;
    }

    /// Arm or pause dictation, from the `enabled` setting.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Publish `microphone.sensitivity` so a reload reaches the running
    /// pipeline. Clamped to the documented 1-100 range, because a config file
    /// is user-editable and a zero here would mean "never hear anything".
    pub fn set_sensitivity(&self, sensitivity: u8) {
        self.sensitivity
            .store(sensitivity.clamp(1, 100), Ordering::Relaxed);
    }

    /// The sensitivity the segmenter should be built with right now.
    pub fn sensitivity(&self) -> u8 {
        self.sensitivity.load(Ordering::Relaxed)
    }

    /// Publish `silence-timeout-ms` so a reload reaches the running
    /// pipeline. Clamped to the schema's documented range: a zero would
    /// mean "close the microphone immediately", i.e. dictation that can
    /// never record anything, and an unbounded value would defeat the
    /// safety net entirely.
    pub fn set_hot_mic_timeout_ms(&self, ms: u64) {
        self.hot_mic_timeout_ms
            .store(ms.clamp(1_000, 600_000), Ordering::Relaxed);
    }

    /// How long capture may run before the pipeline force-commits and
    /// closes the microphone, as of right now.
    pub fn hot_mic_timeout_ms(&self) -> u64 {
        self.hot_mic_timeout_ms.load(Ordering::Relaxed)
    }

    /// Record a working capture device. Also clears the blocked flag: a
    /// stream that came up is the definitive answer to "can we record", and
    /// leaving a stale warning row in the menu after recovery would train
    /// users to ignore it.
    pub fn set_microphone(&self, device: String) {
        let mut r = self.inner.lock().expect("runtime lock poisoned");
        r.microphone = Some(device);
        r.microphone_blocked = false;
    }

    /// Publish the live Accessibility trust state.
    ///
    /// Takes the positive sense (`trusted`) rather than a `set_blocked`
    /// pair, because unlike the microphone this is a level, not an event:
    /// it is polled, so every poll must be able to clear the flag as well as
    /// set it. That is what makes granting the permission while the daemon
    /// runs take effect without a restart.
    pub fn set_accessibility_trusted(&self, trusted: bool) {
        self.inner
            .lock()
            .expect("runtime lock poisoned")
            .accessibility_blocked = !trusted;
    }

    /// Record that capture cannot open a device at all.
    pub fn set_microphone_blocked(&self) {
        let mut r = self.inner.lock().expect("runtime lock poisoned");
        r.microphone_blocked = true;
        r.microphone = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_clears_the_blocked_warning() {
        // A menu row that says "cannot open the microphone" while dictation
        // works would teach the user to ignore the menu.
        let shared = RuntimeShared::new();
        shared.set_microphone_blocked();
        assert!(shared.snapshot().microphone_blocked);
        shared.set_microphone("Built-in".into());
        let snap = shared.snapshot();
        assert!(!snap.microphone_blocked);
        assert_eq!(snap.microphone.as_deref(), Some("Built-in"));
    }

    #[test]
    fn losing_the_device_drops_its_name() {
        // Naming a device we are not recording from is worse than naming
        // none: it reads as confirmation that the right mic is in use.
        let shared = RuntimeShared::new();
        shared.set_microphone("AirPods".into());
        shared.set_microphone_blocked();
        assert_eq!(shared.snapshot().microphone, None);
    }

    /// Granting the permission while the daemon runs must take effect
    /// without a restart. This is the common direction: the quickstart tells
    /// users to grant Accessibility, and they do it with OutLoud already
    /// running.
    #[test]
    fn regaining_trust_clears_the_block() {
        let shared = RuntimeShared::new();
        shared.set_accessibility_trusted(false);
        assert!(shared.snapshot().accessibility_blocked);
        shared.set_accessibility_trusted(true);
        assert!(!shared.snapshot().accessibility_blocked);
    }

    #[test]
    fn clones_share_one_slot() {
        let a = RuntimeShared::new();
        let b = a.clone();
        a.set_bound_hotkey(Some("f13".into()));
        assert_eq!(b.snapshot().bound_hotkey.as_deref(), Some("f13"));
    }

    /// A config reload must reach the running pipeline, not just the menu.
    ///
    /// F-8 in docs/investigations/robustness.md. The segmenter IS rebuilt at
    /// every key-down, so sensitivity was always adoptable in principle; the
    /// pipeline just read it from a `Config` copied at startup, so the reload
    /// had nowhere to land and the setting appeared restart-only.
    #[test]
    fn sensitivity_changes_are_visible_to_a_live_reader() {
        let shared = RuntimeShared::new();
        // Matches Config::default(), so a host that never sets it still
        // segments the way the schema documents.
        assert_eq!(shared.sensitivity(), 50);

        shared.set_sensitivity(80);
        assert_eq!(shared.sensitivity(), 80, "a reload must be observable");

        // A config file is user-editable, and 0 would mean "never hear
        // anything", which is indistinguishable from a broken microphone.
        shared.set_sensitivity(0);
        assert_eq!(shared.sensitivity(), 1, "clamped to the documented floor");
        shared.set_sensitivity(255);
        assert_eq!(
            shared.sensitivity(),
            100,
            "clamped to the documented ceiling"
        );

        // Clones share the value: the pipeline holds one and the menu host
        // another, which is the whole point.
        let other = shared.clone();
        shared.set_sensitivity(30);
        assert_eq!(other.sensitivity(), 30);
    }
}
