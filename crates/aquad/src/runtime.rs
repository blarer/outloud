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

use std::sync::atomic::{AtomicBool, Ordering};
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

    /// Arm or pause dictation, from the `enabled` setting.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
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

    #[test]
    fn clones_share_one_slot() {
        let a = RuntimeShared::new();
        let b = a.clone();
        a.set_bound_hotkey(Some("f13".into()));
        assert_eq!(b.snapshot().bound_hotkey.as_deref(), Some("f13"));
    }
}
