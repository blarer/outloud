//! Single-instance enforcement for the daemon.
//!
//! Two daemons is not a cosmetic problem. Both bind the same hotkey, both
//! open the microphone, and one keypress puts both into `listening`, so the
//! user is recorded twice and their text is injected twice into the field
//! they are focused on. Observed directly:
//!
//! ```text
//! === A ===                        === B ===
//! aquad: state idle                aquad: state idle
//! aquad: state listening    <---   aquad: state listening    <--- both hot
//! ```
//!
//! It is easy to reach by accident rather than contrived. The quickstart
//! tells users to run the binary directly to watch logs, which is exactly
//! how a second copy ends up alongside one already started from the `.app`.
//!
//! ## Why `flock` rather than a pid file we parse
//!
//! The obvious design, write our pid and have the next process check whether
//! that pid is alive, is wrong in the case that matters most: a daemon killed
//! with `SIGKILL`, or one that panicked, never gets to clean up. The next
//! launch then finds a pid file naming a dead process, or worse, a live
//! process that has since inherited that pid, and has to guess.
//!
//! An advisory `flock` has no such state. The lock is owned by the open file
//! description and the kernel drops it when the process dies, however it
//! dies. So "is another daemon running?" becomes a question with an
//! authoritative answer rather than a heuristic. The pid is still written
//! into the file, but only as a human-readable courtesy so the error message
//! can name the process to quit; correctness never depends on it.
//!
//! ## Why the lock is not taken for `--once`
//!
//! `--once` is a measurement: run one utterance, print timings, exit. Those
//! are run concurrently in benchmarks and CI, and they neither bind the
//! hotkey nor stay resident, so two of them cannot fight over the things
//! this guard protects.

use std::io::{Read, Seek, Write};
use std::path::PathBuf;

/// A held single-instance lock. The lock lives as long as this value: drop
/// it, or exit the process, and the next daemon may start.
///
/// The file handle is deliberately kept even though nothing reads it again.
/// Dropping it would close the descriptor and release the kernel's lock,
/// which is the entire mechanism.
#[derive(Debug)]
pub struct InstanceLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl InstanceLock {
    /// The lock file's path, for diagnostics.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Remove the file so a `ls` of the runtime directory reflects
        // reality. Best-effort: the kernel has already released the lock by
        // the time the descriptor closes, so a failure here costs nothing
        // but a stray empty file, and racing another daemon that has just
        // created its own must not produce an error on this path.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum Error {
    /// Another daemon holds the lock. Carries its pid when the lock file was
    /// readable, which it normally is; `None` means the holder had not
    /// finished writing it, which is a harmless race.
    AlreadyRunning { pid: Option<u32> },
    /// The lock file could not be created or locked for some other reason.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Every error message names the next action: the user is not
            // debugging a lock, they are trying to start their dictation
            // tool and need to know why it refused.
            Error::AlreadyRunning { pid: Some(pid) } => write!(
                f,
                "aquad is already running (pid {pid}). Quit it from the menu bar, \
                 or `kill {pid}`, then start this one. Running two copies makes \
                 both record you and both type what you said."
            ),
            Error::AlreadyRunning { pid: None } => write!(
                f,
                "aquad is already running. Quit it from the menu bar, or \
                 `pkill -f aquad`, then start this one."
            ),
            Error::Io(e) => write!(f, "could not take the single-instance lock: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Where the lock file lives: `$XDG_RUNTIME_DIR`, else the temp directory.
///
/// Not beside the config: this is ephemeral machine state, and putting it in
/// `~/.config/aqua` would mean a crash leaves litter in a directory the user
/// is invited to read, edit, and check into a dotfiles repository.
fn lock_path() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => std::env::temp_dir(),
    };
    dir.join("aquad.lock")
}

/// Take the single-instance lock, or report who already holds it.
pub fn acquire() -> Result<InstanceLock, Error> {
    acquire_at(lock_path())
}

/// `acquire`, against an explicit path, so tests do not fight each other or
/// the developer's own running daemon.
pub fn acquire_at(path: PathBuf) -> Result<InstanceLock, Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(Error::Io)?;

    if !try_lock_exclusive(&file) {
        // Held by someone else. Read their pid for the message, but never
        // depend on it: the holder may not have written it yet.
        let mut text = String::new();
        let pid = file
            .read_to_string(&mut text)
            .ok()
            .and_then(|_| text.trim().parse::<u32>().ok());
        return Err(Error::AlreadyRunning { pid });
    }

    // Ours. Record the pid so the next process can name us in its error.
    // Truncate first: the previous holder's pid is longer than ours if it
    // had more digits, and a stale tail would parse wrongly.
    file.set_len(0).map_err(Error::Io)?;
    file.rewind().map_err(Error::Io)?;
    write!(file, "{}", std::process::id()).map_err(Error::Io)?;
    file.flush().map_err(Error::Io)?;

    Ok(InstanceLock { _file: file, path })
}

/// `flock(LOCK_EX | LOCK_NB)`. True when the lock was taken.
///
/// Hand-written rather than pulling in a crate: this is one libc call, and
/// the daemon already links libc. `EWOULDBLOCK` means someone else holds it,
/// which is the expected answer rather than an error.
#[cfg(unix)]
fn try_lock_exclusive(file: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `fd` is valid for as long as `file` is borrowed, and `flock`
    // only reads it.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Non-unix has no `flock`. Returning true keeps the daemon working rather
/// than refusing to start on a platform where the guard is not implemented;
/// Windows should use a named mutex when its backends are exercised on real
/// hardware.
#[cfg(not(unix))]
fn try_lock_exclusive(_file: &std::fs::File) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own lock path: they run in parallel, and a shared
    /// path would make them contend with each other rather than with the
    /// thing under test.
    fn temp_lock(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aquad-test-{}-{}.lock", name, std::process::id()))
    }

    #[test]
    fn a_lock_can_be_taken_and_records_our_pid() {
        let path = temp_lock("basic");
        let _ = std::fs::remove_file(&path);

        let lock = acquire_at(path.clone()).expect("an unheld lock must be available");
        assert_eq!(lock.path(), path);

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.trim().parse::<u32>().unwrap(),
            std::process::id(),
            "the lock file must name the holder so the next process can report it"
        );
    }

    #[test]
    fn releasing_the_lock_lets_the_next_one_in() {
        // The contract that makes restart-after-quit work. If dropping did
        // not release, a user who quit and relaunched would be told the
        // daemon is already running, which is worse than no guard at all.
        let path = temp_lock("sequential");
        let _ = std::fs::remove_file(&path);

        let first = acquire_at(path.clone()).expect("first acquisition");
        drop(first);

        acquire_at(path.clone()).expect("the lock must be free once the holder is gone");
    }

    #[test]
    fn the_file_is_cleaned_up_on_release() {
        let path = temp_lock("cleanup");
        let _ = std::fs::remove_file(&path);

        let lock = acquire_at(path.clone()).expect("first acquisition");
        assert!(path.exists(), "a held lock has a file");
        drop(lock);
        assert!(!path.exists(), "releasing removes the file");
    }

    /// The case the guard exists for. A second acquisition of a lock that is
    /// still held must fail, and must name the holder's pid, because the
    /// whole value of this feature is the error message.
    #[test]
    #[cfg(unix)]
    fn a_second_instance_is_refused_and_told_which_pid_to_quit() {
        let path = temp_lock("contended");
        let _ = std::fs::remove_file(&path);

        let _held = acquire_at(path.clone()).expect("first acquisition");

        // flock is per open-file-description, and two descriptors in ONE
        // process still contend, so this genuinely exercises the refusal
        // path without spawning a child.
        match acquire_at(path.clone()) {
            Err(Error::AlreadyRunning { pid }) => {
                assert_eq!(
                    pid,
                    Some(std::process::id()),
                    "the refusal must name the pid a user has to quit"
                );
            }
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("a held lock must not be handed out twice"),
        }
    }

    #[test]
    fn the_refusal_message_names_an_action() {
        // A message that says only "already running" leaves the user stuck:
        // the daemon has no Dock icon, so "which one, and how do I stop it?"
        // is a real question.
        let with_pid = Error::AlreadyRunning { pid: Some(4321) }.to_string();
        assert!(with_pid.contains("4321"), "{with_pid}");
        assert!(with_pid.contains("kill 4321"), "{with_pid}");
        assert!(with_pid.contains("menu bar"), "{with_pid}");

        let without = Error::AlreadyRunning { pid: None }.to_string();
        assert!(without.contains("pkill"), "{without}");
    }

    #[test]
    fn a_stale_lock_file_from_a_dead_process_does_not_block_startup() {
        // The failure mode a parsed pid file has and flock does not: a
        // daemon killed with SIGKILL leaves its file behind with a pid that
        // is either dead or since reused. The kernel released the lock when
        // the process died, so the file's contents are irrelevant and the
        // next daemon must start normally.
        let path = temp_lock("stale");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "999999").unwrap();

        let lock = acquire_at(path.clone()).expect("a stale file must not block startup");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.trim().parse::<u32>().unwrap(),
            std::process::id(),
            "the stale pid must be replaced, not appended to"
        );
        drop(lock);
    }
}
