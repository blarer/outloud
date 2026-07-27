//! Tier 6: headless operation, no display server at all.
//!
//! Two shapes:
//!
//! 1. **Filter mode** ([`StdioFilterTarget`]): this process sits in a pipe
//!    (`voice | text-target-filter > consumer`) or wraps a pty, reads the
//!    current buffer from stdin on demand, writes the rewrite to stdout.
//!    Zero setup, works in CI, and is how the crate is tested end to end.
//!
//! 2. **Daemon mode** ([`DaemonTarget`]): a unix socket at
//!    `$XDG_RUNTIME_DIR/text-target.sock` (or `$TMPDIR` on macOS) speaking a
//!    line protocol any editor, shell widget, or terminal can implement in a
//!    few lines of its own scripting language. This is what makes the bash
//!    `READLINE_LINE` / zsh `$BUFFER` / fish `commandline` integrations
//!    possible: the *shell* is the only process that can touch its own line
//!    editor state, so the shell registers as a client and we edit through
//!    it.
//!
//! The daemon protocol, chosen for trivial client implementation (no JSON
//! parser needed in a shell snippet), one request per line:
//!
//! ```text
//! client -> HELLO <name>                 register as the focused target
//! client -> BUFFER <base64>              report current buffer (push, on change)
//! server -> READ                         ask for the buffer now
//! server -> REPLACE <base64>             set the whole buffer
//! server -> INSERT <base64>              insert at cursor
//! client -> OK / ERR <reason>
//! ```
//!
//! Base64 because buffers contain newlines and the protocol is line-framed.
//! A bash client is ~10 lines:
//!
//! ```text
//! # ~/.bashrc: expose the readline buffer to the daemon on a hotkey
//! _tt_edit() {
//!   local reply
//!   reply=$(printf 'BUFFER %s\n' "$(printf %s "$READLINE_LINE" | base64)" \
//!           | nc -U "$XDG_RUNTIME_DIR/text-target.sock")
//!   case $reply in
//!     "REPLACE "*) READLINE_LINE=$(printf %s "${reply#REPLACE }" | base64 -d)
//!                  READLINE_POINT=${#READLINE_LINE} ;;
//!   esac
//! }
//! bind -x '"\C-g": _tt_edit'
//! ```

use std::io::{BufRead, Write};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

/// Serialize one daemon-protocol command. Public so shell-integration
/// generators and tests share the exact framing.
pub fn frame(verb: &str, payload: Option<&str>) -> String {
    match payload {
        Some(p) => format!("{verb} {}\n", B64.encode(p.as_bytes())),
        None => format!("{verb}\n"),
    }
}

/// Parse one daemon-protocol line into `(verb, decoded payload)`.
pub fn parse_frame(line: &str) -> Result<(&str, Option<String>), TargetError> {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.split_once(' ') {
        None => Ok((line, None)),
        Some((verb, b64)) => {
            let bytes = B64
                .decode(b64)
                .map_err(|_| TargetError::Transport("payload is not valid base64".into()))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| TargetError::Transport("payload is not UTF-8".into()))?;
            Ok((verb, Some(text)))
        }
    }
}

/// Filter mode over arbitrary reader/writer pairs.
///
/// Generic over the streams rather than hardcoding stdin/stdout so tests
/// drive it with in-memory buffers, the same reason `escape` returns bytes.
pub struct StdioFilterTarget<R, W> {
    input: R,
    output: W,
    /// Last buffer received; `read` returns it without blocking so the
    /// voice pipeline never stalls on a quiet upstream.
    last: Option<String>,
}

impl<R: BufRead, W: Write> StdioFilterTarget<R, W> {
    pub fn new(input: R, output: W) -> Self {
        StdioFilterTarget {
            input,
            output,
            last: None,
        }
    }

    /// Consume one protocol line from upstream, updating the cached buffer.
    /// Callers pump this from their event loop.
    pub fn pump(&mut self) -> Result<bool, TargetError> {
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Ok(false);
        }
        if let ("BUFFER", Some(text)) = parse_frame(&line)? {
            self.last = Some(text);
        }
        Ok(true)
    }
}

impl<R: BufRead, W: Write> TextTarget for StdioFilterTarget<R, W> {
    fn name(&self) -> &'static str {
        "stdio-filter"
    }

    fn tier(&self) -> Tier {
        Tier::Headless
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            // The client applies our REPLACE through its own editor state
            // (READLINE_LINE assignment participates in readline undo).
            preserves_undo: true,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        match &self.last {
            Some(text) => Ok(Snapshot {
                text: text.clone(),
                selection: None,
            }),
            None => Err(TargetError::NotReadable(
                "no BUFFER frame received yet; pump the input first",
            )),
        }
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        self.output
            .write_all(frame("INSERT", Some(text)).as_bytes())?;
        self.output.flush()?;
        Ok(())
    }

    fn replace(&mut self, text: &str) -> Result<(), TargetError> {
        self.output
            .write_all(frame("REPLACE", Some(text)).as_bytes())?;
        self.output.flush()?;
        self.last = Some(text.to_string());
        Ok(())
    }
}

/// Daemon socket client side. Stub for the transport, real for the protocol.
///
/// The remaining work is a `UnixListener` accept loop and client registry
/// (which client is "focused" when several shells register). The protocol
/// itself is fully specified and tested via [`frame`] / [`parse_frame`],
/// so shell snippets written today will not need to change.
pub struct DaemonTarget {
    socket_path: std::path::PathBuf,
}

impl DaemonTarget {
    pub fn new(socket_path: std::path::PathBuf) -> Self {
        DaemonTarget { socket_path }
    }

    /// Default socket location: `$XDG_RUNTIME_DIR` has the right lifetime
    /// and permissions on Linux; `$TMPDIR` is per-user on macOS.
    pub fn default_socket_path() -> std::path::PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .or_else(|| std::env::var_os("TMPDIR"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("text-target.sock")
    }

    pub fn available() -> bool {
        Self::default_socket_path().exists()
    }
}

impl TextTarget for DaemonTarget {
    fn name(&self) -> &'static str {
        "daemon-socket"
    }

    fn tier(&self) -> Tier {
        Tier::Headless
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            preserves_undo: true,
            is_headless: true,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        let _ = &self.socket_path;
        Err(TargetError::Unsupported(
            "daemon accept loop not yet implemented; protocol is final, see module docs",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "daemon accept loop not yet implemented; protocol is final, see module docs",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "daemon accept loop not yet implemented; protocol is final, see module docs",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrips_multiline_payload() {
        let f = frame("REPLACE", Some("line one\nline two"));
        // Line-framed: the payload's newline must not split the frame.
        assert_eq!(f.matches('\n').count(), 1);
        let (verb, payload) = parse_frame(&f).unwrap();
        assert_eq!(verb, "REPLACE");
        assert_eq!(payload.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn bare_verb_has_no_payload() {
        let (verb, payload) = parse_frame("READ\n").unwrap();
        assert_eq!(verb, "READ");
        assert_eq!(payload, None);
    }

    #[test]
    fn filter_reads_pushed_buffer_and_writes_replace() {
        let input = frame("BUFFER", Some("git sttaus"));
        let mut out: Vec<u8> = Vec::new();
        let mut t = StdioFilterTarget::new(input.as_bytes(), &mut out);
        assert!(t.pump().unwrap());
        assert_eq!(t.read().unwrap().text, "git sttaus");

        t.replace("git status").unwrap();
        let written = String::from_utf8(out).unwrap();
        let (verb, payload) = parse_frame(&written).unwrap();
        assert_eq!(verb, "REPLACE");
        assert_eq!(payload.as_deref(), Some("git status"));
    }

    #[test]
    fn filter_without_input_is_honest_about_it() {
        let mut out: Vec<u8> = Vec::new();
        let mut t = StdioFilterTarget::new(&b""[..], &mut out);
        assert!(matches!(t.read(), Err(TargetError::NotReadable(_))));
    }
}
