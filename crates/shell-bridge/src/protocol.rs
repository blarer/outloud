//! Line-framed protocol between shell plugins and the bridge daemon.
//!
//! The controlling constraint: a client must be implementable from a shell
//! script using only `printf`, `base64`, and `nc -U`. That rules out JSON
//! (no parser in POSIX sh), length-prefixed framing (no binary reads), and
//! multi-round-trip handshakes (each `nc` invocation is one connection).
//! What survives is one request line, one response line, connection closed.
//!
//! Payloads are base64 because command lines legally contain newlines
//! (zsh multi-line buffers, heredocs) and the protocol is line-framed.
//! Base64 also means no quoting or escaping rules for a shell to get wrong.
//!
//! ## Requests (client -> server, exactly one per connection)
//!
//! ```text
//! EDIT v1 <shell> <cursor> <b64 buffer>   shell plugin offers its buffer
//! INTENT <b64 utterance>                  voice pipeline stages an edit
//! STATUS                                  human-readable daemon state
//! PEEK                                    last buffer a shell offered
//! ```
//!
//! `<shell>` is `bash`, `zsh`, `fish`, or `other`. It is part of the frame
//! because the shells disagree about what a cursor is: readline's
//! `READLINE_POINT` counts bytes, while zsh's `$CURSOR` and fish's
//! `commandline -C` count characters. The server normalizes.
//!
//! ## Responses (server -> client, exactly one line)
//!
//! ```text
//! REPLACE <cursor_bytes> <cursor_chars> <b64 buffer>
//! NOOP <b64 reason>
//! OK
//! BUFFER <b64 buffer>
//! STATUS <b64 text>
//! ERR <b64 reason>
//! ```
//!
//! `REPLACE` carries the new cursor in *both* units so each plugin can take
//! the one its editor speaks natively without doing unicode arithmetic in
//! shell, where it is somewhere between painful and wrong.
//!
//! Nothing in this protocol can ask a shell to execute anything: there is no
//! "run" verb, and the plugins apply `REPLACE` by assigning their editor's
//! buffer variable, never by sending an accept-line. See the threat model in
//! `docs/shell-integration.md`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

/// Hard cap on one protocol line. A command line orders of magnitude longer
/// than this is not something a human is editing by voice; past it we assume
/// a confused or hostile client and drop the connection instead of buffering.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Which shell a client says it is, i.e. which cursor unit it speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Other,
}

impl Shell {
    pub fn parse(s: &str) -> Shell {
        match s {
            "bash" => Shell::Bash,
            "zsh" => Shell::Zsh,
            "fish" => Shell::Fish,
            _ => Shell::Other,
        }
    }

    /// readline exposes byte offsets; everything else here counts characters.
    pub fn cursor_is_bytes(self) -> bool {
        matches!(self, Shell::Bash)
    }
}

/// A parsed client request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Edit {
        shell: Shell,
        /// Cursor in the unit the shell speaks; normalize with
        /// [`cursor_to_chars`] before doing arithmetic.
        cursor: usize,
        buffer: String,
    },
    Intent {
        utterance: String,
    },
    Status,
    Peek,
}

/// A server response, serialized with [`Response::to_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Replace {
        cursor_bytes: usize,
        cursor_chars: usize,
        buffer: String,
    },
    Noop {
        reason: String,
    },
    Ok,
    Buffer {
        buffer: String,
    },
    Status {
        text: String,
    },
    Err {
        reason: String,
    },
}

fn b64e(s: &str) -> String {
    B64.encode(s.as_bytes())
}

fn b64d(s: &str) -> Result<String, String> {
    let bytes = B64
        .decode(s)
        .map_err(|_| "payload is not valid base64".to_string())?;
    String::from_utf8(bytes).map_err(|_| "payload is not UTF-8".to_string())
}

impl Request {
    /// Parse one request line. Errors are strings destined for an `ERR`
    /// response, so they are phrased for the human reading a shell trace.
    pub fn parse(line: &str) -> Result<Request, String> {
        let line = line.trim_end_matches(['\r', '\n']);
        let mut words = line.split(' ');
        match words.next() {
            Some("EDIT") => {
                if words.next() != Some("v1") {
                    return Err("unknown EDIT version, expected v1".into());
                }
                let shell = Shell::parse(words.next().ok_or("EDIT missing shell")?);
                let cursor: usize = words
                    .next()
                    .ok_or("EDIT missing cursor")?
                    .parse()
                    .map_err(|_| "EDIT cursor is not a number".to_string())?;
                let buffer = b64d(words.next().ok_or("EDIT missing buffer")?)?;
                if words.next().is_some() {
                    return Err("EDIT has trailing fields".into());
                }
                Ok(Request::Edit {
                    shell,
                    cursor,
                    buffer,
                })
            }
            Some("INTENT") => {
                let utterance = b64d(words.next().ok_or("INTENT missing utterance")?)?;
                Ok(Request::Intent { utterance })
            }
            Some("STATUS") => Ok(Request::Status),
            Some("PEEK") => Ok(Request::Peek),
            _ => Err("unknown verb".into()),
        }
    }
}

impl Response {
    pub fn to_line(&self) -> String {
        match self {
            Response::Replace {
                cursor_bytes,
                cursor_chars,
                buffer,
            } => format!("REPLACE {cursor_bytes} {cursor_chars} {}\n", b64e(buffer)),
            Response::Noop { reason } => format!("NOOP {}\n", b64e(reason)),
            Response::Ok => "OK\n".into(),
            Response::Buffer { buffer } => format!("BUFFER {}\n", b64e(buffer)),
            Response::Status { text } => format!("STATUS {}\n", b64e(text)),
            Response::Err { reason } => format!("ERR {}\n", b64e(reason)),
        }
    }

    /// Parse a response line; used by the CLI client and by tests.
    pub fn parse(line: &str) -> Result<Response, String> {
        let line = line.trim_end_matches(['\r', '\n']);
        let mut words = line.split(' ');
        match words.next() {
            Some("REPLACE") => {
                let cursor_bytes = words
                    .next()
                    .ok_or("REPLACE missing cursor_bytes")?
                    .parse()
                    .map_err(|_| "bad cursor_bytes".to_string())?;
                let cursor_chars = words
                    .next()
                    .ok_or("REPLACE missing cursor_chars")?
                    .parse()
                    .map_err(|_| "bad cursor_chars".to_string())?;
                let buffer = b64d(words.next().ok_or("REPLACE missing buffer")?)?;
                Ok(Response::Replace {
                    cursor_bytes,
                    cursor_chars,
                    buffer,
                })
            }
            Some("NOOP") => Ok(Response::Noop {
                reason: words.next().map(b64d).transpose()?.unwrap_or_default(),
            }),
            Some("OK") => Ok(Response::Ok),
            Some("BUFFER") => Ok(Response::Buffer {
                buffer: b64d(words.next().ok_or("BUFFER missing payload")?)?,
            }),
            Some("STATUS") => Ok(Response::Status {
                text: b64d(words.next().ok_or("STATUS missing payload")?)?,
            }),
            Some("ERR") => Ok(Response::Err {
                reason: words.next().map(b64d).transpose()?.unwrap_or_default(),
            }),
            _ => Err("unknown response verb".into()),
        }
    }
}

/// Normalize a shell-reported cursor to a character offset, clamped to the
/// buffer. Clamping matters because a plugin racing the user's typing can
/// report a cursor past the end of the buffer it also reports.
pub fn cursor_to_chars(shell: Shell, cursor: usize, buffer: &str) -> usize {
    let char_len = buffer.chars().count();
    if shell.cursor_is_bytes() {
        // Byte offset -> character offset. A byte offset inside a multi-byte
        // character (readline mid-edit can produce one) rounds down to the
        // character it is inside, which is the only answer that never panics.
        let byte = cursor.min(buffer.len());
        // A character counts only when it ends at or before the cursor byte,
        // which is what makes a mid-character offset round *down*.
        buffer
            .char_indices()
            .take_while(|(i, c)| i + c.len_utf8() <= byte)
            .count()
    } else {
        cursor.min(char_len)
    }
}

/// Character offset -> byte offset, for the `REPLACE` bytes field.
pub fn chars_to_bytes(buffer: &str, chars: usize) -> usize {
    buffer
        .char_indices()
        .nth(chars)
        .map(|(i, _)| i)
        .unwrap_or(buffer.len())
}

/// Where the cursor should land after `old` became `new`, given it sat at
/// `old_cursor_chars` (character offset) before.
///
/// `edit_intent::apply` does not report which span it edited, and this crate
/// may not change that crate. So the mapping is reconstructed from the two
/// buffers: the longest common prefix and suffix bound the changed region,
/// and the cursor either stays (before the change), shifts by the length
/// delta (after the change), or lands at the end of the rewritten region
/// (inside the change), which is where an editing human expects to resume.
pub fn map_cursor(old: &str, new: &str, old_cursor_chars: usize) -> usize {
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();

    let prefix = old_chars
        .iter()
        .zip(new_chars.iter())
        .take_while(|(a, b)| a == b)
        .count();
    // Suffix must not overlap the prefix, or "aa" -> "aaa" double counts.
    let max_suffix = old_chars.len().min(new_chars.len()) - prefix;
    let suffix = old_chars
        .iter()
        .rev()
        .zip(new_chars.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();

    let cursor = old_cursor_chars.min(old_chars.len());
    // Order matters: a cursor at the very end of the buffer satisfies both
    // the prefix and suffix conditions when text was appended ("aa" -> "aaa"
    // with cursor 2), and the user expects an append to carry the end-cursor
    // along, so the suffix rule wins ties.
    if cursor >= old_chars.len() - suffix {
        // In the unchanged tail: same distance from the end as before.
        new_chars.len() - (old_chars.len() - cursor)
    } else if cursor <= prefix {
        cursor
    } else {
        // Inside the changed region: end of the replacement text.
        new_chars.len() - suffix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_request_roundtrips_multiline_unicode() {
        let buf = "echo \"héllo\nwörld\"";
        let line = format!("EDIT v1 zsh 5 {}\n", B64.encode(buf));
        let req = Request::parse(&line).unwrap();
        assert_eq!(
            req,
            Request::Edit {
                shell: Shell::Zsh,
                cursor: 5,
                buffer: buf.into()
            }
        );
    }

    #[test]
    fn replace_response_roundtrips() {
        let r = Response::Replace {
            cursor_bytes: 7,
            cursor_chars: 6,
            buffer: "git staus\nwc -l".into(),
        };
        let line = r.to_line();
        assert_eq!(line.matches('\n').count(), 1, "must stay line-framed");
        assert_eq!(Response::parse(&line).unwrap(), r);
    }

    #[test]
    fn bad_base64_is_an_error_not_a_panic() {
        assert!(Request::parse("EDIT v1 bash 0 !!!\n").is_err());
        assert!(Request::parse("INTENT ???\n").is_err());
    }

    #[test]
    fn bash_cursor_bytes_convert_to_chars() {
        // "wö" is 3 bytes, 2 chars; byte cursor 3 = char cursor 2.
        assert_eq!(cursor_to_chars(Shell::Bash, 3, "wörld"), 2);
        // Mid-character byte offset rounds down instead of panicking.
        assert_eq!(cursor_to_chars(Shell::Bash, 2, "wörld"), 1);
        // Past-end clamps.
        assert_eq!(cursor_to_chars(Shell::Bash, 99, "wörld"), 5);
    }

    #[test]
    fn zsh_cursor_chars_clamp() {
        assert_eq!(cursor_to_chars(Shell::Zsh, 99, "wörld"), 5);
        assert_eq!(cursor_to_chars(Shell::Zsh, 3, "wörld"), 3);
    }

    #[test]
    fn cursor_before_edit_stays_put() {
        assert_eq!(map_cursor("ls -la /tmp", "ls -la /var", 3), 3);
    }

    #[test]
    fn cursor_after_edit_shifts_with_delta() {
        // cursor at end stays at end even when the buffer grew
        let old = "kubectl get pods -n prod";
        let new = "kubectl get pods -n staging";
        assert_eq!(
            map_cursor(old, new, old.chars().count()),
            new.chars().count()
        );
    }

    #[test]
    fn cursor_inside_edit_lands_after_replacement() {
        // "prod" -> "staging", cursor was inside "prod"
        let old = "echo prod now";
        let new = "echo staging now";
        // cursor at 7 (inside "prod"), changed region ends at "staging"|
        assert_eq!(map_cursor(old, new, 7), 12);
        // cursor in the unchanged tail keeps its distance from the end
        assert_eq!(map_cursor(old, new, 11), 14);
    }

    #[test]
    fn cursor_map_handles_repeated_text() {
        // "aa" -> "aaa": prefix/suffix overlap trap
        assert_eq!(map_cursor("aa", "aaa", 2), 3);
    }
}
