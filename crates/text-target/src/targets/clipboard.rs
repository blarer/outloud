//! Tier 4: clipboard paste with restore.
//!
//! Universally available and the only fallback that delivers arbitrary
//! unicode atomically. The user's clipboard is state they own, so the target
//! saves it before each write and offers it back through
//! [`ClipboardTarget::restore`], as a separate step because the paste
//! keystroke and the restore must be ordered around each other by the
//! caller, which knows how long the destination needs to consume the paste.
//!
//! Shells out to the platform clipboard tool (`pbcopy`/`pbpaste`,
//! `wl-copy`/`wl-paste`, `xclip`) rather than binding a clipboard crate: the
//! tools are ubiquitous, and Command keeps this crate free of display-server
//! linkage so it still builds headless.

use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

/// Which external tools move text in and out of the clipboard here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// macOS `pbcopy` / `pbpaste`.
    Pasteboard,
    /// Wayland `wl-copy` / `wl-paste`.
    WlClipboard,
    /// X11 `xclip -selection clipboard`.
    Xclip,
}

impl Backend {
    fn detect() -> Option<Backend> {
        if cfg!(target_os = "macos") {
            return Some(Backend::Pasteboard);
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && which("wl-copy") {
            return Some(Backend::WlClipboard);
        }
        if std::env::var_os("DISPLAY").is_some() && which("xclip") {
            return Some(Backend::Xclip);
        }
        None
    }

    fn copy_cmd(self) -> Command {
        match self {
            Backend::Pasteboard => Command::new("pbcopy"),
            Backend::WlClipboard => Command::new("wl-copy"),
            Backend::Xclip => {
                let mut c = Command::new("xclip");
                c.args(["-selection", "clipboard"]);
                c
            }
        }
    }

    fn paste_cmd(self) -> Command {
        match self {
            Backend::Pasteboard => Command::new("pbpaste"),
            Backend::WlClipboard => {
                let mut c = Command::new("wl-paste");
                c.arg("--no-newline");
                c
            }
            Backend::Xclip => {
                let mut c = Command::new("xclip");
                c.args(["-selection", "clipboard", "-o"]);
                c
            }
        }
    }
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

/// Clipboard-based delivery: set clipboard, synthesize paste, restore.
pub struct ClipboardTarget {
    backend: Backend,
    saved: Option<String>,
}

impl ClipboardTarget {
    /// Errors rather than defaulting when no clipboard tool exists, because
    /// a clipboard target that silently drops text is worse than none.
    pub fn new() -> Result<Self, TargetError> {
        let backend = Backend::detect().ok_or(TargetError::Unsupported(
            "no clipboard tool found (need pbcopy, wl-copy, or xclip)",
        ))?;
        Ok(ClipboardTarget {
            backend,
            saved: None,
        })
    }

    pub fn available() -> bool {
        Backend::detect().is_some()
    }

    fn get_clipboard(&self) -> Result<String, TargetError> {
        let out = self.backend.paste_cmd().output()?;
        if !out.status.success() {
            return Err(TargetError::Transport(format!(
                "clipboard read exited with {}",
                out.status
            )));
        }
        String::from_utf8(out.stdout)
            .map_err(|_| TargetError::Transport("clipboard is not UTF-8".into()))
    }

    fn set_clipboard(&self, text: &str) -> Result<(), TargetError> {
        let mut child = self.backend.copy_cmd().stdin(Stdio::piped()).spawn()?;
        child
            .stdin
            .as_mut()
            .expect("stdin was requested piped")
            .write_all(text.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(TargetError::Transport(format!(
                "clipboard write exited with {status}"
            )));
        }
        Ok(())
    }

    /// Put the user's original clipboard back. A no-op when nothing was
    /// saved, so callers can run it unconditionally after the paste lands.
    pub fn restore(&mut self) -> Result<(), TargetError> {
        if let Some(saved) = self.saved.take() {
            self.set_clipboard(&saved)?;
        }
        Ok(())
    }

    fn send_paste_keystroke(&self) -> Result<(), TargetError> {
        // macOS can do this cheaply through System Events; it needs the same
        // Accessibility grant the AX tier needs, which is fine because this
        // tier only runs once that grant already failed to find a text field.
        if cfg!(target_os = "macos") {
            let status = Command::new("osascript")
                .args([
                    "-e",
                    "tell application \"System Events\" to keystroke \"v\" using command down",
                ])
                .status()?;
            if status.success() {
                return Ok(());
            }
            return Err(TargetError::Transport(
                "osascript paste keystroke failed (Accessibility grant?)".into(),
            ));
        }
        Err(TargetError::Unsupported(
            "paste keystroke synthesis needs the synthetic-keys tier on this platform",
        ))
    }
}

impl TextTarget for ClipboardTarget {
    fn name(&self) -> &'static str {
        "clipboard-paste"
    }

    fn tier(&self) -> Tier {
        Tier::Clipboard
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: false,
            can_write_in_place: false,
            preserves_undo: false,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::NotReadable(
            "the clipboard is not the focused destination's contents",
        ))
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        // Save before clobbering; the caller restores after the destination
        // has consumed the paste.
        if self.saved.is_none() {
            self.saved = Some(self.get_clipboard().unwrap_or_default());
        }
        self.set_clipboard(text)?;
        self.send_paste_keystroke()
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "clipboard paste cannot address existing text; select it first via another tier",
        ))
    }
}
