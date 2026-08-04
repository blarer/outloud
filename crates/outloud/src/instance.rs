//! Single-instance enforcement for the daemon.
//!
//! Two daemons is not a cosmetic problem. Both bind the same hotkey, both
//! open the microphone, and one keypress puts both into `listening`, so the
//! user is recorded twice and their text is injected twice into the field
//! they are focused on. Observed directly:
//!
//! ```text
//! === A ===                        === B ===
//! outloud: state idle                outloud: state idle
//! outloud: state listening    <---   outloud: state listening    <--- both hot
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
    /// The named mutex standing in for `flock` on Windows.
    ///
    /// Held here rather than leaked so it is released when the lock is
    /// dropped, not merely when the process exits. Leaking it meant
    /// `drop(lock)` did not let the next acquisition in, which is the
    /// contract that makes quit-then-relaunch work.
    #[cfg(windows)]
    mutex: Option<WindowsMutex>,
    /// Nothing on unix: there the file descriptor itself is the lock.
    #[cfg(not(windows))]
    _platform: PlatformLock,
}

impl InstanceLock {
    /// The lock file's path, for diagnostics.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Release the mutex before removing the file, so a waiter that sees
        // the file gone also finds the mutex free.
        #[cfg(windows)]
        self.mutex.take();
        // Remove the file so a `ls` of the runtime directory reflects
        // reality. Best-effort: the kernel has already released the lock by
        // the time the descriptor closes, so a failure here costs nothing
        // but a stray empty file, and racing another daemon that has just
        // created its own must not produce an error on this path.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// An owned `CreateMutexW` handle that releases and closes on drop.
#[cfg(windows)]
#[derive(Debug)]
struct WindowsMutex(windows::Win32::Foundation::HANDLE);

// SAFETY: a Windows mutex HANDLE is not thread-affine; releasing it from a
// thread other than the creator is supported, and `InstanceLock` is moved
// between threads.
#[cfg(windows)]
unsafe impl Send for WindowsMutex {}
#[cfg(windows)]
unsafe impl Sync for WindowsMutex {}

#[cfg(windows)]
impl Drop for WindowsMutex {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        // Both are best-effort: this runs on the way out, and there is
        // nothing to do about a failure except leak, which process exit
        // cleans up anyway.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
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
                "outloud is already running (pid {pid}). Quit it from the menu bar, \
                 or `kill {pid}`, then start this one. Running two copies makes \
                 both record you and both type what you said."
            ),
            Error::AlreadyRunning { pid: None } => write!(
                f,
                "outloud is already running. Quit it from the menu bar, or \
                 `pkill -f outloud`, then start this one."
            ),
            Error::Io(e) => write!(f, "could not take the single-instance lock: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Where the lock file lives: `$XDG_RUNTIME_DIR`, else the temp directory.
///
/// Not beside the config: this is ephemeral machine state, and putting it in
/// `~/.config/outloud` would mean a crash leaves litter in a directory the user
/// is invited to read, edit, and check into a dotfiles repository.
fn lock_path() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => std::env::temp_dir(),
    };
    dir.join("outloud.lock")
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

    let Some(platform) = try_lock_exclusive(&file, &path) else {
        // Held by someone else. Read their pid for the message, but never
        // depend on it: the holder may not have written it yet.
        let mut text = String::new();
        let pid = file
            .read_to_string(&mut text)
            .ok()
            .and_then(|_| text.trim().parse::<u32>().ok());
        return Err(Error::AlreadyRunning { pid });
    };

    // Ours. Record the pid so the next process can name us in its error.
    // Truncate first: the previous holder's pid is longer than ours if it
    // had more digits, and a stale tail would parse wrongly.
    file.set_len(0).map_err(Error::Io)?;
    file.rewind().map_err(Error::Io)?;
    write!(file, "{}", std::process::id()).map_err(Error::Io)?;
    file.flush().map_err(Error::Io)?;

    Ok(InstanceLock {
        _file: file,
        path,
        #[cfg(windows)]
        mutex: Some(platform),
        #[cfg(not(windows))]
        _platform: platform,
    })
}

/// `flock(LOCK_EX | LOCK_NB)`. `Some` when the lock was taken.
///
/// Hand-written rather than pulling in a crate: this is one libc call, and
/// the daemon already links libc. `EWOULDBLOCK` means someone else holds it,
/// which is the expected answer rather than an error.
///
/// `path` is unused here because the descriptor IS the lock; it exists for
/// the Windows implementation, whose named mutex has to be derived from
/// something.
#[cfg(unix)]
fn try_lock_exclusive(file: &std::fs::File, _path: &std::path::Path) -> Option<PlatformLock> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `fd` is valid for as long as `file` is borrowed, and `flock`
    // only reads it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 } {
        Some(())
    } else {
        None
    }
}

/// What a successful lock hands back: nothing on unix (the descriptor IS the
/// lock), the owned mutex on Windows.
#[cfg(unix)]
type PlatformLock = ();
#[cfg(windows)]
type PlatformLock = WindowsMutex;
#[cfg(not(any(unix, windows)))]
type PlatformLock = ();

/// Windows has no `flock`, so the guard is a named mutex.
///
/// This returned `true` unconditionally until the Windows backends were run
/// on real hardware, which made the guard a no-op on the platform where
/// double-launching is most likely. Two daemons both bind the hotkey and
/// both open the microphone, so one keypress records twice and types twice,
/// which is exactly what this module exists to prevent.
///
/// `Global\` scope rather than `Local\`: session-local would still allow one
/// daemon per terminal-services session, and the microphone is not
/// per-session. The handle is owned by the returned [`InstanceLock`] and
/// released on drop, so quitting and relaunching within one process works;
/// it was previously leaked, which meant only process exit freed the lock.
#[cfg(windows)]
fn try_lock_exclusive(_file: &std::fs::File, path: &std::path::Path) -> Option<PlatformLock> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    // Derived from the lock PATH rather than fixed, so this matches what
    // `flock` does on unix: there the lock is a property of the file, and
    // `acquire_at` exists so callers can hold independent locks. One
    // hardcoded name made every path share a single mutex, so the
    // `acquire_at` tests contended with each other rather than with the
    // thing under test, and had never passed on Windows. Two real daemons
    // still exclude each other because they resolve the same `lock_path()`.
    //
    // Hashed rather than embedded: a mutex name cannot contain a backslash
    // beyond its namespace prefix, and is capped at MAX_PATH.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.to_string_lossy().as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let name: Vec<u16> = format!("Global\\dev.outloud.outloud.single.{hash:016x}\0")
        .encode_utf16()
        .collect();

    unsafe {
        let handle = match CreateMutexW(None, true, windows::core::PCWSTR(name.as_ptr())) {
            Ok(h) => h,
            // Cannot create the mutex at all (a sandbox denying Global\, say).
            // Allow the launch: refusing to start over a guard we could not
            // evaluate is worse than the duplicate it was meant to prevent.
            Err(_) => return Some(WindowsMutex(windows::Win32::Foundation::HANDLE::default())),
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Another daemon holds it. Close our (non-owning) handle rather
            // than leaking it until process exit.
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            return None;
        }
        // Owned by the returned value: releasing it on drop is what lets a
        // quit-then-relaunch succeed within one process. Previously the
        // handle was leaked deliberately, so the lock outlived the
        // `InstanceLock` that supposedly held it.
        Some(WindowsMutex(handle))
    }
}

/// Platforms with neither `flock` nor a named mutex keep the daemon working
/// rather than refusing to start over a guard that is not implemented.
#[cfg(not(any(unix, windows)))]
fn try_lock_exclusive(_file: &std::fs::File, _path: &std::path::Path) -> Option<PlatformLock> {
    Some(())
}

/// Kill speech helpers left over from a previous daemon, and report how many.
///
/// A helper was found on a development machine still alive, reparented to
/// launchd, nearly eight hours after its parent died, wedged in a semaphore
/// wait and holding an OS speech session:
///
/// ```text
///   PID  PPID STARTED                       ELAPSED COMMAND
/// 45677     1 Tue Jul 28 01:07:42 2026     07:46:20 .../aqua-speech-helper
/// ```
///
/// **The trigger is unknown.** The obvious hypothesis, that `SIGKILL` skips
/// the recognizer's `Drop` and leaks the child, was tested across sixteen
/// signal-and-timing combinations by two people and never reproduced: the
/// helper exits on its own when its stdin writer disappears, and there is
/// both a `Drop` that kills and reaps and an explicit kill on the
/// finalize-timeout path.
///
/// So this reaper is deliberately a *cure for the class rather than the
/// cause*. Whatever wedges a helper, a leftover one is always wrong once no
/// daemon is running, because only a daemon ever spawns one and each is used
/// for a single utterance. Waiting to find the trigger before making the
/// symptom impossible would leave beta users holding a live microphone
/// session belonging to a process that no longer exists.
///
/// Safe by construction: this runs only after the single-instance lock is
/// held, so no other daemon is alive to own the helpers being killed.
#[cfg(unix)]
pub fn reap_stale_helpers() -> usize {
    // Match on the binary's name rather than a full path: the helper lives
    // beside the executable in a bundle, in the source tree during
    // development, and wherever AQUA_SPEECH_HELPER points, and a stale one
    // from any of those is equally wrong.
    let Ok(out) = std::process::Command::new("pgrep")
        .args(["-f", "aqua-speech-helper"])
        .output()
    else {
        // No pgrep, or it failed. Not being able to tidy up is not a reason
        // to refuse to start.
        return 0;
    };

    let targets = helper_pids_to_reap(&String::from_utf8_lossy(&out.stdout), std::process::id());
    let mut killed = 0;
    for pid in targets {
        // SAFETY: `kill` with a pid we just read from pgrep. A pid that has
        // exited in the interim returns ESRCH, which is ignored.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
            killed += 1;
        }
    }
    killed
}

/// Which pids from `pgrep` output should be signalled.
///
/// Split out from the killing so it can be tested: a test that called the
/// real reaper would kill helpers belonging to a developer's own running
/// daemon, which is both rude and makes the test's result depend on what
/// else is happening on the machine.
#[cfg(unix)]
fn helper_pids_to_reap(pgrep_stdout: &str, own_pid: u32) -> Vec<u32> {
    pgrep_stdout
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        // `pgrep -f` matches on the whole command line, so a shell or editor
        // with the string in its arguments can appear in the list. Signalling
        // ourselves would be spectacular.
        .filter(|&pid| pid != own_pid)
        .collect()
}

/// No `pgrep`/`kill` contract to rely on off unix; the Windows helper story
/// does not exist yet either, since the recognizer there is unimplemented.
#[cfg(not(unix))]
pub fn reap_stale_helpers() -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own lock path: they run in parallel, and a shared
    /// path would make them contend with each other rather than with the
    /// thing under test.
    fn temp_lock(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("outloud-test-{}-{}.lock", name, std::process::id()))
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

    /// A HELD lock must refuse a second acquisition of the same path.
    ///
    /// The other direction from `releasing_the_lock_lets_the_next_one_in`,
    /// and the half that actually prevents two daemons. Worth asserting in
    /// the same process because the Windows guard is a named mutex whose
    /// name was once fixed rather than derived from the path: every path
    /// shared one mutex, so the per-path tests fought each other and none of
    /// them had ever passed on Windows.
    ///
    /// Holds on unix too: `flock` treats each `open` as an independent file
    /// description, and a lock taken through one denies another, even within
    /// the same process.
    #[test]
    fn a_held_lock_refuses_a_second_acquisition() {
        let path = temp_lock("exclusion");
        let _ = std::fs::remove_file(&path);

        let _held = acquire_at(path.clone()).expect("first acquisition");
        match acquire_at(path.clone()) {
            Err(Error::AlreadyRunning { .. }) => {}
            Ok(_) => panic!(
                "a second acquisition succeeded while the first was held: \
                 two daemons would bind the hotkey and both type what was said"
            ),
            Err(e) => panic!("expected AlreadyRunning, got {e:?}"),
        }
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

    /// The reaper must never signal the process calling it. `pgrep -f`
    /// matches on the whole command line, so a daemon launched from a shell
    /// whose arguments mention the helper can match itself, and a daemon
    /// that killed itself on startup would be a spectacular regression.
    ///
    /// Tested on the parsing rather than by calling the real reaper, which
    /// would signal helpers belonging to a developer's own running daemon.
    #[test]
    #[cfg(unix)]
    fn the_reaper_never_targets_its_own_process() {
        let me = std::process::id();
        // Derived from our own pid rather than hardcoded: with literal 111
        // and 222 this test failed whenever the test runner happened to BE
        // pid 111 or 222, which is reachable on macOS where pids recycle
        // low. A reaper test that fails at random would train us to ignore
        // it, and this reaper sends SIGKILL.
        let (other_a, other_b) = (me.wrapping_add(1), me.wrapping_add(2));
        let pgrep_output = format!("{other_a}\n{me}\n{other_b}\n");
        let targets = helper_pids_to_reap(&pgrep_output, me);
        assert_eq!(
            targets,
            vec![other_a, other_b],
            "our own pid must be filtered out"
        );
        assert!(!targets.contains(&me));
    }

    #[test]
    #[cfg(unix)]
    fn reaping_tolerates_empty_and_ragged_pgrep_output() {
        // pgrep prints nothing and exits 1 when there is no match, which is
        // the common case on a healthy machine and must not be an error.
        assert!(helper_pids_to_reap("", 1).is_empty());
        // Blank lines and stray whitespace must not panic a parse on the
        // startup path.
        assert_eq!(helper_pids_to_reap("\n  42  \n\n", 1), vec![42]);
        // Anything non-numeric is ignored rather than guessed at.
        assert!(helper_pids_to_reap("not-a-pid\n", 1).is_empty());
    }
}
