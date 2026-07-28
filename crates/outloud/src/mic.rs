//! Microphone lifetime: open it to dictate, close it the moment we stop.
//!
//! The daemon used to open the input stream at startup and hold it for the
//! whole session, discarding samples whenever the hotkey was not held. The
//! audio genuinely was thrown away, but that is invisible to the person
//! using the machine: macOS shows its orange recording indicator for as long
//! as a stream is open, so the tray said idle while the system said "this
//! app is listening to you", all day. Competing dictation tools do not do
//! that, and a user is right to read it as the tool recording them.
//!
//! "Trust me, the samples are discarded" is exactly the kind of claim this
//! product refuses to make (docs/ux/00-principles.md: settings-as-proof,
//! claims a user can check). So the stream is now opened on key-down and
//! closed on commit, which makes the orange dot mean precisely what the user
//! thinks it means: the microphone is on ONLY while they are dictating.
//!
//! The cost is stream startup latency on each utterance. That is acceptable
//! because it overlaps the user beginning to speak: nobody starts a word in
//! the same millisecond they press a key, and the VAD discards the leading
//! silence anyway.
//!
//! # Dictating while another app holds the microphone
//!
//! Dictating into a Discord or FaceTime call is a normal thing to want, and it
//! works: CoreAudio shares input devices between processes rather than handing
//! one owner an exclusive lock. Measured on macOS 26 with an `AVAudioEngine`
//! tap held open throughout, both streams captured a full 1.2M frames with no
//! error on either side (`crates/audio/tests/shared_device.rs`).
//!
//! Two decisions keep that true, and both are easy to break:
//!
//! - We never set `kAudioDevicePropertyHogMode`. Hog mode is the one call that
//!   *would* take the device exclusively, and taking it would break every call
//!   app on the machine the moment a user pressed the hotkey.
//! - We accept the device's own `default_input_config()` and resample to 16kHz
//!   ourselves, rather than requesting a rate. A process that demands a format
//!   the device is not already in can force a reconfiguration that interrupts
//!   whoever was there first.
//!
//! The open-on-keydown lifetime above helps here too: we hold the device for
//! the length of an utterance rather than the length of a session, so even a
//! driver that dislikes sharing has a small window in which to prove it.

use tokio::sync::mpsc::UnboundedSender;

use crate::runtime::RuntimeShared;
use crate::source::{self, FrontendEvent};

/// Owns the capture stream when one is open.
///
/// Not a plain `Option<CaptureHandle>` in the pipeline because the invariant
/// worth protecting is "closed unless dictating", and giving it a name means
/// the one place that opens a stream and the one place that closes it are
/// both obvious.
pub struct Mic {
    /// How to start capture. Held rather than captured once, because the
    /// stream is rebuilt for every utterance.
    events: UnboundedSender<FrontendEvent>,
    runtime: RuntimeShared,
    /// The open capture stream. Headless builds link no capture backend
    /// (`source::spawn_mic` errors there), so the slot only exists when one
    /// can: this is what keeps `--no-default-features` compiling.
    #[cfg(feature = "display")]
    open: Option<audio::capture::CaptureHandle>,
}

impl Mic {
    pub fn new(events: UnboundedSender<FrontendEvent>, runtime: RuntimeShared) -> Mic {
        Mic {
            events,
            runtime,
            #[cfg(feature = "display")]
            open: None,
        }
    }

    pub fn is_open(&self) -> bool {
        #[cfg(feature = "display")]
        {
            self.open.is_some()
        }
        #[cfg(not(feature = "display"))]
        {
            false
        }
    }

    /// Open the stream if it is not already open.
    ///
    /// Idempotent, because a key-down that arrives while an utterance is
    /// still finalizing must not stack two streams on one device.
    pub fn open(&mut self) -> anyhow::Result<()> {
        #[cfg(feature = "display")]
        {
            if self.open.is_some() {
                return Ok(());
            }
            self.open = Some(source::spawn_mic(
                self.events.clone(),
                self.runtime.clone(),
            )?);
            Ok(())
        }
        #[cfg(not(feature = "display"))]
        {
            // The headless spawn_mic names --wav as the working path.
            source::spawn_mic(self.events.clone(), self.runtime.clone())
        }
    }

    /// Close the stream, releasing the device and clearing the system's
    /// recording indicator.
    ///
    /// Called on every path that leaves the listening state, including the
    /// error paths: a failed utterance that left the microphone open would
    /// be the worst version of this bug, since it is both invisible and
    /// unbounded.
    pub fn close(&mut self) {
        #[cfg(feature = "display")]
        if let Some(handle) = self.open.take() {
            handle.stop();
        }
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        // A daemon that exits mid-utterance must not leave the device held.
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant this module exists for, stated as a test even though it
    /// cannot open a real device here: a fresh `Mic` holds nothing, so the
    /// system indicator is off until something explicitly opens it.
    #[test]
    fn a_new_mic_holds_no_stream() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mic = Mic::new(tx, RuntimeShared::new());
        assert!(!mic.is_open(), "the microphone must start closed");
    }

    #[test]
    fn closing_an_unopened_mic_is_harmless() {
        // Commit paths call close() unconditionally rather than checking
        // first, so this has to be a no-op instead of a panic.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut mic = Mic::new(tx, RuntimeShared::new());
        mic.close();
        mic.close();
        assert!(!mic.is_open());
    }
}
