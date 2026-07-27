//! Hot reload: apply config changes without restarting the daemon.
//!
//! A dictation daemon that must be restarted to change a hotkey is hostile
//! (docs/ux/05: "every setting change applies live"). The design splits into
//! a pure debouncer state machine, tested with fake time, and a small
//! polling loop that feeds it.
//!
//! Polling, not FSEvents/inotify, on purpose for this layer: editors save
//! with a storm of writes (write temp, rename, chmod), and a 250ms poll of
//! (mtime, size, content hash) coalesces the storm for free, costs nothing
//! measurable at this frequency, and behaves identically on every platform
//! and every filesystem including network mounts where the native watchers
//! are unreliable. A native-watcher backend can feed the same debouncer
//! later without changing any caller.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// The debouncer: raw "file looks different" observations in, settled
/// "reload now" decisions out. Time is an argument, so tests do not sleep.
#[derive(Debug)]
pub struct Debouncer {
    /// How long the file must hold still before we reload. Editors that
    /// write in bursts settle well inside this; a human cannot save twice
    /// faster than it.
    quiet: Duration,
    /// When we last saw a change, if a reload is pending.
    pending_since: Option<Instant>,
}

impl Debouncer {
    pub fn new(quiet: Duration) -> Debouncer {
        Debouncer {
            quiet,
            pending_since: None,
        }
    }

    /// Record that the file changed at `now`.
    pub fn observe_change(&mut self, now: Instant) {
        // Restart the quiet window on every change: a save storm keeps
        // pushing the reload out until the file actually settles, which is
        // the entire point of debouncing.
        self.pending_since = Some(now);
    }

    /// Should we reload at `now`? Consumes the pending state on yes.
    pub fn should_reload(&mut self, now: Instant) -> bool {
        match self.pending_since {
            Some(since) if now.duration_since(since) >= self.quiet => {
                self.pending_since = None;
                true
            }
            _ => false,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending_since.is_some()
    }
}

/// Cheap change fingerprint. Content hash included because mtime granularity
/// is a whole second on some filesystems, and a save-then-save-again inside
/// one second must still be seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    mtime: Option<std::time::SystemTime>,
    len: u64,
    hash: u64,
}

/// Fingerprint a file. A missing file is a valid fingerprint (deleting the
/// config is a change: it means "back to defaults").
pub fn fingerprint(path: &Path) -> Fingerprint {
    let meta = std::fs::metadata(path).ok();
    let content = std::fs::read(path).unwrap_or_default();
    Fingerprint {
        mtime: meta.as_ref().and_then(|m| m.modified().ok()),
        len: content.len() as u64,
        hash: fnv1a(&content),
    }
}

/// FNV-1a: tiny, dependency-free, and plenty for change detection (we are
/// not defending against adversarial collisions in the user's own file).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// A reload notification: which file settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reload {
    pub path: PathBuf,
}

/// Watch files on a background thread, sending a [`Reload`] after each
/// settled change. Dropping the watcher stops the thread.
pub struct Watcher {
    stop: Sender<()>,
    events: Receiver<Reload>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    /// Default quiet window. Long enough to swallow editor save storms,
    /// short enough that a settings change feels immediate (well under the
    /// ~1s a user takes to switch back to the app they were dictating into).
    pub const DEFAULT_QUIET: Duration = Duration::from_millis(300);
    const POLL: Duration = Duration::from_millis(250);

    pub fn spawn(paths: Vec<PathBuf>, quiet: Duration) -> Watcher {
        let (stop_tx, stop_rx) = channel::<()>();
        let (event_tx, event_rx) = channel::<Reload>();
        let handle = std::thread::Builder::new()
            .name("config-watch".into())
            .spawn(move || {
                let mut state: Vec<(PathBuf, Fingerprint, Debouncer)> = paths
                    .into_iter()
                    .map(|p| {
                        let fp = fingerprint(&p);
                        (p, fp, Debouncer::new(quiet))
                    })
                    .collect();
                loop {
                    // recv_timeout doubles as the poll interval and the stop
                    // signal, so shutdown is immediate rather than waiting
                    // out a sleep.
                    match stop_rx.recv_timeout(Self::POLL) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let now = Instant::now();
                    for (path, last_fp, debouncer) in &mut state {
                        let fp = fingerprint(path);
                        if fp != *last_fp {
                            *last_fp = fp;
                            debouncer.observe_change(now);
                        }
                        if debouncer.should_reload(now)
                            && event_tx.send(Reload { path: path.clone() }).is_err()
                        {
                            return; // receiver gone: nobody to reload for
                        }
                    }
                }
            })
            .expect("spawn config-watch thread");
        Watcher {
            stop: stop_tx,
            events: event_rx,
            handle: Some(handle),
        }
    }

    /// The channel of settled reloads.
    pub fn events(&self) -> &Receiver<Reload> {
        &self.events
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn no_change_no_reload() {
        let mut d = Debouncer::new(Duration::from_millis(300));
        assert!(!d.should_reload(t0()));
        assert!(!d.is_pending());
    }

    #[test]
    fn single_change_reloads_after_quiet_window() {
        let start = t0();
        let mut d = Debouncer::new(Duration::from_millis(300));
        d.observe_change(start);
        // Too early: still inside the quiet window.
        assert!(!d.should_reload(start + Duration::from_millis(100)));
        // Settled: reload fires exactly once.
        assert!(d.should_reload(start + Duration::from_millis(300)));
        assert!(!d.should_reload(start + Duration::from_millis(400)));
    }

    #[test]
    fn save_storm_coalesces_to_one_reload() {
        let start = t0();
        let mut d = Debouncer::new(Duration::from_millis(300));
        // An editor writing temp + rename + chmod within 50ms.
        for ms in [0u64, 10, 20, 50] {
            d.observe_change(start + Duration::from_millis(ms));
            assert!(!d.should_reload(start + Duration::from_millis(ms)));
        }
        // The window restarts from the LAST write, not the first.
        assert!(!d.should_reload(start + Duration::from_millis(340)));
        assert!(d.should_reload(start + Duration::from_millis(350)));
    }

    #[test]
    fn changes_after_reload_arm_again() {
        let start = t0();
        let mut d = Debouncer::new(Duration::from_millis(300));
        d.observe_change(start);
        assert!(d.should_reload(start + Duration::from_millis(300)));
        // A later save arms a second, independent reload.
        d.observe_change(start + Duration::from_millis(1000));
        assert!(!d.should_reload(start + Duration::from_millis(1100)));
        assert!(d.should_reload(start + Duration::from_millis(1300)));
    }

    #[test]
    fn fingerprint_sees_content_change_with_same_length() {
        let dir = std::env::temp_dir().join(format!("aqua-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "hotkey = \"f13\"").unwrap();
        let a = fingerprint(&path);
        // Same byte length, different content: mtime may not tick, hash must.
        std::fs::write(&path, "hotkey = \"f14\"").unwrap();
        let b = fingerprint(&path);
        assert_ne!(a, b);
        std::fs::remove_file(&path).unwrap();
        // Deletion is also a change (back to defaults).
        let c = fingerprint(&path);
        assert_ne!(b, c);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn watcher_delivers_a_settled_reload_end_to_end() {
        let dir = std::env::temp_dir().join(format!("aqua-watch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "a = 1").unwrap();

        // Short windows so the test runs in well under a second.
        let watcher = Watcher::spawn(vec![path.clone()], Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&path, "a = 2").unwrap();

        let reload = watcher
            .events()
            .recv_timeout(Duration::from_secs(5))
            .expect("reload event");
        assert_eq!(reload.path, path);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
