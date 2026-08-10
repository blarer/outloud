//! Linux backend: a unix-domain socket that accepts PRESS/RELEASE/PING
//! lines, fed by whatever the compositor was told to `exec`.
//!
//! ## Why this design and not the others in the module doc
//!
//! Wayland has no global hotkey protocol at all (see the crate's top-level
//! doc, which used to live here before this file had real code — moved to
//! `docs/hotkeys.md` section 7 so it survives the stub being replaced). Of
//! the three viable paths:
//!
//! - **Compositor config exec** (this file): the user's compositor already
//!   owns every keybinding on the system and is trusted with arbitrary
//!   `exec` commands. On a machine the user controls entirely (a NixOS
//!   Hyprland config declared in their own flake, the deployment target
//!   this was built for), wiring one `bind`/`bindr` pair to run our CLI is
//!   a single config line, needs no portal negotiation, no DBus session,
//!   and works identically on every wlroots compositor that can `exec` a
//!   program on a key edge (sway, Hyprland, river). It is available TODAY
//!   with zero new system dependencies.
//! - **GlobalShortcuts XDG portal**: the "proper" native path, but only on
//!   KDE and GNOME >= 45; not implemented by Hyprland or most wlroots
//!   compositors as of this writing, so it would leave the actual target
//!   platform stubbed while a technically-nicer path sits unused. Sketched
//!   as a stub below rather than left undocumented, because a future KDE/
//!   GNOME user should not have to reverse-engineer this module to know
//!   what to build.
//! - **evdev/libinput** (`input` group): works everywhere but is
//!   system-wide keylogger capability that must be an explicit, informed
//!   opt-in, never a default. Not attempted here; if it is ever built, it
//!   must live behind its own opt-in gate, matching the crate's stance on
//!   privilege the UX doc takes for permission prompts generally.
//!
//! ## The socket protocol
//!
//! One line per connection, ASCII, case-sensitive, terminated by `\n`:
//!
//! ```text
//! PRESS      the bound key went down (compositor's `bind`)
//! RELEASE    the bound key came up (compositor's `bindr`)
//! PING       liveness probe; touches nothing (see crate::trigger)
//! ```
//!
//! Response, exactly one line: `OK` or `ERR <reason>`. The connection is
//! then closed. This mirrors `shell-bridge`'s protocol (`docs/shell-
//! integration.md`) deliberately: same one-shot-connection shape, same
//! reasoning (a compositor `exec` is exactly as constrained as a shell
//! script — it can run a program with arguments, nothing fancier), same
//! peer-credential gate. It is a DIFFERENT socket and a DIFFERENT crate
//! from `shell-bridge` because the two features are unrelated (this one
//! feeds the hotkey state machine; that one feeds command-line rewrites)
//! and a user must be able to run one without the other.
//!
//! ## Threat model
//!
//! Whoever can speak PRESS/RELEASE on this socket can start and stop
//! microphone capture, exactly as if they pressed the physical key. That
//! is a real capability but a narrow one — unlike shell-bridge's socket it
//! can never inject or execute arbitrary text or commands, only toggle
//! capture — and it gets the same defense in depth: 0700 parent directory,
//! 0600 socket, and `SO_PEERCRED` restricted to our own uid (root
//! rejected, matching `shell-bridge::peer`'s reasoning: root has its own
//! ways to act as us and a root connection here is confused tooling at
//! best).
//!
//! ## Failure modes this backend owns
//!
//! - **Daemon not running.** The compositor's `exec` runs our CLI
//!   (`outloud trigger press`), which tries to connect and finds nothing
//!   listening. It must fail LOUDLY to wherever Hyprland logs a failed
//!   exec, not swallow the error, or the hotkey looks like it does nothing
//!   with no trace of why. See `send_trigger`'s connect-failure message.
//! - **Duplicate presses / a doubled exec.** Handled for free: see
//!   `crate::trigger`'s module doc for why `TapHold` itself absorbs a
//!   repeated PRESS or a stray RELEASE.
//! - **A lost RELEASE (stuck-pressed).** The most dangerous failure this
//!   crate has: a PRESS with no matching RELEASE (the release keybind's
//!   `exec` failed independently, the compositor reloaded mid-hold and
//!   dropped one binding, the CLI process itself was OOM-killed) leaves
//!   the tap-hold machine in `Pressed` forever with no OS-level signal
//!   telling us so, unlike the macOS tap or the Windows hook. A watchdog
//!   thread polls for exactly this and resets the machine; see
//!   `crate::trigger::DEFAULT_WATCHDOG_TIMEOUT` for the full reasoning and
//!   `watchdog_loop` below for the mechanism.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::matcher::Matcher;
use crate::taphold::TapHold;
use crate::trigger::{self, TriggerVerb, DEFAULT_WATCHDOG_TIMEOUT};
use crate::{HotkeyError, HotkeyEvent};

/// A client (our own socket handling, or a hand-run `nc -U`) gets this long
/// to send its one line. Generous for a compositor's `exec`, stingy for a
/// wedged one; matches `shell-bridge`'s `READ_TIMEOUT` reasoning exactly.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the watchdog thread checks for a stuck PRESS. Cheap (one
/// mutex lock, one duration comparison) and unrelated to the timeout
/// itself: a short poll interval just means the 120s deadline (or whatever
/// override is set) is caught close to on time rather than being the
/// deadline itself.
const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Default socket path: `$XDG_RUNTIME_DIR/outloud/hotkey-trigger.sock`,
/// else a uid-scoped path under `/tmp`.
///
/// `$XDG_RUNTIME_DIR` is authoritative on every real Linux desktop
/// (systemd-logind sets it to a `0700` per-user tmpfs with session
/// lifetime) so it is trusted first with no existence probe, matching
/// `shell-bridge::server::default_socket_path`. The `/tmp` fallback embeds
/// the uid for the same collision reason shell-bridge's does: a predictable
/// shared path is a symlink-attack invitation between two users on one
/// machine.
///
/// A DIFFERENT socket from shell-bridge's (`outloud/shell.sock` vs
/// `outloud/hotkey-trigger.sock` in the same directory): unrelated
/// features, unrelated failure domains, and a user must be able to run one
/// without the other existing.
///
/// `$OUTLOUD_HOTKEY_TRIGGER_SOCKET` overrides everything, for tests and for
/// a user running two configurations on one machine.
pub fn default_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("OUTLOUD_HOTKEY_TRIGGER_SOCKET") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: geteuid cannot fail.
            PathBuf::from(format!("/tmp/outloud-{}", unsafe { libc::geteuid() }))
        });
    base.join("outloud").join("hotkey-trigger.sock")
}

/// The watchdog timeout, overridable for testing the recovery path against
/// real hardware without waiting two minutes. Read once at bind time, not
/// per-tick, so a test can set the env var before binding and see it take
/// effect deterministically.
fn watchdog_timeout() -> Duration {
    std::env::var("OUTLOUD_HOTKEY_TRIGGER_WATCHDOG_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_WATCHDOG_TIMEOUT)
}

/// Everything the accept loop and the watchdog share. One mutex, two
/// writers: the accept loop on every connection, the watchdog once per
/// poll tick. Neither holds it for long (no IO happens under the lock),
/// so contention is not a concern for a hotkey's edge rate.
struct TriggerState {
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
    /// Set when a PRESS arrives with no RELEASE since; cleared on RELEASE.
    /// Deliberately tracks the TRIGGER protocol's own liveness (did both
    /// halves of one gesture arrive) rather than `TapHold`'s internal
    /// state, so a legitimate tap-then-latch (RELEASE arrived, on purpose
    /// leaves capture running) never trips the watchdog — that ongoing
    /// latch is the pipeline's `hot_mic_timeout_ms` safety net's job
    /// (`crates/outloud/src/pipeline.rs`), not this one's.
    press_outstanding_since: Option<Instant>,
}

impl TriggerState {
    fn apply(&mut self, verb: TriggerVerb, now: Instant) {
        match verb {
            TriggerVerb::Press => self.press_outstanding_since = Some(now),
            TriggerVerb::Release => self.press_outstanding_since = None,
            TriggerVerb::Ping => {}
        }
        for ev in trigger::apply(&mut self.machine, verb, now) {
            let _ = self.sender.send(ev);
        }
    }
}

pub fn spawn(
    matcher: Matcher,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    // The pre-compiled macOS matcher speaks CGEvent vocabulary and has
    // nothing to match here: the compositor already decided which
    // physical key mattered, and the trigger protocol carries only PRESS/
    // RELEASE/PING, never a keycode.
    let _ = matcher;
    spawn_at(&default_socket_path(), machine, sender)
}

/// `spawn`, against an explicit socket path, so a test (or a user running
/// two configurations) does not collide with a real daemon's socket.
pub fn spawn_at(
    path: &Path,
    machine: TapHold,
    sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    let listener = bind(path).map_err(|e| HotkeyError::Backend(e.to_string()))?;

    let state = Arc::new(Mutex::new(TriggerState {
        machine,
        sender,
        press_outstanding_since: None,
    }));

    let watchdog_state = Arc::clone(&state);
    let timeout = watchdog_timeout();
    std::thread::Builder::new()
        .name("hotkey-trigger-watchdog".into())
        .spawn(move || watchdog_loop(watchdog_state, timeout))
        .map_err(|e| HotkeyError::Backend(format!("failed to spawn watchdog thread: {e}")))?;

    // Confirmation the accept loop is actually ready to accept, so `spawn`
    // keeps its documented "Ok means live" contract (crate::backend doc):
    // the listener above is already bound by the time we get here (bind()
    // returns only once the socket exists and is chmod'd), so there is
    // nothing further to wait for. The thread below just needs to exist.
    std::thread::Builder::new()
        .name("hotkey-trigger-accept".into())
        .spawn(move || accept_loop(listener, state))
        .map_err(|e| HotkeyError::Backend(format!("failed to spawn accept thread: {e}")))?;

    Ok(())
}

/// Bind the socket with the same permission ordering `shell-bridge::server`
/// uses and for the same reason: a socket that is briefly world-writable is
/// briefly a hole, so the parent directory is `0700` and the socket `0600`
/// BEFORE the first `accept`, never after.
fn bind(path: &Path) -> std::io::Result<UnixListener> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::other("hotkey trigger socket path has no parent directory")
    })?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;

    // A leftover socket from a dead daemon must not block us; a LIVE daemon
    // must win over a second one starting by accident (two daemons racing
    // to bind the trigger socket has the same double-record failure mode
    // `crates/outloud/src/instance.rs` documents for the whole process).
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::other(format!(
                "another hotkey-trigger listener is already live at {}",
                path.display()
            )));
        }
        std::fs::remove_file(path)?;
    }

    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn accept_loop(listener: UnixListener, state: Arc<Mutex<TriggerState>>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            // A transient accept failure (fd exhaustion, EINTR) must not
            // take the whole hotkey down; log and keep serving, same
            // posture as shell-bridge's per-connection error handling.
            eprintln!("hotkey: trigger socket accept failed; continuing");
            continue;
        };
        if let Err(e) = handle_connection(stream, &state) {
            eprintln!("hotkey: trigger connection error: {e}");
        }
    }
}

fn handle_connection(
    mut stream: UnixStream,
    state: &Arc<Mutex<TriggerState>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;

    // The credential gate comes before reading a single byte, exactly like
    // shell-bridge: no parsing of untrusted input from a peer about to be
    // rejected anyway.
    if !peer_is_self(&stream)? {
        let _ = stream.write_all(b"ERR peer uid mismatch\n");
        return Err(std::io::Error::other(
            "rejected connection from foreign uid",
        ));
    }

    let mut line = String::new();
    // 64 bytes is generous for "RELEASE\n" plus margin; capped so a
    // confused or hostile client cannot make us buffer arbitrarily.
    let mut reader = BufReader::new((&stream).take(64));
    reader.read_line(&mut line)?;

    let response = match TriggerVerb::parse(&line) {
        Err(e) => format!("ERR {e}\n"),
        Ok(verb) => {
            let mut guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.apply(verb, Instant::now());
            "OK\n".to_string()
        }
    };
    stream.write_all(response.as_bytes())?;
    Ok(())
}

/// Effective uid of the connected peer. `SO_PEERCRED` on Linux; refuses on
/// any other unix this module happens to compile for (BSDs are not a build
/// target today, but a silent accept-everyone fallback would be a much
/// worse failure than refusing).
fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();

    #[cfg(target_os = "linux")]
    {
        // SAFETY: ucred is plain-old-data; the kernel writes at most `len`
        // bytes and we pass its exact size.
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        Err(std::io::Error::other(
            "peer credential check unsupported on this platform",
        ))
    }
}

/// Accept only our own uid; root deliberately rejected, matching
/// `shell-bridge::peer::peer_is_self`'s reasoning verbatim: root does not
/// need this socket to act as us, so a root connection here is at best
/// confused tooling.
fn peer_is_self(stream: &UnixStream) -> std::io::Result<bool> {
    // SAFETY: geteuid cannot fail.
    let me = unsafe { libc::geteuid() };
    Ok(peer_uid(stream)? == me)
}

/// Poll for a PRESS that never got its RELEASE and force recovery.
///
/// This is the Linux backend's answer to the same question the macOS tap
/// callback and the Windows hook's `pump_with_watchdog` both answer: a
/// key-up went missing, so reset rather than leave the microphone hot
/// forever. See `crate::trigger::DEFAULT_WATCHDOG_TIMEOUT` for why the
/// interval is long (no independent "something broke" signal exists here,
/// unlike a disabled event tap) and `TriggerState::press_outstanding_since`
/// for why a genuine latch never trips it.
fn watchdog_loop(state: Arc<Mutex<TriggerState>>, timeout: Duration) {
    loop {
        std::thread::sleep(WATCHDOG_POLL_INTERVAL);
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(since) = guard.press_outstanding_since else {
            continue;
        };
        if !trigger::watchdog_expired(since, Instant::now(), timeout) {
            continue;
        }
        eprintln!(
            "hotkey: no RELEASE trigger arrived within {}s of a PRESS \
             (lost compositor exec, or the daemon missed it); forcing the \
             microphone closed and resetting",
            timeout.as_secs()
        );
        for ev in guard.machine.reset() {
            let _ = guard.sender.send(ev);
        }
        let _ = guard.sender.send(HotkeyEvent::TapRecovered);
        guard.press_outstanding_since = None;
    }
}

// --- The client side: what `outloud trigger <verb>` and the doctor use ----

/// Connect to `path`, send one trigger line, and read the one-line
/// response. Used both by the `outloud trigger` CLI (what the compositor's
/// `exec` actually runs) and by the doctor's liveness probe.
///
/// The connect-failure message is deliberately explicit about "daemon not
/// running": a compositor `exec` that fails silently reads as a hotkey that
/// does nothing, with zero trace of why, which is exactly the "silently
/// dead hotkey" failure class this crate exists to prevent (see the crate
/// doc). Whatever a user's terminal or Hyprland's exec log shows for a
/// nonzero exit needs to be an actionable sentence, not `ENOENT`.
pub fn send_trigger(path: &Path, verb: TriggerVerb) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "cannot reach the outloud hotkey-trigger socket at {} ({e}); is `outloud` \
                 running? (a stopped daemon means every compositor keypress silently does \
                 nothing)",
                path.display()
            ),
        )
    })?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    stream.write_all(verb.as_line().as_bytes())?;
    stream.write_all(b"\n")?;

    let mut line = String::new();
    BufReader::new(stream.take(256)).read_line(&mut line)?;
    let line = line.trim_end();
    if let Some(reason) = line.strip_prefix("ERR ") {
        return Err(std::io::Error::other(format!(
            "hotkey-trigger daemon rejected {}: {reason}",
            verb.as_line()
        )));
    }
    if line != "OK" {
        return Err(std::io::Error::other(format!(
            "hotkey-trigger daemon sent an unexpected response: {line:?}"
        )));
    }
    Ok(())
}

/// Is a trigger daemon listening at all? A `PING` that succeeds, nothing
/// more. Used by `outloud --permissions` / the doctor to report the honest
/// Linux hotkey situation instead of silence.
pub fn daemon_reachable(path: &Path) -> bool {
    send_trigger(path, TriggerVerb::Ping).is_ok()
}

// ---------------------------------------------------------------------------
// XDG GlobalShortcuts portal: sketch, not wired up.
// ---------------------------------------------------------------------------

/// Whether a `org.freedesktop.portal.GlobalShortcuts` implementation looks
/// reachable on this session, for a future native-binding path on KDE/
/// GNOME >= 45.
///
/// Deliberately NOT implemented beyond this detection stub: doing the
/// portal properly needs a DBus session connection, a `CreateSession` +
/// `BindShortcuts` request/response round trip through the portal's request
/// object pattern, persisting the returned session token across restarts
/// (the portal only shows its one-time consent dialog once per token), and
/// listening for `Activated`/`Deactivated` signals — each a genuine chunk
/// of work and, per `docs/hotkeys.md` section 7, not implemented by
/// Hyprland (the actual deployment target) at all. Building it now would
/// spend the budget this task explicitly protects ("do not let this block
/// the working Hyprland path") on a path this machine cannot even exercise.
/// Detection is cheap and worth having so a future contributor building the
/// real integration does not have to first figure out how to tell whether
/// one exists.
///
/// The portal announces itself at the well-known DBus name
/// `org.freedesktop.portal.Desktop` implementing the
/// `org.freedesktop.portal.GlobalShortcuts` interface; the cheapest honest
/// signal without a DBus client library is whether `dbus-send` (present on
/// every desktop that ships DBus, which every portal-capable session
/// requires) can successfully introspect that interface.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn globalshortcuts_portal_available() -> bool {
    std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.DBus.Introspectable.Introspect",
        ])
        .output()
        .map(|o| {
            o.status.success() && String::from_utf8_lossy(&o.stdout).contains("GlobalShortcuts")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "outloud-hotkey-test-{}-{name}-{}.sock",
            std::process::id(),
            name
        ))
    }

    /// End-to-end over a real socket: PRESS then RELEASE past the tap
    /// threshold must produce Pressed then Released on the manager's
    /// channel, exactly like a held physical key on the other backends.
    #[test]
    fn press_then_release_over_the_real_socket_drives_the_machine() {
        let path = temp_socket("e2e-hold");
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let machine = TapHold::new(crate::taphold::Timing::default());
        spawn_at(&path, machine, tx).expect("spawn_at");

        send_trigger(&path, TriggerVerb::Press).expect("press");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            HotkeyEvent::Pressed
        );

        std::thread::sleep(Duration::from_millis(350)); // past the 300ms tap threshold
        send_trigger(&path, TriggerVerb::Release).expect("release");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            HotkeyEvent::Released
        );

        let _ = std::fs::remove_file(&path);
    }

    /// PING must never appear on the event channel: it is a pure liveness
    /// probe, not a key edge. A doctor or a curious human running it by
    /// hand must never accidentally start a dictation.
    #[test]
    fn ping_reaches_the_daemon_but_emits_no_event() {
        let path = temp_socket("ping");
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let machine = TapHold::new(crate::taphold::Timing::default());
        spawn_at(&path, machine, tx).expect("spawn_at");

        assert!(daemon_reachable(&path));
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "PING must not reach the event channel"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The daemon-not-running message is the whole point of `send_trigger`;
    /// a silent connect failure is indistinguishable from a dead hotkey.
    #[test]
    fn connecting_to_nothing_names_the_daemon_as_the_problem() {
        let path = temp_socket("nobody-home");
        let _ = std::fs::remove_file(&path);
        let err = send_trigger(&path, TriggerVerb::Press).unwrap_err();
        assert!(
            err.to_string().contains("outloud"),
            "must name the daemon: {err}"
        );
    }

    /// The watchdog's job: a PRESS with no RELEASE recovers on its own
    /// instead of holding the microphone open until the process restarts.
    /// Uses the env override so the test does not wait two real minutes.
    #[test]
    fn a_lost_release_is_recovered_by_the_watchdog() {
        let path = temp_socket("watchdog");
        let _ = std::fs::remove_file(&path);
        // SAFETY: no other thread reads/writes this var during the test's
        // own setup window; the harness may run tests in parallel, but the
        // var only affects backends spawned after it is read at bind time.
        unsafe { std::env::set_var("OUTLOUD_HOTKEY_TRIGGER_WATCHDOG_MS", "150") };
        let (tx, rx) = std::sync::mpsc::channel();
        let machine = TapHold::new(crate::taphold::Timing::default());
        spawn_at(&path, machine, tx).expect("spawn_at");
        unsafe { std::env::remove_var("OUTLOUD_HOTKEY_TRIGGER_WATCHDOG_MS") };

        send_trigger(&path, TriggerVerb::Press).expect("press");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            HotkeyEvent::Pressed
        );

        // No RELEASE ever sent. Within (150ms timeout + 5s poll interval)
        // the watchdog must force it closed.
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(7);
        while Instant::now() < deadline && got.len() < 2 {
            if let Ok(ev) = rx.recv_timeout(Duration::from_secs(7)) {
                got.push(ev);
            }
        }
        assert!(
            got.contains(&HotkeyEvent::Released),
            "watchdog must release a stuck press: {got:?}"
        );
        assert!(
            got.contains(&HotkeyEvent::TapRecovered),
            "watchdog recovery must be visible to the UI: {got:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A doubled PRESS exec (retried, or a fumbled compositor config
    /// binding the same key twice) must not double-start capture.
    #[test]
    fn duplicate_press_over_the_socket_starts_capture_once() {
        let path = temp_socket("dup-press");
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let machine = TapHold::new(crate::taphold::Timing::default());
        spawn_at(&path, machine, tx).expect("spawn_at");

        send_trigger(&path, TriggerVerb::Press).expect("press 1");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            HotkeyEvent::Pressed
        );
        send_trigger(&path, TriggerVerb::Press).expect("press 2");
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a second PRESS must not re-fire Pressed"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn socket_permissions_are_owner_only() {
        let path = temp_socket("perms");
        let _ = std::fs::remove_file(&path);
        let (tx, _rx) = std::sync::mpsc::channel();
        let machine = TapHold::new(crate::taphold::Timing::default());
        spawn_at(&path, machine, tx).expect("spawn_at");
        // Give the accept thread a moment to finish binding before we stat.
        std::thread::sleep(Duration::from_millis(50));

        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let dir_meta = std::fs::metadata(path.parent().unwrap()).unwrap();
        assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_malformed_line_gets_a_named_error_not_a_hang() {
        let path = temp_socket("malformed");
        let _ = std::fs::remove_file(&path);
        let (tx, _rx) = std::sync::mpsc::channel();
        let machine = TapHold::new(crate::taphold::Timing::default());
        spawn_at(&path, machine, tx).expect("spawn_at");

        let mut stream = UnixStream::connect(&path).expect("connect");
        stream.write_all(b"BOGUS\n").unwrap();
        let mut resp = String::new();
        BufReader::new(stream).read_line(&mut resp).unwrap();
        assert!(resp.starts_with("ERR"), "{resp}");
        assert!(resp.contains("BOGUS"), "{resp}");

        let _ = std::fs::remove_file(&path);
    }
}
