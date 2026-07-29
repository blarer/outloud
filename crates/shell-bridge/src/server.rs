//! The bridge daemon: owns the socket, holds the staged intent, answers
//! shell plugins.
//!
//! ## The interaction model
//!
//! Editing a readline/ZLE buffer from outside the shell is impossible: only
//! the shell process can touch its own line editor state. So the dataflow is
//! inverted from a normal client/server edit. The *voice pipeline* stages an
//! intent (`INTENT change prod to staging`); then the *shell*, when the user
//! presses the plugin's keybinding, offers its buffer (`EDIT`), and the
//! staged intent is applied and returned as a `REPLACE`. The shell is always
//! the party that mutates its buffer, through its own editor, which is what
//! preserves the shell's undo.
//!
//! An intent is consumed by the first EDIT that arrives (or expires), so a
//! stale voice command can never fire into tomorrow's command line.
//!
//! ## Concurrency
//!
//! One thread, sequential accept loop. Each connection is one request and
//! one response between a human keypress and a prompt redraw; concurrency
//! would buy nothing and would buy races between two shells editing against
//! one staged intent. Read timeouts keep a stalled client from wedging the
//! loop.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::peer;
use crate::protocol::{
    chars_to_bytes, cursor_to_chars, map_cursor, Request, Response, MAX_LINE_BYTES,
};

/// How long a staged intent stays valid. Long enough for "speak, then reach
/// for the keyboard"; short enough that a forgotten utterance does not
/// ambush an unrelated command line minutes later.
pub const INTENT_TTL: Duration = Duration::from_secs(30);

/// A client gets this long to send its one line. Generous for a shell
/// pipeline, stingy for a wedged one.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Default socket path: `$XDG_RUNTIME_DIR/outloud/shell.sock` where that
/// exists (Linux: correct lifetime and 0700 by contract), else a 0700 dir
/// under `$TMPDIR` (macOS: per-user by construction), else
/// `/tmp/outloud-$UID`.
///
/// `$OUTLOUD_BRIDGE_SOCKET` overrides everything, and the pre-rename
/// `$AQUA_BRIDGE_SOCKET` is still honored: an existing install exported it
/// from a shell rc, and ignoring it would split the daemon and that user's
/// plugins onto two different sockets. Drop the legacy name once no
/// pre-rename installs remain.
pub fn default_socket_path() -> PathBuf {
    if let Some(p) =
        std::env::var_os("OUTLOUD_BRIDGE_SOCKET").or_else(|| std::env::var_os("AQUA_BRIDGE_SOCKET"))
    {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("TMPDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // /tmp fallback: encode the uid so two users never collide on
            // a predictable path (a collision is a symlink-attack invitation).
            PathBuf::from(format!("/tmp/outloud-{}", unsafe { libc::geteuid() }))
        });
    base.join("outloud").join("shell.sock")
}

/// What the daemon knows between connections.
struct StagedIntent {
    utterance: String,
    staged_at: Instant,
}

pub struct Server {
    listener: UnixListener,
    socket_path: PathBuf,
    staged: Option<StagedIntent>,
    /// Last buffer any shell offered; serves PEEK so the voice pipeline can
    /// show context ("editing: kubectl get ...") before the user confirms.
    last_buffer: Option<String>,
    served: u64,
}

impl Server {
    /// Bind, with the permission dance done in the safe order: create the
    /// parent 0700, bind, then chmod the socket 0600 *before* the first
    /// accept. A socket that is briefly 0755 is briefly a command-injection
    /// hole, so order matters more than it looks.
    pub fn bind(path: &Path) -> anyhow::Result<Server> {
        let dir = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("socket path has no parent directory"))?;
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;

        // A leftover socket from a dead daemon blocks bind; a *live* daemon
        // should win, so only unlink when nothing answers.
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                anyhow::bail!("another bridge is already listening at {}", path.display());
            }
            std::fs::remove_file(path)?;
        }

        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Server {
            listener,
            socket_path: path.to_path_buf(),
            staged: None,
            last_buffer: None,
            served: 0,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Serve forever. `max_connections` bounds the loop for tests and the
    /// demo; `None` means run until killed.
    pub fn serve(&mut self, max_connections: Option<u64>) -> anyhow::Result<()> {
        loop {
            if let Some(max) = max_connections {
                if self.served >= max {
                    return Ok(());
                }
            }
            let (stream, _) = self.listener.accept()?;
            self.served += 1;
            // A single bad client must not kill the daemon: log and move on.
            if let Err(e) = self.handle(stream) {
                eprintln!("shell-bridge: connection error: {e}");
            }
        }
    }

    fn handle(&mut self, mut stream: UnixStream) -> anyhow::Result<()> {
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(READ_TIMEOUT))?;

        // The credential gate comes before reading a single byte: no parsing
        // of untrusted input from a peer we are going to reject anyway.
        if !peer::peer_is_self(&stream)? {
            let _ = stream.write_all(
                Response::Err {
                    reason: "peer uid mismatch".into(),
                }
                .to_line()
                .as_bytes(),
            );
            anyhow::bail!("rejected connection from foreign uid");
        }

        let mut line = String::new();
        let mut reader = BufReader::new((&stream).take(MAX_LINE_BYTES as u64));
        reader.read_line(&mut line)?;
        if line.len() >= MAX_LINE_BYTES {
            anyhow::bail!("request exceeds line limit");
        }

        let response = match Request::parse(&line) {
            Err(reason) => Response::Err { reason },
            Ok(req) => self.respond(req),
        };
        stream.write_all(response.to_line().as_bytes())?;
        Ok(())
    }

    fn respond(&mut self, req: Request) -> Response {
        // Expire lazily: no timers, checked at the only moment it matters.
        if let Some(s) = &self.staged {
            if s.staged_at.elapsed() > INTENT_TTL {
                self.staged = None;
            }
        }

        match req {
            Request::Intent { utterance } => {
                self.staged = Some(StagedIntent {
                    utterance,
                    staged_at: Instant::now(),
                });
                Response::Ok
            }
            Request::Status => Response::Status {
                text: format!(
                    "socket={} staged={} last_buffer={}",
                    self.socket_path.display(),
                    self.staged
                        .as_ref()
                        .map(|s| s.utterance.as_str())
                        .unwrap_or("<none>"),
                    self.last_buffer.as_deref().unwrap_or("<none>"),
                ),
            },
            Request::Peek => match &self.last_buffer {
                Some(b) => Response::Buffer { buffer: b.clone() },
                None => Response::Noop {
                    reason: "no shell has offered a buffer yet".into(),
                },
            },
            Request::Edit {
                shell,
                cursor,
                buffer,
            } => {
                self.last_buffer = Some(buffer.clone());

                // Take the intent unconditionally: even a failed apply
                // consumes it, because retrying a mismatched edit against
                // every subsequent keypress would be worse than asking the
                // user to speak again.
                let Some(staged) = self.staged.take() else {
                    return Response::Noop {
                        reason: "no edit staged; speak first".into(),
                    };
                };

                let intent = edit_intent::parse(&staged.utterance);
                match edit_intent::apply(&buffer, &intent) {
                    None => Response::Noop {
                        reason: format!("'{}' did not match the line", staged.utterance),
                    },
                    Some(new_buffer) => {
                        let old_cursor = cursor_to_chars(shell, cursor, &buffer);
                        let cursor_chars = map_cursor(&buffer, &new_buffer, old_cursor);
                        Response::Replace {
                            cursor_bytes: chars_to_bytes(&new_buffer, cursor_chars),
                            cursor_chars,
                            buffer: new_buffer,
                        }
                    }
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Leave no dangling socket for a future process to squat on.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// One-shot client: connect, send one line, read one line. This is the CLI
/// side and also documents exactly what the shell plugins do with `nc -U`.
pub fn request(path: &Path, req_line: &str) -> anyhow::Result<Response> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| anyhow::anyhow!("cannot reach bridge at {}: {e}", path.display()))?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.write_all(req_line.as_bytes())?;
    if !req_line.ends_with('\n') {
        stream.write_all(b"\n")?;
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Response::parse(&line).map_err(|e| anyhow::anyhow!("bad response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Shell;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;

    fn test_server(name: &str) -> Server {
        let path = std::env::temp_dir()
            .join(format!("sb-test-{}-{name}", std::process::id()))
            .join("shell.sock");
        Server::bind(&path).unwrap()
    }

    #[test]
    fn edit_without_intent_is_noop() {
        let mut s = test_server("noop");
        let r = s.respond(Request::Edit {
            shell: Shell::Zsh,
            cursor: 0,
            buffer: "ls".into(),
        });
        assert!(matches!(r, Response::Noop { .. }));
    }

    #[test]
    fn staged_intent_rewrites_and_is_consumed() {
        let mut s = test_server("consume");
        assert_eq!(
            s.respond(Request::Intent {
                utterance: "change prod-web to staging-web".into()
            }),
            Response::Ok
        );
        let r = s.respond(Request::Edit {
            shell: Shell::Zsh,
            cursor: 34,
            buffer: "kubectl get pods --namespace prod-web --output wide".into(),
        });
        match r {
            Response::Replace {
                buffer,
                cursor_chars,
                ..
            } => {
                assert_eq!(
                    buffer,
                    "kubectl get pods --namespace staging-web --output wide"
                );
                // Cursor at 34 sat on the "w" of "-web", in the unchanged
                // tail; it keeps its distance from the end of the buffer.
                assert_eq!(cursor_chars, 37);
            }
            other => panic!("expected Replace, got {other:?}"),
        }
        // Consumed: a second EDIT gets nothing.
        let r2 = s.respond(Request::Edit {
            shell: Shell::Zsh,
            cursor: 0,
            buffer: "ls".into(),
        });
        assert!(matches!(r2, Response::Noop { .. }));
    }

    #[test]
    fn failed_apply_still_consumes_intent() {
        let mut s = test_server("failedapply");
        s.respond(Request::Intent {
            utterance: "change xyzzy to plugh".into(),
        });
        let r = s.respond(Request::Edit {
            shell: Shell::Bash,
            cursor: 0,
            buffer: "ls -la".into(),
        });
        assert!(matches!(r, Response::Noop { .. }));
        assert!(s.staged.is_none());
    }

    #[test]
    fn multiline_buffer_survives_round_trip() {
        let mut s = test_server("multiline");
        s.respond(Request::Intent {
            utterance: "change wold to world".into(),
        });
        let buf = "for f in *.txt; do\n  echo \"hello wold: $f\"\ndone";
        let r = s.respond(Request::Edit {
            shell: Shell::Zsh,
            cursor: 0,
            buffer: buf.into(),
        });
        match r {
            Response::Replace { buffer, .. } => {
                assert_eq!(
                    buffer,
                    "for f in *.txt; do\n  echo \"hello world: $f\"\ndone"
                );
            }
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[test]
    fn end_to_end_over_real_socket() {
        let mut s = test_server("socket");
        let path = s.socket_path().to_path_buf();
        let handle = std::thread::spawn(move || s.serve(Some(2)));

        let r = request(
            &path,
            &format!("INTENT {}", B64.encode("change hello to goodbye")),
        )
        .unwrap();
        assert_eq!(r, Response::Ok);

        let r = request(
            &path,
            &format!("EDIT v1 bash 4 {}", B64.encode("echo hello wörld")),
        )
        .unwrap();
        match r {
            Response::Replace {
                buffer,
                cursor_bytes,
                cursor_chars,
            } => {
                assert_eq!(buffer, "echo goodbye wörld");
                // Cursor at byte 4 (before the edit) stays at 4/4.
                assert_eq!((cursor_bytes, cursor_chars), (4, 4));
            }
            other => panic!("expected Replace, got {other:?}"),
        }
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn socket_permissions_are_owner_only() {
        let s = test_server("perms");
        let meta = std::fs::metadata(s.socket_path()).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let dir_meta = std::fs::metadata(s.socket_path().parent().unwrap()).unwrap();
        assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn second_bind_on_live_socket_refuses() {
        let s = test_server("dupbind");
        let path = s.socket_path().to_path_buf();
        // Note: the first server is alive (listening), so bind must refuse.
        assert!(Server::bind(&path).is_err());
    }
}
