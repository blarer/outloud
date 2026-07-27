//! Escape-sequence builders for terminal-native delivery.
//!
//! Kept as pure functions returning `Vec<u8>` so they are testable without a
//! terminal: the escape bytes are the contract, and getting one byte wrong
//! fails silently in some emulators and corrupts the screen in others.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

/// OSC 52: set the system clipboard through the terminal.
///
/// `Pc` selects the target selection: `c` is the clipboard, `p` the primary
/// selection. The payload is base64 because OSC bodies cannot carry arbitrary
/// bytes. Terminated with BEL rather than ST because every emulator that
/// speaks OSC 52 accepts BEL, while a few old ones mis-parse the two-byte ST.
pub fn osc52_set_clipboard(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 4 / 3 + 16);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(B64.encode(text.as_bytes()).as_bytes());
    out.push(0x07);
    out
}

/// OSC 52 query form: ask the terminal to send the clipboard back.
///
/// Most emulators ship with the *read* direction disabled (it lets any
/// program running in the terminal, including one on a remote host, exfiltrate
/// the clipboard), so callers must treat no-response as the common case and
/// time out.
pub fn osc52_query_clipboard() -> Vec<u8> {
    b"\x1b]52;c;?\x07".to_vec()
}

/// Wrap `text` in bracketed-paste markers.
///
/// A shell with bracketed paste enabled (readline and zle both enable it by
/// default now) treats everything between the markers as literal text: no
/// history expansion, no accidental execution when the payload contains a
/// newline. This is what makes writing multi-line text into a shell prompt
/// safe at all.
///
/// The markers themselves (ESC [200~ / ESC [201~) are what the *terminal*
/// sends to the application; a program writing to the pty master, or a
/// multiplexer's paste command, sends them directly.
pub fn bracketed_paste(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// iTerm2 proprietary OSC 1337 `Copy` variant: like OSC 52 but iTerm-only.
///
/// Provided because iTerm2 predates its own OSC 52 support and some
/// configurations enable only this form.
pub fn iterm2_copy_to_clipboard(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 4 / 3 + 32);
    out.extend_from_slice(b"\x1b]1337;Copy=;");
    out.extend_from_slice(B64.encode(text.as_bytes()).as_bytes());
    out.push(0x07);
    out
}

/// tmux passthrough wrapper: deliver `seq` to the *outer* terminal when
/// running inside tmux.
///
/// tmux consumes escape sequences itself, so an OSC 52 written to a pane
/// never reaches the real emulator unless wrapped in DCS tmux passthrough
/// (and `allow-passthrough` is on). Every ESC in the payload must be doubled,
/// which is the detail people get wrong.
pub fn tmux_passthrough(seq: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(seq.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in seq {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_encodes_payload_as_base64() {
        assert_eq!(osc52_set_clipboard("hi"), b"\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn osc52_query_uses_question_mark_payload() {
        assert_eq!(osc52_query_clipboard(), b"\x1b]52;c;?\x07");
    }

    #[test]
    fn bracketed_paste_wraps_without_touching_payload() {
        let out = bracketed_paste("echo hi\n");
        assert_eq!(out, b"\x1b[200~echo hi\n\x1b[201~");
    }

    #[test]
    fn tmux_passthrough_doubles_escapes() {
        let inner = osc52_query_clipboard();
        let out = tmux_passthrough(&inner);
        assert!(out.starts_with(b"\x1bPtmux;"));
        assert!(out.ends_with(b"\x1b\\"));
        // The inner sequence's single ESC must appear doubled.
        assert!(out.windows(2).any(|w| w == [0x1b, 0x1b]));
    }

    #[test]
    fn iterm2_copy_is_osc_1337() {
        let out = iterm2_copy_to_clipboard("x");
        assert!(out.starts_with(b"\x1b]1337;Copy=;"));
        assert!(out.ends_with(b"\x07"));
    }
}
