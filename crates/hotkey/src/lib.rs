//! Global hotkey layer: hold a key anywhere in the system, speak, release,
//! text appears. This is the product's primary interaction, so its failure
//! modes (dead key, stuck mic, missed release) are treated as product
//! failures, not edge cases. See docs/hotkeys.md for the platform trap list.
//!
//! Layering, deliberately testable from the bottom up:
//!
//! - [`chord`]: parse/display of human-readable bindings. Pure.
//! - [`taphold`]: the tap-latches / hold-is-PTT state machine. Pure, time is
//!   an argument.
//! - [`matcher`]: raw (event type, keycode, flags) -> chord edge. Pure.
//! - [`conflict`]: is the chord already claimed? Parsing pure; one OS probe.
//! - [`backend`]: the only OS-coupled layer. macOS CGEventTap today,
//!   Windows/Linux stubs with the intended designs written down.
//! - [`HotkeyManager`]: ties them together behind a channel of events.

pub mod backend;
pub mod chord;
pub mod conflict;
pub mod keycode;
pub mod matcher;
pub mod taphold;

pub use chord::{Chord, ChordParseError, Key, Modifier};
pub use conflict::{check_chord, Conflict, Severity};
pub use taphold::{HotkeyEvent, TapHold, Timing};

use std::fmt;
use std::sync::mpsc::{channel, Receiver};

/// What can go wrong binding a hotkey.
#[derive(Debug)]
pub enum HotkeyError {
    /// The OS refused the tap/hook, which on macOS means the responsible
    /// process lacks Accessibility (or Input Monitoring) trust. The message
    /// names the fix because "permission denied" alone costs hours
    /// (docs/macos-permissions.md).
    PermissionDenied,
    /// The chord cannot be compiled for this backend.
    BadChord(String),
    /// This platform's backend is not implemented yet.
    Unsupported(&'static str),
    /// Anything else from the OS layer.
    Backend(String),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyError::PermissionDenied => write!(
                f,
                "the OS refused the event tap: grant Accessibility permission to the \
                 responsible process (System Settings > Privacy & Security > Accessibility; \
                 if launched from a terminal, that means the TERMINAL - see \
                 docs/macos-permissions.md), or launch via `open -a`"
            ),
            HotkeyError::BadChord(msg) => write!(f, "unbindable chord: {msg}"),
            HotkeyError::Unsupported(msg) => write!(f, "{msg}"),
            HotkeyError::Backend(msg) => write!(f, "hotkey backend error: {msg}"),
        }
    }
}

impl std::error::Error for HotkeyError {}

/// The bound hotkey: owns the OS listener, exposes an event stream.
///
/// One manager per binding. Dropping the manager currently leaves the
/// backend thread running but with a disconnected channel (events go
/// nowhere); a full unbind is future work and irrelevant to a daemon whose
/// binding lives as long as the process.
pub struct HotkeyManager {
    chord: Chord,
    receiver: Receiver<HotkeyEvent>,
    conflicts: Vec<Conflict>,
}

impl HotkeyManager {
    /// Bind `chord` globally with the given timing. Conflict detection runs
    /// FIRST and its findings are carried on the manager (advisory, per the
    /// UX doc: warn loudly, let the user decide, never silently accept).
    pub fn bind(chord: Chord, timing: Timing) -> Result<HotkeyManager, HotkeyError> {
        let conflicts = conflict::check_chord(&chord);
        let matcher =
            matcher::Matcher::new(&chord).map_err(|e| HotkeyError::BadChord(e.to_string()))?;
        let machine = TapHold::new(timing);
        let (tx, rx) = channel();
        backend::spawn(matcher, machine, tx)?;
        Ok(HotkeyManager {
            chord,
            receiver: rx,
            conflicts,
        })
    }

    /// The chord this manager is bound to.
    pub fn chord(&self) -> &Chord {
        &self.chord
    }

    /// Collisions found at bind time. Non-empty does not mean the bind
    /// failed; it means the user must be told before they rely on it.
    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    /// The event stream. Blocking iterator semantics via the mpsc receiver:
    /// call `recv()` on a worker, or `try_recv()` from a poll loop.
    pub fn events(&self) -> &Receiver<HotkeyEvent> {
        &self.receiver
    }
}
