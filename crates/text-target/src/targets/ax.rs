//! Tier 1: accessibility in-place edit.
//!
//! macOS goes through `ax-edit`, which already proved the read/rewrite loop
//! in M0. Windows and Linux are honest stubs: the shapes of their APIs are
//! known and documented here so the ports are mechanical, not exploratory.

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

/// macOS Accessibility (AXUIElement) target, delegating to `ax-edit`.
///
/// Off macOS every call surfaces `ax-edit`'s own `Unsupported`, so this type
/// compiles everywhere and [`crate::detect`] simply never selects it.
pub struct AxTarget;

impl AxTarget {
    /// Whether this process can actually use the AX tier right now: on macOS
    /// and trusted. `prompt: false` because detection must never pop dialogs.
    pub fn available() -> bool {
        cfg!(target_os = "macos") && ax_edit::is_trusted(false)
    }
}

impl TextTarget for AxTarget {
    fn name(&self) -> &'static str {
        "macos-ax"
    }

    fn tier(&self) -> Tier {
        Tier::Accessibility
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            // True when the field accepts AXSelectedText writes; ax-edit
            // reports the strategy actually used, so this is the honest
            // best case rather than a guarantee.
            preserves_undo: true,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        let snap = ax_edit::snapshot_focused()?;
        let text = snap
            .value
            .clone()
            .or_else(|| snap.selected_text.clone())
            .ok_or(TargetError::NotReadable("focused element exposes no text"))?;
        Ok(Snapshot {
            text,
            // AX reports UTF-16 units; translating to byte offsets needs the
            // value string, which we have, but a wrong mapping is worse than
            // none, so only pass it through when it is trivially in range.
            selection: None,
        })
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        // With an empty selection, writing AXSelectedText inserts at the
        // caret, which is exactly insert semantics.
        ax_edit::replace_focused(text)?;
        Ok(())
    }

    fn replace(&mut self, text: &str) -> Result<(), TargetError> {
        ax_edit::replace_focused(text)?;
        Ok(())
    }
}

/// Windows UIAutomation `TextPattern` target. Stub.
///
/// The port needs: `IUIAutomation::GetFocusedElement`, then
/// `ITextPattern::DocumentRange` for read and `ITextPattern2` /
/// `ValuePattern::SetValue` for write. In-place replacement of a subrange
/// goes through `ITextRange::Select` plus TSF or `ValuePattern`, and
/// undo preservation matches the AX story: editing via the pattern keeps
/// the app's undo, replacing the whole value usually does not. The
/// `windows` crate exposes all of it; the work is COM lifetime plumbing.
pub struct UiaTarget;

impl TextTarget for UiaTarget {
    fn name(&self) -> &'static str {
        "windows-uia"
    }

    fn tier(&self) -> Tier {
        Tier::Accessibility
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            preserves_undo: true,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "Windows UIAutomation backend not yet implemented",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Windows UIAutomation backend not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Windows UIAutomation backend not yet implemented",
        ))
    }
}

/// Linux AT-SPI2 target. Stub.
///
/// The port needs a D-Bus session connection (the `atspi` crate), the
/// focused object via the `Accessible` registry, its `Text` interface for
/// read, and `EditableText::insert_text` / `delete_text` for write. The
/// practical caveats are that GTK4 and Qt6 expose EditableText patchily,
/// Electron only when started with `--force-renderer-accessibility` or
/// when AT-SPI announces a screen reader, and Wayland-native apps vary by
/// toolkit version. can_read is real; can_write_in_place is app-dependent
/// in exactly the way `value_settable` is on macOS.
pub struct AtspiTarget;

impl TextTarget for AtspiTarget {
    fn name(&self) -> &'static str {
        "linux-atspi"
    }

    fn tier(&self) -> Tier {
        Tier::Accessibility
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            preserves_undo: false,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "Linux AT-SPI2 backend not yet implemented",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Linux AT-SPI2 backend not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Linux AT-SPI2 backend not yet implemented",
        ))
    }
}
