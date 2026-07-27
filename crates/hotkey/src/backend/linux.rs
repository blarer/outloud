//! Linux backend: STUB, with the honest platform story.
//!
//! **X11** is straightforward: `XGrabKey` on the root window delivers
//! KeyPress AND KeyRelease for a chord globally, and grabs are exclusive so
//! a failed grab (BadAccess) IS the conflict detection. Bare modifiers need
//! `XISelectEvents` for raw key events instead, since XGrabKey grabs
//! keycode+modifier combinations, not modifier keys alone.
//!
//! **Wayland has no global hotkey protocol.** This is a design decision of
//! the protocol, not an oversight: a client seeing keys typed into other
//! clients is exactly the keylogger capability Wayland exists to prevent.
//! The honest options, all of which push the binding OUT of this process:
//!
//! - The **GlobalShortcuts XDG desktop portal**
//!   (org.freedesktop.portal.GlobalShortcuts): the app requests a named
//!   action, the COMPOSITOR owns the actual key binding and delivers
//!   Activated/Deactivated signals over DBus. Deactivated exists, so
//!   push-to-talk works. Implemented by KDE and by GNOME >= 45; not
//!   universal across wlroots compositors.
//! - **Compositor config**: instruct the user to bind a key in sway/Hyprland
//!   config to `exec ourcli press` / release, i.e. we ship a tiny IPC
//!   trigger instead of listening ourselves.
//! - **evdev / libinput** direct read: works everywhere including the
//!   console, but requires the user to be in the `input` group, which is a
//!   real privilege grant (it IS system-wide key visibility) and must be an
//!   informed opt-in, never a silent install step.
//!
//! Plan: X11 XGrabKey/XI2 first (still the majority of remote/older
//! desktops), portal on Wayland with the compositor-config fallback
//! documented, evdev as an explicit power-user opt-in.

use std::sync::mpsc::Sender;

use crate::matcher::Matcher;
use crate::taphold::TapHold;
use crate::{HotkeyError, HotkeyEvent};

pub fn spawn(
    _matcher: Matcher,
    _machine: TapHold,
    _sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    Err(HotkeyError::Unsupported(
        "Linux backend not yet implemented (planned: X11 XGrabKey/XI2; Wayland has no \
         global hotkey protocol - needs the GlobalShortcuts portal or compositor config)",
    ))
}
