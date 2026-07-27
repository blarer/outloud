//! Platform dispatch. Exactly one backend compiles per target; the others
//! are documented stubs so their designs (and their traps) live next to the
//! code that will implement them.

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod linux;

use std::sync::mpsc::Sender;

use crate::matcher::Matcher;
use crate::taphold::TapHold;
use crate::{HotkeyError, HotkeyEvent};

/// Start the platform listener. Returns once the OS-level tap/hook/grab is
/// confirmed installed, so a caller that gets Ok can trust the binding is
/// live (not merely queued), which matters for the "never silently dead"
/// requirement.
pub fn spawn(
    matcher: Matcher,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    #[cfg(target_os = "macos")]
    return macos::spawn(matcher, machine, sender);

    #[cfg(target_os = "windows")]
    return windows::spawn(matcher, machine, sender);

    #[cfg(all(unix, not(target_os = "macos")))]
    return linux::spawn(matcher, machine, sender);

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let _ = (matcher, machine, sender);
        Err(HotkeyError::Unsupported(
            "no hotkey backend for this platform",
        ))
    }
}
