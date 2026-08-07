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

/// Why this tier refuses on a platform with no paste-keystroke synthesis.
///
/// One constant, used both by the pre-flight check and by the keystroke
/// path's own fallback, so the two cannot drift into disagreeing about
/// whether pasting is possible.
const NO_PASTE_KEYSTROKE: &str =
    "paste keystroke synthesis needs the synthetic-keys tier on this platform";

/// Whether this build has a paste-keystroke path compiled into it.
///
/// The single place that answers the question, so `ensure_paste_supported`
/// and `send_paste_keystroke` cannot drift apart.
fn platform_can_paste() -> bool {
    cfg!(target_os = "macos") || cfg!(all(target_os = "windows", feature = "display"))
}

/// Which external tools move text in and out of the clipboard here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// macOS `pbcopy` / `pbpaste`.
    Pasteboard,
    /// Wayland `wl-copy` / `wl-paste`.
    WlClipboard,
    /// X11 `xclip -selection clipboard`.
    Xclip,
    /// Windows `clip.exe` for copy, PowerShell `Get-Clipboard` for paste.
    /// Shelling out keeps the parity with the other platforms (no clipboard
    /// crate, no window handle ownership) at the cost of PowerShell startup
    /// on the read path, acceptable because reads happen once per edit to
    /// save the user's clipboard, not in a hot loop.
    WinClip,
}

impl Backend {
    fn detect() -> Option<Backend> {
        Self::detect_with_env(&crate::detect::SystemEnv)
    }

    /// Backend choice as a function of a described [`Env`], so selection
    /// (`select()`) and construction (`detect_with_env`) consult the same
    /// facts. With a hardcoded probe of the real machine, a simulated
    /// Wayland environment selected clipboard-paste and then failed to
    /// construct it on any host without a clipboard tool, which is exactly
    /// the select/construct drift the transport matrix exists to forbid.
    fn detect_with_env(env: &dyn crate::detect::Env) -> Option<Backend> {
        if cfg!(target_os = "macos") {
            return Some(Backend::Pasteboard);
        }
        if cfg!(target_os = "windows") {
            // clip.exe ships with every Windows since Vista; PowerShell
            // with every Windows since 7. No probe needed.
            return Some(Backend::WinClip);
        }
        if env.var("WAYLAND_DISPLAY").is_some() && env.has_command("wl-copy") {
            return Some(Backend::WlClipboard);
        }
        if env.var("DISPLAY").is_some() && env.has_command("xclip") {
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
            Backend::WinClip => Command::new("clip.exe"),
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
            Backend::WinClip => {
                let mut c = Command::new("powershell.exe");
                // -Raw: no trailing newline appended per line. NoProfile
                // keeps startup out of the user's profile scripts.
                c.args(["-NoProfile", "-Command", "Get-Clipboard -Raw"]);
                c
            }
        }
    }
}

/// Clipboard-based delivery: set clipboard, synthesize paste, restore.
pub struct ClipboardTarget {
    backend: Backend,
    saved: Option<String>,
    /// Whether this build can synthesize the paste keystroke.
    ///
    /// A field rather than a `cfg!` read inline, so the refusal path is
    /// reachable in a test on any host. The bug this guards against only
    /// occurs where pasting is impossible, which is precisely where the
    /// developer's machine cannot run the test.
    can_paste: bool,
}

impl ClipboardTarget {
    /// Errors rather than defaulting when no clipboard tool exists, because
    /// a clipboard target that silently drops text is worse than none.
    pub fn new() -> Result<Self, TargetError> {
        Self::new_with_env(&crate::detect::SystemEnv)
    }

    /// [`ClipboardTarget::new`] against an explicit environment, so
    /// `detect_with_env` builds from the same facts `select()` decided on.
    pub fn new_with_env(env: &dyn crate::detect::Env) -> Result<Self, TargetError> {
        let backend = Backend::detect_with_env(env).ok_or(TargetError::Unsupported(
            "no clipboard tool found (need pbcopy, wl-copy, or xclip)",
        ))?;
        Ok(ClipboardTarget {
            backend,
            saved: None,
            can_paste: platform_can_paste(),
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

    /// Refuse unless this build can synthesize the paste keystroke.
    fn ensure_paste_supported(&self) -> Result<(), TargetError> {
        if self.can_paste {
            Ok(())
        } else {
            Err(TargetError::Unsupported(NO_PASTE_KEYSTROKE))
        }
    }

    fn send_paste_keystroke(&self) -> Result<(), TargetError> {
        // macOS: post Cmd+V ourselves via CGEvent when trusted. This is both
        // ~100ms faster than spawning osascript and more reliable: System
        // Events keystroke synthesis is TCC-gated against *osascript*, not
        // against us, so on a correctly-configured machine the osascript
        // route fails with "osascript is not allowed to send keystrokes"
        // while our own grant works (see ax-edit::synth module docs).
        #[cfg(target_os = "macos")]
        if ax_edit::is_trusted(false) && ax_edit::synth::press_cmd_v().is_ok() {
            return Ok(());
        }
        // Untrusted (or the CGEvent post refused): System Events is the
        // remaining hope, and its failure message names the real fix.
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
        #[cfg(all(target_os = "windows", feature = "display"))]
        {
            return send_ctrl_v();
        }
        #[allow(unreachable_code)]
        Err(TargetError::Unsupported(NO_PASTE_KEYSTROKE))
    }
}

/// Ctrl+V through SendInput, all four edges in one atomic batch so a real
/// keystroke cannot interleave and turn our paste into ctrl+shift+v.
#[cfg(all(target_os = "windows", feature = "display"))]
fn send_ctrl_v() -> Result<(), TargetError> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    const VK_CONTROL: u16 = 0x11;
    const VK_V: u16 = 0x56;

    let key = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        key(VK_CONTROL, false),
        key(VK_V, false),
        key(VK_V, true),
        key(VK_CONTROL, true),
    ];
    // SAFETY: fixed-size INPUT array, not retained past the call.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(TargetError::Transport(
            "SendInput(ctrl+V) was blocked (UIPI: elevated window in focus?)".into(),
        ));
    }
    Ok(())
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
        // Refuse BEFORE touching the clipboard. This tier looks portable --
        // `insert` is not cfg-gated and every backend can copy -- but the
        // paste keystroke is macOS/Windows only. Discovering that after the
        // copy left the user with no text delivered and their clipboard
        // destroyed, which is strictly worse than not running at all.
        self.ensure_paste_supported()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A target that believes it cannot paste, regardless of host.
    ///
    /// The bug being guarded happens only where paste synthesis is missing,
    /// which on this project is Linux -- a platform the developer's machine
    /// cannot execute. Injecting the capability makes the refusal path
    /// reachable everywhere, so the test actually runs in CI and locally
    /// instead of being compiled out exactly where it matters.
    fn target_that_cannot_paste() -> ClipboardTarget {
        ClipboardTarget {
            backend: Backend::Pasteboard,
            saved: None,
            can_paste: false,
        }
    }

    /// `insert` must refuse BEFORE it writes to the clipboard.
    ///
    /// This tier's `insert` is not cfg-gated, so it reads as portable. Before
    /// the fix it copied the text in and only then found it could not paste,
    /// leaving nothing typed and the user's clipboard destroyed -- worse than
    /// declining outright. `saved` staying `None` is the observable proof the
    /// refusal came first: `insert` fills it immediately before the first
    /// write, so a `Some` here means the clipboard had already been read to
    /// be overwritten.
    #[test]
    fn refuses_before_touching_the_clipboard() {
        let mut target = target_that_cannot_paste();
        let err = target.insert("hello").expect_err("cannot paste here");
        assert!(
            matches!(err, TargetError::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
        assert!(
            target.saved.is_none(),
            "clipboard was saved, so it had already been overwritten"
        );
    }

    /// The capability field must reflect the platform for a real target.
    ///
    /// Without this, `can_paste` could be wired to a constant and the test
    /// above would still pass while the shipped binary misbehaved.
    ///
    /// Constructing a real target needs a clipboard TOOL (`pbcopy`,
    /// `wl-copy`, `xclip`), which a headless Linux CI runner does not have.
    /// An `expect` here therefore failed the Linux job while passing on
    /// macOS -- the exact blind spot the cross-target checks exist for, and
    /// which they could not catch because they lint without running tests.
    /// No backend is not a defect in what this asserts, so it skips.
    #[test]
    fn a_real_target_reports_the_platform_capability() {
        let Ok(target) = ClipboardTarget::new() else {
            // No clipboard tool on this machine; nothing to assert about.
            return;
        };
        assert_eq!(
            target.can_paste,
            platform_can_paste(),
            "the capability field disagrees with the compiled keystroke path"
        );
    }
}
