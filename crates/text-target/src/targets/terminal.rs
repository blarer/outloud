//! Tier 5: terminal-native transports.
//!
//! Terminals are the reason this crate exists. None of them exposes a
//! writable accessibility text field, the "field" is a character grid owned
//! by whatever program is running inside, so the higher tiers can at best
//! type at them blindly. But terminals grew their own control planes, and
//! several allow the one thing accessibility never gives us for a shell:
//! **reading the current command line back**, which is what turns dictation
//! into edit-by-voice.
//!
//! Who can read the line buffer:
//!
//! - **tmux**: `capture-pane` reads the whole visible pane, and
//!   `display-message` can report cursor position. Best-in-class, works over
//!   SSH, no display server needed. Implemented below.
//! - **bash**: a `bind -x` widget sees `READLINE_LINE` / `READLINE_POINT`
//!   and may assign them, true read-modify-write of the live prompt, but
//!   only from *inside* the shell, so it needs a shell integration snippet
//!   that talks to our daemon (see [`crate::targets::headless`]).
//! - **zsh**: same shape, a `zle` widget reads and writes `$BUFFER` and
//!   `$CURSOR`. `zle` widgets can be invoked by a signal trap (`TRAPUSR1`),
//!   which is how an external process triggers the round trip.
//! - **fish**: `commandline` prints the buffer, `commandline -r` replaces
//!   it, and `commandline -f repaint` redraws. Uniquely, `fish` can be told
//!   to run this remotely via `fish -c` only for *its own* process through
//!   a keybinding, so it also needs the integration snippet.
//! - **kitty**: `kitten @ get-text --extent screen` reads the pane like
//!   tmux does, when `allow_remote_control` is enabled.
//! - **iTerm2 / WezTerm**: pane text is readable via the Python API /
//!   `wezterm cli get-text` respectively.
//! - **OSC 52 read** exists in the spec but ships disabled nearly
//!   everywhere, and reads the *clipboard*, not the line, so it does not
//!   solve this even where enabled.
//!
//! Everything else (plain Terminal.app, Alacritty, GNOME Terminal, xterm,
//! a raw ConPTY) is write-only from the outside: bracketed paste delivers
//! text safely, and reading requires the shell-side integration.

use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::escape;
use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> Result<String, TargetError> {
    let out = cmd.stdin(Stdio::null()).output()?;
    if !out.status.success() {
        return Err(TargetError::Transport(format!(
            "{:?} exited with {}: {}",
            cmd.get_program(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    String::from_utf8(out.stdout)
        .map_err(|_| TargetError::Transport("command output is not UTF-8".into()))
}

/// tmux: the strongest terminal target and the only fully implemented
/// read-and-write one.
///
/// `send-keys` types into the active pane; `load-buffer` + `paste-buffer -p`
/// delivers arbitrary text as a bracketed paste, which is why paste is the
/// default write path; `capture-pane -p` reads the pane contents back.
/// All of it works headless and over SSH because tmux is its own server.
pub struct TmuxTarget {
    /// Target pane in tmux syntax; `None` means the active pane of the
    /// current client, which is right when we run inside the same tmux.
    pane: Option<String>,
}

impl TmuxTarget {
    pub fn new(pane: Option<String>) -> Self {
        TmuxTarget { pane }
    }

    /// Inside a tmux client, or able to reach a running server. `$TMUX` is
    /// the cheap check; falling back to `tmux has-session` would also catch
    /// the outside-attach case but spawns a process, so detection keeps to
    /// the env var and callers construct this directly for the remote case.
    pub fn available() -> bool {
        std::env::var_os("TMUX").is_some() && which("tmux")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new("tmux");
        c.args(args);
        if let Some(pane) = &self.pane {
            c.args(["-t", pane]);
        }
        c
    }
}

impl TextTarget for TmuxTarget {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            // Reads the pane, writes keystrokes/paste: it cannot surgically
            // rewrite the line, only clear and retype it (C-u then paste),
            // which loses shell undo (readline's own C-_ stack).
            can_write_in_place: true,
            preserves_undo: false,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        // -p prints to stdout; trailing blank lines are grid padding, not
        // content, so they are trimmed rather than handed to the editor.
        let text = run(&mut self.cmd(&["capture-pane", "-p"]))?;
        Ok(Snapshot {
            text: text.trim_end_matches('\n').to_string(),
            selection: None,
        })
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        // load-buffer from stdin avoids every quoting hazard send-keys has.
        let mut load = Command::new("tmux");
        load.args(["load-buffer", "-b", "text-target", "-"]);
        let mut child = load.stdin(Stdio::piped()).spawn()?;
        child
            .stdin
            .as_mut()
            .expect("stdin was requested piped")
            .write_all(text.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(TargetError::Transport(format!(
                "tmux load-buffer exited with {status}"
            )));
        }
        // -p pastes bracketed when the pane's program enabled bracketed
        // paste, exactly the safety property we want at a shell prompt.
        run(&mut self.cmd(&["paste-buffer", "-d", "-p", "-b", "text-target"]))?;
        Ok(())
    }

    fn replace(&mut self, text: &str) -> Result<(), TargetError> {
        // C-u kills the whole line in readline, zle, and fish alike. This
        // replaces the *line*, not arbitrary pane text, which is the edit
        // region a shell actually has.
        run(&mut self.cmd(&["send-keys", "C-u"]))?;
        self.insert(text)
    }
}

/// WezTerm via `wezterm cli`: read with `get-text`, write with `send-text`.
///
/// `send-text` delivers as a bracketed paste by default, and the CLI talks
/// to the mux server over a unix socket, so like tmux it works from scripts
/// and over SSH domains.
pub struct WezTermTarget {
    pane_id: Option<String>,
}

impl WezTermTarget {
    pub fn new(pane_id: Option<String>) -> Self {
        WezTermTarget { pane_id }
    }

    pub fn available() -> bool {
        std::env::var_os("WEZTERM_PANE").is_some() && which("wezterm")
    }

    fn cmd(&self, sub: &str) -> Command {
        let mut c = Command::new("wezterm");
        c.args(["cli", sub]);
        if let Some(id) = &self.pane_id {
            c.args(["--pane-id", id]);
        }
        c
    }
}

impl TextTarget for WezTermTarget {
    fn name(&self) -> &'static str {
        "wezterm-cli"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            preserves_undo: false,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        let text = run(&mut self.cmd("get-text"))?;
        Ok(Snapshot {
            text: text.trim_end_matches('\n').to_string(),
            selection: None,
        })
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        let mut c = self.cmd("send-text");
        let mut child = c.stdin(Stdio::piped()).spawn()?;
        child
            .stdin
            .as_mut()
            .expect("stdin was requested piped")
            .write_all(text.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(TargetError::Transport(format!(
                "wezterm cli send-text exited with {status}"
            )));
        }
        Ok(())
    }

    fn replace(&mut self, text: &str) -> Result<(), TargetError> {
        // --no-paste sends raw bytes, so C-u (0x15) arrives as the
        // kill-line keystroke rather than as pasted text.
        let mut c = self.cmd("send-text");
        c.arg("--no-paste");
        let mut child = c.stdin(Stdio::piped()).spawn()?;
        child
            .stdin
            .as_mut()
            .expect("stdin was requested piped")
            .write_all(b"\x15")?;
        let status = child.wait()?;
        if !status.success() {
            return Err(TargetError::Transport(format!(
                "wezterm cli send-text exited with {status}"
            )));
        }
        self.insert(text)
    }
}

/// OSC 52: set the system clipboard by writing an escape sequence to the
/// controlling terminal. Write-only in practice; see
/// [`escape::osc52_query_clipboard`] for why reads are dead on arrival.
///
/// This is the transport that makes "copy on a remote SSH host, paste
/// locally" work with zero setup, so it pairs with the clipboard tier: set
/// via OSC 52, then paste with a local keystroke or Cmd-V by hand.
pub struct Osc52Target {
    /// Wrap in DCS passthrough when inside tmux, otherwise tmux eats the
    /// sequence. Captured at construction because it is an env property.
    inside_tmux: bool,
}

impl Osc52Target {
    pub fn new() -> Self {
        Osc52Target {
            inside_tmux: std::env::var_os("TMUX").is_some(),
        }
    }

    /// Only meaningful when a controlling terminal exists.
    pub fn available() -> bool {
        std::path::Path::new("/dev/tty").exists()
    }

    fn write_to_tty(&self, seq: &[u8]) -> Result<(), TargetError> {
        // /dev/tty rather than stdout: stdout may be redirected, and this
        // sequence is for the terminal, not for the data stream.
        let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
        let wrapped;
        let bytes = if self.inside_tmux {
            wrapped = escape::tmux_passthrough(seq);
            &wrapped
        } else {
            seq
        };
        tty.write_all(bytes)?;
        tty.flush()?;
        Ok(())
    }
}

impl Default for Osc52Target {
    fn default() -> Self {
        Self::new()
    }
}

impl TextTarget for Osc52Target {
    fn name(&self) -> &'static str {
        "osc52"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: false,
            can_write_in_place: false,
            preserves_undo: false,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::NotReadable(
            "OSC 52 reads the clipboard, not the line, and emulators ship reads disabled",
        ))
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        self.write_to_tty(&escape::osc52_set_clipboard(text))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "OSC 52 only sets the clipboard; it cannot touch existing text",
        ))
    }
}

/// Bracketed paste written straight to the controlling terminal's input
/// side is not possible from a sibling process: only the terminal emulator
/// itself can inject into the pty master. What *is* possible is writing to
/// `/dev/tty`, which reaches the pty **slave** and comes back as if the
/// program printed it, useless for input.
///
/// So this target requires an explicit pty file descriptor path (as used
/// when we *own* the pty, e.g. wrapping a shell in filter mode), and
/// otherwise reports itself unavailable rather than pretending.
pub struct BracketedPasteTarget {
    /// Path to the pty master we own, e.g. from a ConPTY/openpty wrapper.
    pty_master: Option<std::path::PathBuf>,
}

impl BracketedPasteTarget {
    pub fn new(pty_master: Option<std::path::PathBuf>) -> Self {
        BracketedPasteTarget { pty_master }
    }
}

impl TextTarget for BracketedPasteTarget {
    fn name(&self) -> &'static str {
        "bracketed-paste"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::insert_only(true)
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::NotReadable(
            "a pty master carries keystrokes in, not buffer contents out",
        ))
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        let Some(path) = &self.pty_master else {
            return Err(TargetError::Unsupported(
                "no pty master owned; only the emulator can inject into a foreign pty",
            ));
        };
        let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
        f.write_all(&escape::bracketed_paste(text))?;
        f.flush()?;
        Ok(())
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "bracketed paste is insert-only; line replacement needs the shell's line editor",
        ))
    }
}

/// kitty remote control. Stub with the shape known.
///
/// Needs `allow_remote_control yes` (and usually a `listen_on` socket) in
/// kitty.conf, then `kitten @ send-text` writes and `kitten @ get-text
/// --extent screen` reads. Full read-and-write parity with tmux once the
/// user opts in; without the config it refuses every command, which is why
/// this stays a stub until detection can distinguish the two states cheaply
/// (`kitten @ ls` succeeds iff remote control is on).
pub struct KittyTarget;

impl KittyTarget {
    pub fn available() -> bool {
        std::env::var_os("KITTY_WINDOW_ID").is_some() && which("kitten")
    }
}

impl TextTarget for KittyTarget {
    fn name(&self) -> &'static str {
        "kitty-remote-control"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            preserves_undo: false,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "kitty remote control not yet wired up; needs allow_remote_control in kitty.conf",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "kitty remote control not yet wired up; needs allow_remote_control in kitty.conf",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "kitty remote control not yet wired up; needs allow_remote_control in kitty.conf",
        ))
    }
}

/// iTerm2. Stub.
///
/// Two routes: the Python API (a websocket the user must enable in
/// Preferences > API, full session read/write via
/// `session.async_get_screen_contents` and `async_send_text`), and the
/// proprietary escape codes (OSC 1337, write-oriented, see
/// [`escape::iterm2_copy_to_clipboard`]). The Python API is the real one;
/// it requires spawning a helper because the protocol needs an
/// authenticated websocket handshake with cookies iTerm2 issues per-launch.
pub struct Iterm2Target;

impl Iterm2Target {
    pub fn available() -> bool {
        std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "iTerm.app")
    }
}

impl TextTarget for Iterm2Target {
    fn name(&self) -> &'static str {
        "iterm2-api"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
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
            "iTerm2 Python API client not yet implemented; needs the API enabled in Preferences",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "iTerm2 Python API client not yet implemented; needs the API enabled in Preferences",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "iTerm2 Python API client not yet implemented; needs the API enabled in Preferences",
        ))
    }
}

/// GNU screen. Stub.
///
/// `screen -X paste .` pastes the paste buffer into the active window and
/// `screen -X readbuf file` loads it from a file, so write works via a
/// temp file; `screen -X hardcopy file` dumps the window for read. All of
/// it works headless like tmux, but through temp files rather than pipes,
/// which is why tmux is implemented first and screen documented.
pub struct ScreenTarget;

impl ScreenTarget {
    pub fn available() -> bool {
        std::env::var_os("STY").is_some() && which("screen")
    }
}

impl TextTarget for ScreenTarget {
    fn name(&self) -> &'static str {
        "gnu-screen"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: false,
            preserves_undo: false,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "GNU screen backend not yet implemented; needs readbuf/hardcopy via temp files",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "GNU screen backend not yet implemented; needs readbuf/hardcopy via temp files",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "GNU screen backend not yet implemented; needs readbuf/hardcopy via temp files",
        ))
    }
}

/// Windows ConPTY. Stub.
///
/// When we *own* the pseudoconsole (`CreatePseudoConsole`), writing to its
/// input pipe is exactly the pty-master case: bracketed paste in, VT
/// stream out, and reading the buffer means parsing our own VT output or
/// asking the shell (PSReadLine has no external query API). For a foreign
/// console, `WriteConsoleInput` on an attached console handle can inject
/// keys, and the legacy `ReadConsoleOutput` can read the cell grid, both
/// require `AttachConsole(pid)`, which detaches our own console, so it
/// belongs in a helper process.
pub struct ConPtyTarget;

impl TextTarget for ConPtyTarget {
    fn name(&self) -> &'static str {
        "windows-conpty"
    }

    fn tier(&self) -> Tier {
        Tier::TerminalNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: false,
            preserves_undo: false,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "ConPTY backend not yet implemented; needs AttachConsole in a helper process",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "ConPTY backend not yet implemented; needs AttachConsole in a helper process",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "ConPTY backend not yet implemented; needs AttachConsole in a helper process",
        ))
    }
}
