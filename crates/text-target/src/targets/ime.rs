//! Tier 2: input-method / IME injection. All stubs, documented so each port
//! is a known quantity.
//!
//! An input method sits between the keyboard and the application, so every
//! toolkit already accepts its text, including ones with broken
//! accessibility. The cost is that it is insert-at-cursor only: the IME
//! protocol has commit and preedit, not "replace that sentence over there".
//! Wayland's `zwp_input_method_v2` is the exception worth noting: it carries
//! `surrounding_text`, which gives limited read-back around the cursor.

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

/// Wayland `zwp_input_method_v2` target. Stub.
///
/// Needs: a Wayland connection (the `wayland-client` crate), binding the
/// `zwp_input_method_manager_v2` global, and the compositor's permission,
/// only one input method may be active, so this conflicts with a running
/// fcitx5/ibus unless it goes through them instead. `commit_string` inserts;
/// `delete_surrounding_text` plus `commit_string` gives replacement *near
/// the cursor*, and the `surrounding_text` event gives read-back of up to
/// 4000 bytes around it when the client app supports the protocol (GTK and
/// Qt do, many others do not). Supported by wlroots compositors and KWin;
/// GNOME Mutter only exposes it to the registered IBus process.
pub struct WaylandImeTarget;

impl TextTarget for WaylandImeTarget {
    fn name(&self) -> &'static str {
        "wayland-input-method-v2"
    }

    fn tier(&self) -> Tier {
        Tier::InputMethod
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // surrounding_text is real but partial; advertised as readable
            // so callers try it before falling back.
            can_read: true,
            can_write_in_place: true,
            preserves_undo: true,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "Wayland zwp_input_method_v2 backend not yet implemented",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Wayland zwp_input_method_v2 backend not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Wayland zwp_input_method_v2 backend not yet implemented",
        ))
    }
}

/// Windows Text Services Framework target. Stub.
///
/// Needs: registering a TSF text service (a COM in-proc server, installed
/// per-user), then `ITfInsertAtSelection` for insert and
/// `ITfRange::SetText` inside an edit session for replacement.
/// `ITfContext` gives full document read access in TSF-aware apps (Office,
/// modern UWP, browsers). Legacy apps fall through to TSF's IMM32 shim,
/// which is insert-only. The registration requirement makes this a
/// shipping-product feature, not something a spike binary can do on the fly.
pub struct TsfTarget;

impl TextTarget for TsfTarget {
    fn name(&self) -> &'static str {
        "windows-tsf"
    }

    fn tier(&self) -> Tier {
        Tier::InputMethod
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
            "Windows TSF text service not yet implemented",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Windows TSF text service not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Windows TSF text service not yet implemented",
        ))
    }
}

/// macOS Input Method Kit target. Stub.
///
/// Needs: a separate `.app` registered as an input method (InputMethodKit,
/// `IMKInputController`), which the user must select in the input menu.
/// `insertText:` commits; `client()` conforms to `IMKTextInput`, whose
/// `attributedSubstringFromRange:` gives read access and
/// `insertText:replacementRange:` gives true in-place replacement in
/// cooperative apps. Strictly better than CGEvent typing when installed,
/// but installation friction is why the AX tier is preferred on macOS.
pub struct MacInputMethodTarget;

impl TextTarget for MacInputMethodTarget {
    fn name(&self) -> &'static str {
        "macos-input-method"
    }

    fn tier(&self) -> Tier {
        Tier::InputMethod
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
            "macOS InputMethodKit backend not yet implemented",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "macOS InputMethodKit backend not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "macOS InputMethodKit backend not yet implemented",
        ))
    }
}
