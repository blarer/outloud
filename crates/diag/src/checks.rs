//! The concrete checks. Each one exists because a real, hours-costing
//! environmental failure was hit during M0 (see docs/M0-results.md and
//! docs/macos-permissions.md). The remedy strings are the product: they must
//! name the exact next action, because "permission denied" was precisely the
//! kind of message that cost those hours.
//!
//! Testing strategy: anything that can be decided from strings (env vars,
//! `codesign` output, executable paths) is factored into a pure function and
//! unit-tested. Probes that require the live OS (AX calls, `system_profiler`)
//! are exercised only by running `doctor` on a real machine.

use std::path::Path;
use std::process::Command;

use crate::{Check, CheckOutcome, Env, ErrorClass};

// ---------------------------------------------------------------------------
// Accessibility permission
// ---------------------------------------------------------------------------

/// Accessibility trust, including the responsible-process trap.
///
/// The trap: TCC attributes the grant to the *responsible process*. A binary
/// run from a shell is judged against the terminal, so the app's own toggle
/// can read "on" while every call fails. We therefore report not just the
/// trust bit but *who* is being judged, by looking at how we were launched.
pub struct AccessibilityPermission;

impl Check for AccessibilityPermission {
    fn name(&self) -> &'static str {
        "accessibility-permission"
    }

    fn run(&self, env: &Env) -> CheckOutcome {
        // Windows needs no accessibility grant at all: any process may be a
        // UI Automation client. The elevation boundary is the real limit, so
        // that is what the check reports instead of a permission that does
        // not exist. Reporting "unimplemented" here (as this did while the
        // backends were stubs) would send a Windows user hunting for a
        // setting they cannot find.
        if cfg!(target_os = "windows") {
            return CheckOutcome::warn(
                "Windows: UI Automation needs no permission grant, but a non-elevated \
                 process cannot read or write ELEVATED windows (UIPI)",
                ErrorClass::Environment,
                "if dictation goes silent, check whether the focused window is running as \
                 administrator; move focus to a normal window (docs/compat-matrix.md)",
            );
        }
        if !cfg!(target_os = "macos") {
            return CheckOutcome::warn(
                "not macOS: accessibility backend unimplemented here",
                ErrorClass::Environment,
                "run on macOS, or wait for the Linux (AT-SPI) backend",
            );
        }
        let launched_from_shell = launched_from_shell(env);
        if ax_edit::is_trusted(false) {
            if launched_from_shell {
                // Trusted, but the grant being exercised is the TERMINAL's,
                // not the app's. Works today, misleads tomorrow.
                return CheckOutcome::warn(
                    "trusted, but launched from a shell: the grant in effect is the terminal's",
                    ErrorClass::Configuration,
                    "launch via LaunchServices (`open -a dist/OutLoud.app` or scripts/doctor.sh) \
                     so the app is judged against its own grant",
                );
            }
            return CheckOutcome::pass("process is trusted for accessibility");
        }
        if launched_from_shell {
            return CheckOutcome::fail(
                "not trusted, and launched from a shell: macOS is checking the TERMINAL'S \
                 permission, not this app's (responsible-process rule)",
                ErrorClass::Permission,
                "either grant your terminal Accessibility in System Settings > Privacy & \
                 Security > Accessibility, or (better) relaunch through LaunchServices: \
                 `open -a dist/OutLoud.app` / scripts/doctor.sh",
            );
        }
        CheckOutcome::fail(
            "not trusted for accessibility",
            ErrorClass::Permission,
            format!(
                "System Settings > Privacy & Security > Accessibility: enable the toggle for \
                 this app. If the toggle already reads 'on', the signature changed since the \
                 grant; run `tccutil reset Accessibility {}` and re-grant",
                crate::BUNDLE_ID
            ),
        )
    }
}

/// Heuristic for "was this process started by a shell", the situation in which
/// TCC judges the terminal rather than the binary.
///
/// Complication: `open --env` (used by scripts/doctor.sh) passes the caller's
/// environment through, so TERM survives a LaunchServices launch. The wrapper
/// therefore sets AQUA_LAUNCHED_VIA_LS=1 explicitly, which overrides the
/// shell markers: LaunchServices decides the responsible process regardless
/// of what env vars leaked through.
pub fn launched_from_shell(env: &Env) -> bool {
    if env.get("AQUA_LAUNCHED_VIA_LS").is_some() {
        return false;
    }
    // TERM_SESSION_ID / ITERM_SESSION_ID mark Terminal.app/iTerm2 sessions;
    // a plain TERM alone also only appears when a tty was inherited.
    env.get("TERM_SESSION_ID").is_some()
        || env.get("ITERM_SESSION_ID").is_some()
        || env.get("TERM").is_some()
}

// ---------------------------------------------------------------------------
// Microphone permission
// ---------------------------------------------------------------------------

/// Microphone access. A dictation tool without a microphone grant records
/// silence, which surfaces as "the model transcribes nothing", far from the
/// actual cause. We read the TCC decision non-invasively where possible.
pub struct MicrophonePermission;

impl Check for MicrophonePermission {
    fn name(&self) -> &'static str {
        "microphone-permission"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::pass("non-macOS: no TCC microphone gate");
        }
        // There is no public "query without prompting" API from a plain
        // binary, so probe the TCC database read-only. Failure to read is
        // itself informative (means we cannot know, not that it is denied).
        match tcc_microphone_state() {
            Some(true) => CheckOutcome::pass("microphone access granted"),
            Some(false) => CheckOutcome::fail(
                "microphone access denied or not yet requested",
                ErrorClass::Permission,
                "System Settings > Privacy & Security > Microphone: enable this app. If it is \
                 not listed, run the app once so it can request access",
            ),
            None => CheckOutcome::warn(
                "cannot determine microphone permission from here",
                ErrorClass::Permission,
                "run a capture test: if it records silence, grant microphone access in System \
                 Settings > Privacy & Security > Microphone",
            ),
        }
    }
}

/// Best-effort read of the per-user TCC decision for microphone. Returns None
/// when the database is unreadable (SIP protects it in most configurations),
/// which callers must treat as "unknown", never as "denied".
fn tcc_microphone_state() -> Option<bool> {
    let home = std::env::var("HOME").ok()?;
    let db = format!("{home}/Library/Application Support/com.apple.TCC/TCC.db");
    let out = Command::new("sqlite3")
        .arg(&db)
        .arg("select client, auth_value from access where service='kTCCServiceMicrophone';")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // auth_value 2 = allowed. Any row for our bundle id decides; absence of
    // any readable row means unknown.
    for line in text.lines() {
        if line.contains(crate::BUNDLE_ID) {
            return Some(line.trim_end().ends_with("|2"));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Code signature
// ---------------------------------------------------------------------------

/// Signature identity, and specifically whether it is ad-hoc.
///
/// TCC pins grants to the cdhash for ad-hoc signatures, so every rebuild
/// silently revokes the grant while the System Settings toggle still reads
/// "on". This is Warn rather than Fail: it works right now, it will break on
/// the next rebuild, which is exactly what Warn means.
pub struct CodeSignature;

impl Check for CodeSignature {
    fn name(&self) -> &'static str {
        "code-signature"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::pass("non-macOS: no code signature gate");
        }
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return CheckOutcome::fail(
                    format!("cannot locate own executable: {e}"),
                    ErrorClass::Bug,
                    "file a GitHub issue with this doctor output",
                )
            }
        };
        // codesign prints its human-readable details on stderr, not stdout.
        let out = match Command::new("codesign").arg("-dv").arg(&exe).output() {
            Ok(o) => o,
            Err(e) => {
                return CheckOutcome::warn(
                    format!("codesign not runnable: {e}"),
                    ErrorClass::Environment,
                    "install Xcode command line tools: `xcode-select --install`",
                )
            }
        };
        let text = String::from_utf8_lossy(&out.stderr);
        classify_codesign_output(out.status.success(), &text)
    }
}

/// Pure classification of `codesign -dv` output so the parsing is testable
/// without a real signed binary.
pub fn classify_codesign_output(success: bool, stderr: &str) -> CheckOutcome {
    if !success || stderr.contains("code object is not signed") {
        return CheckOutcome::fail(
            "binary is unsigned",
            ErrorClass::Configuration,
            "build through scripts/bundle-macos.sh so the bundle is signed; TCC cannot pin a \
             grant to an unsigned binary",
        );
    }
    // "Signature=adhoc" is the marker codesign prints for ad-hoc signatures.
    // A real identity prints "Authority=Developer ID Application: ..." lines.
    if stderr.lines().any(|l| l.trim() == "Signature=adhoc") {
        return CheckOutcome::warn(
            "signature is AD-HOC: the accessibility grant is pinned to this exact build's \
             cdhash and will silently die on the next rebuild (toggle will still read 'on')",
            ErrorClass::Configuration,
            format!(
                "after every rebuild run `tccutil reset Accessibility {}` and re-grant; long \
                 term, sign with a Developer ID certificate so the grant survives rebuilds",
                crate::BUNDLE_ID
            ),
        );
    }
    let identity = stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("Authority="))
        .unwrap_or("unknown identity");
    CheckOutcome::pass(format!("signed by {identity}"))
}

// ---------------------------------------------------------------------------
// Bundle vs bare binary
// ---------------------------------------------------------------------------

/// Whether we are running from inside a .app bundle. A bare binary has no
/// stable identity for TCC to attach a grant to, and gets the terminal as its
/// responsible process; both traps disappear inside a bundle launched by
/// LaunchServices.
pub struct BundleLaunch;

impl Check for BundleLaunch {
    fn name(&self) -> &'static str {
        "bundle-launch"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::pass("non-macOS: bundles not applicable");
        }
        let exe = std::env::current_exe().unwrap_or_default();
        if path_is_in_bundle(&exe) {
            CheckOutcome::pass(format!("running from bundle ({})", exe.display()))
        } else {
            CheckOutcome::warn(
                "running as a bare binary, not from a .app bundle: TCC has no stable identity \
                 to grant against",
                ErrorClass::Configuration,
                "package with scripts/bundle-macos.sh and launch the bundle (`open -a \
                 dist/OutLoud.app` or scripts/doctor.sh)",
            )
        }
    }
}

/// A macOS bundle executable always lives at *.app/Contents/MacOS/<name>.
pub fn path_is_in_bundle(exe: &Path) -> bool {
    let s = exe.to_string_lossy();
    s.contains(".app/Contents/MacOS/")
}

// ---------------------------------------------------------------------------
// Space / window visibility
// ---------------------------------------------------------------------------

/// Whether the frontmost app exposes any windows to the accessibility API.
///
/// Apps whose windows live on another Space report zero windows, which reads
/// as "this app exposes nothing editable" and sent M0 down a false trail with
/// Chrome. Zero windows is ambiguous, so we say so explicitly.
pub struct WindowVisibility;

impl Check for WindowVisibility {
    fn name(&self) -> &'static str {
        "window-visibility"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::pass("non-macOS: skipped");
        }
        if !ax_edit::is_trusted(false) {
            return CheckOutcome::warn(
                "cannot probe windows without accessibility trust",
                ErrorClass::Permission,
                "fix the accessibility-permission check first, then re-run doctor",
            );
        }
        let Some(app) = ax_edit::frontmost_app() else {
            return CheckOutcome::warn(
                "no frontmost application resolvable",
                ErrorClass::Environment,
                "click into any application window on the CURRENT Space and re-run doctor",
            );
        };
        // Depth 1 is enough: we only need the window count, not the fields.
        match ax_edit::find_text_fields(&app, 1) {
            Ok(scan) if scan.windows == 0 => CheckOutcome::warn(
                format!(
                    "frontmost app ({app}) reports ZERO windows: they are likely on \
                         another Space, which the window server hides"
                ),
                ErrorClass::Environment,
                "move one of the app's windows to the current Space (or Mission Control > \
                 drag it here) and re-run doctor",
            ),
            Ok(scan) => CheckOutcome::pass(format!(
                "frontmost app ({app}) exposes {} window(s)",
                scan.windows
            )),
            Err(e) => CheckOutcome::warn(
                format!("window scan failed: {e}"),
                crate::classify_ax_error(&e),
                "re-run doctor with a stable foreground app; if it persists, this may be a bug \
                 worth filing",
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Chromium opt-in
// ---------------------------------------------------------------------------

/// Chromium apps (Chrome, Electron) expose NO accessibility tree until the
/// private `AXManualAccessibility` attribute is set on them, or a system
/// assistive client is detected. Without the opt-in they look completely
/// opaque, which is indistinguishable from "broken" unless named.
pub struct ChromiumOptIn;

impl Check for ChromiumOptIn {
    fn name(&self) -> &'static str {
        "chromium-opt-in"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::pass("non-macOS: skipped");
        }
        let chromium_running = running_chromium_apps();
        if chromium_running.is_empty() {
            return CheckOutcome::pass("no Chromium-family app running; nothing to opt in");
        }
        if !ax_edit::is_trusted(false) {
            return CheckOutcome::warn(
                format!(
                    "Chromium apps running ({}) but no trust to probe them",
                    chromium_running.join(", ")
                ),
                ErrorClass::Permission,
                "fix accessibility-permission first; the AXManualAccessibility opt-in needs it",
            );
        }
        // Probe: if a running Chromium app exposes at least one window with a
        // tree, the opt-in (or a system AX client) is already in effect.
        for app in &chromium_running {
            if let Ok(scan) = ax_edit::find_text_fields(app, 1) {
                if scan.windows > 0 {
                    return CheckOutcome::pass(format!(
                        "{app} exposes {} window(s): accessibility tree is on",
                        scan.windows
                    ));
                }
            }
        }
        CheckOutcome::warn(
            format!(
                "Chromium app(s) running ({}) but exposing no tree",
                chromium_running.join(", ")
            ),
            ErrorClass::Configuration,
            "the client must set AXManualAccessibility=true on the app element before reading \
             it (ax-edit does this in find_text_fields); if windows are still zero, check they \
             are on the current Space",
        )
    }
}

/// Names of running Chromium-family processes we care about, via `pgrep`.
fn running_chromium_apps() -> Vec<String> {
    let mut found = Vec::new();
    for (proc_pat, ax_name) in [
        ("Google Chrome", "Google Chrome"),
        ("Electron", "Electron"),
        ("Code Helper", "Visual Studio Code"),
        ("Slack", "Slack"),
    ] {
        let ok = Command::new("pgrep")
            .arg("-x")
            .arg(proc_pat)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            found.push(ax_name.to_string());
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Display server
// ---------------------------------------------------------------------------

/// What kind of display session this is. Injection strategy differs entirely:
/// Wayland forbids synthetic input without a portal, X11 allows XTEST,
/// headless/SSH means there is no session to inject into at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayKind {
    MacOsAqua,
    /// A Windows interactive desktop (window station WinSta0).
    WindowsDesktop,
    Wayland,
    X11,
    HeadlessOrSsh,
}

/// Pure env-based detection so it is unit-testable and honest about SSH:
/// an SSH session can still have DISPLAY forwarded, but injection then acts
/// on a remote X server, which is almost never what the user means.
pub fn detect_display(env: &Env, is_macos: bool) -> DisplayKind {
    detect_display_on(env, is_macos, cfg!(target_os = "windows"))
}

/// The detection above with the platform passed in, so both platform
/// branches are unit-testable from any host. `detect_display` is the
/// production entry point that supplies the real `cfg!`.
pub fn detect_display_on(env: &Env, is_macos: bool, is_windows: bool) -> DisplayKind {
    if env.get("SSH_TTY").is_some() || env.get("SSH_CONNECTION").is_some() {
        return DisplayKind::HeadlessOrSsh;
    }
    if is_macos {
        return DisplayKind::MacOsAqua;
    }
    // Windows has no display-server environment variable to probe: an
    // interactive logon always has a desktop, and the non-interactive cases
    // (a service, SSH into Windows) are caught by the SSH check above or by
    // the caller running headless anyway. Passed in rather than sniffed
    // because there is nothing honest to sniff.
    if is_windows {
        return DisplayKind::WindowsDesktop;
    }
    if env.get("WAYLAND_DISPLAY").is_some() {
        return DisplayKind::Wayland;
    }
    if env.get("DISPLAY").is_some() {
        return DisplayKind::X11;
    }
    DisplayKind::HeadlessOrSsh
}

pub struct DisplayServer;

impl Check for DisplayServer {
    fn name(&self) -> &'static str {
        "display-server"
    }

    fn run(&self, env: &Env) -> CheckOutcome {
        match detect_display(env, cfg!(target_os = "macos")) {
            DisplayKind::MacOsAqua => CheckOutcome::pass("macOS Aqua session"),
            DisplayKind::WindowsDesktop => CheckOutcome::pass(
                "Windows interactive desktop (UI Automation and SendInput available)",
            ),
            DisplayKind::X11 => CheckOutcome::pass("X11 session (XTEST injection available)"),
            DisplayKind::Wayland => CheckOutcome::warn(
                "Wayland session: synthetic input is blocked by design",
                ErrorClass::Environment,
                "injection needs the RemoteDesktop XDG portal (or wlroots virtual-keyboard \
                 protocol); AT-SPI reads may still work. Not yet implemented here",
            ),
            DisplayKind::HeadlessOrSsh => CheckOutcome::fail(
                "headless or SSH session: no local display to read or inject into",
                ErrorClass::Environment,
                "run this on the machine's own graphical session, not over SSH",
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal emulator identification
// ---------------------------------------------------------------------------

/// What terminal we are inside and whether text can be injected into it.
/// Terminals are read-only through accessibility, so paste is the only path,
/// and multiplexers (tmux/screen) add a layer that eats bracketed paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    /// Human-readable identification, e.g. "iTerm2 (inside tmux)".
    pub name: String,
    /// Whether paste-style injection is expected to reach the visible prompt.
    pub injectable: bool,
    /// Why not, when not.
    pub caveat: Option<String>,
}

/// Pure env-based terminal identification. TERM_PROGRAM identifies emulators,
/// TMUX/STY identify multiplexers, SSH_TTY means the terminal is remote.
pub fn detect_terminal(env: &Env) -> Option<TerminalInfo> {
    let term = env.get("TERM")?; // no TERM at all: not in a terminal
    let base = match env.get("TERM_PROGRAM") {
        Some("Apple_Terminal") => "Terminal.app".to_string(),
        Some("iTerm.app") => "iTerm2".to_string(),
        Some("vscode") => "VS Code integrated terminal".to_string(),
        Some("WarpTerminal") => "Warp".to_string(),
        Some(other) => other.to_string(),
        None if term.starts_with("xterm") => format!("unknown ({term})"),
        None => format!("unknown ({term})"),
    };
    let mut name = base;
    let mut caveat = None;
    let mut injectable = true;
    if env.get("SSH_TTY").is_some() {
        // Injecting locally cannot reach a remote shell's prompt.
        injectable = false;
        caveat = Some("SSH session: local injection cannot reach the remote shell".into());
        name.push_str(" (over SSH)");
    } else if env.get("TMUX").is_some() {
        name.push_str(" (inside tmux)");
        caveat =
            Some("tmux intercepts paste; enable bracketed paste or use `tmux load-buffer`".into());
    } else if env.get("STY").is_some() {
        name.push_str(" (inside GNU screen)");
        caveat = Some("screen intercepts paste; injection may need `screen -X paste`".into());
    }
    Some(TerminalInfo {
        name,
        injectable,
        caveat,
    })
}

pub struct TerminalEmulator;

impl Check for TerminalEmulator {
    fn name(&self) -> &'static str {
        "terminal-emulator"
    }

    fn run(&self, env: &Env) -> CheckOutcome {
        match detect_terminal(env) {
            None => CheckOutcome::pass("not running inside a terminal (GUI launch)"),
            Some(info) => {
                if info.injectable && info.caveat.is_none() {
                    CheckOutcome::pass(format!("{}: paste injection expected to work", info.name))
                } else {
                    let caveat = info.caveat.unwrap_or_default();
                    CheckOutcome::warn(
                        format!("{}: {}", info.name, caveat),
                        ErrorClass::Environment,
                        caveat_to_remedy(info.injectable),
                    )
                }
            }
        }
    }
}

fn caveat_to_remedy(injectable: bool) -> String {
    if injectable {
        "test dictation into this terminal specifically; if paste is garbled, follow the \
         multiplexer note in docs/debugging.md"
            .into()
    } else {
        "dictate into applications on this machine's own session; the remote shell cannot be \
         reached by local injection"
            .into()
    }
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

/// Clipboard availability. The paste fallback is the universal rewrite path,
/// so a broken clipboard silently disables editing in read-only fields.
pub struct Clipboard;

impl Check for Clipboard {
    fn name(&self) -> &'static str {
        "clipboard"
    }

    fn run(&self, env: &Env) -> CheckOutcome {
        if cfg!(target_os = "macos") {
            // pbpaste exiting zero proves the pasteboard server is reachable.
            match Command::new("pbpaste").output() {
                Ok(o) if o.status.success() => CheckOutcome::pass("pasteboard reachable"),
                _ => CheckOutcome::fail(
                    "pasteboard server unreachable",
                    ErrorClass::Environment,
                    "log into a real GUI session; the pasteboard does not exist in a bare \
                     SSH/daemon context",
                ),
            }
        } else if env.get("WAYLAND_DISPLAY").is_some() {
            CheckOutcome::warn(
                "Wayland clipboard requires wl-clipboard",
                ErrorClass::Configuration,
                "install wl-clipboard (`wl-copy`/`wl-paste`)",
            )
        } else if env.get("DISPLAY").is_some() {
            CheckOutcome::warn(
                "X11 clipboard requires xclip or xsel",
                ErrorClass::Configuration,
                "install xclip (`apt install xclip`) or xsel",
            )
        } else {
            CheckOutcome::fail(
                "no display: no clipboard",
                ErrorClass::Environment,
                "run inside a graphical session",
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Audio input
// ---------------------------------------------------------------------------

/// Is there any input device at all. A machine with no microphone (Mac mini,
/// headless box) fails much earlier than the permission layer, and the two
/// must not be confused.
pub struct AudioInput;

impl Check for AudioInput {
    fn name(&self) -> &'static str {
        "audio-input"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::warn(
                "audio device probe unimplemented off macOS",
                ErrorClass::Environment,
                "verify an input device exists with your platform's audio tool (e.g. \
                 `arecord -l`)",
            );
        }
        // system_profiler is slow (~1s) but dependency-free and honest.
        let out = Command::new("system_profiler")
            .arg("SPAudioDataType")
            .arg("-detailLevel")
            .arg("mini")
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                if text.contains("Input Channels") {
                    CheckOutcome::pass("at least one audio input device present")
                } else {
                    CheckOutcome::fail(
                        "no audio input device found",
                        ErrorClass::Environment,
                        "connect a microphone (or check System Settings > Sound > Input); \
                         dictation has nothing to capture without one",
                    )
                }
            }
            _ => CheckOutcome::warn(
                "could not enumerate audio devices",
                ErrorClass::Environment,
                "check System Settings > Sound > Input manually",
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Model files
// ---------------------------------------------------------------------------

/// Recognizer model files present on disk. The recognizer is not wired up in
/// M0, so absence is Warn, not Fail; the check exists now so onboarding can
/// name the download command instead of crashing at first dictation.
pub struct ModelFiles;

/// Where models will live. Kept as a function so tests and future config can
/// override the base directory.
pub fn model_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    // DELIBERATELY still `.aqua-oss`, not renamed with the product. The
    // copy-then-verify migration that is right for a 2KB config file is
    // indefensible for multi-gigabyte model weights: it either doubles disk
    // usage or performs a move that is not crash-safe, and a half-moved
    // model directory after a power cut is a re-download the user never
    // agreed to. The path is not user-facing. See the rename commit.
    Path::new(&home).join(".aqua-oss/models")
}

impl Check for ModelFiles {
    fn name(&self) -> &'static str {
        "model-files"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        let dir = model_dir();
        let has_model = std::fs::read_dir(&dir)
            .map(|entries| {
                entries.flatten().any(|e| {
                    let name = e.file_name().to_string_lossy().to_lowercase();
                    name.ends_with(".onnx") || name.ends_with(".gguf") || name.ends_with(".bin")
                })
            })
            .unwrap_or(false);
        if has_model {
            CheckOutcome::pass(format!("model file(s) present in {}", dir.display()))
        } else {
            CheckOutcome::warn(
                format!("no recognizer model in {}", dir.display()),
                ErrorClass::Configuration,
                "expected once a recognizer is wired up (M1); then: download Parakeet TDT \
                 ONNX into that directory (see README)",
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Disk space
// ---------------------------------------------------------------------------

/// Free disk space. Models are gigabytes; a full disk fails downloads with
/// opaque errors mid-stream, so name it up front.
pub struct DiskSpace;

/// Threshold below which we warn. Parakeet TDT + a small LLM fit in ~4 GiB.
const MIN_FREE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

impl Check for DiskSpace {
    fn name(&self) -> &'static str {
        "disk-space"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        match free_bytes_at(Path::new("/")) {
            Some(free) => {
                let gib = free as f64 / (1024.0 * 1024.0 * 1024.0);
                if free >= MIN_FREE_BYTES {
                    CheckOutcome::pass(format!("{gib:.1} GiB free"))
                } else {
                    CheckOutcome::warn(
                        format!("only {gib:.1} GiB free; models need ~4 GiB"),
                        ErrorClass::Environment,
                        "free at least 4 GiB before downloading recognizer models",
                    )
                }
            }
            None => CheckOutcome::warn(
                "could not stat filesystem",
                ErrorClass::Environment,
                "check free space manually with `df -h /`",
            ),
        }
    }
}

/// statvfs via libc-free shelling to `df -k`, to avoid a new dependency in a
/// spike crate. Parsing is defensive: any surprise shape returns None.
fn free_bytes_at(path: &Path) -> Option<u64> {
    let out = Command::new("df").arg("-k").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_df_free_kib(&text).map(|kib| kib * 1024)
}

/// Parse `df -k` output: second line, fourth column is available KiB.
pub fn parse_df_free_kib(df_output: &str) -> Option<u64> {
    let line = df_output.lines().nth(1)?;
    line.split_whitespace().nth(3)?.parse().ok()
}

// ---------------------------------------------------------------------------
// CPU features
// ---------------------------------------------------------------------------

/// SIMD capability for ONNX Runtime. On x86, ORT's fast kernels want AVX2;
/// without it inference is several times slower and the latency budget dies.
/// On Apple Silicon NEON is always present, so aarch64 is an automatic pass.
pub struct CpuFeatures;

impl Check for CpuFeatures {
    fn name(&self) -> &'static str {
        "cpu-features"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        #[cfg(target_arch = "aarch64")]
        {
            CheckOutcome::pass("aarch64: NEON always available (ONNX fast path ok)")
        }
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                CheckOutcome::pass("x86_64 with AVX2 (ONNX fast path ok)")
            } else {
                CheckOutcome::warn(
                    "x86_64 WITHOUT AVX2: ONNX inference will be several times slower",
                    ErrorClass::Environment,
                    "expect degraded latency; prefer a smaller model (Moonshine tiny) or \
                     newer hardware",
                )
            }
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            CheckOutcome::warn(
                "unrecognized CPU architecture",
                ErrorClass::Environment,
                "no optimized inference path known for this architecture",
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Platform version
// ---------------------------------------------------------------------------

/// OS version gate. The AX behaviors this project depends on were validated
/// on macOS 13+; older systems have a different TCC layout and the remedies
/// in this doctor would be wrong there.
pub struct PlatformVersion;

impl Check for PlatformVersion {
    fn name(&self) -> &'static str {
        "platform-version"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::pass("non-macOS: no version gate defined yet");
        }
        let out = Command::new("sw_vers").arg("-productVersion").output();
        match out {
            Ok(o) if o.status.success() => {
                let ver = String::from_utf8_lossy(&o.stdout).trim().to_string();
                match macos_major(&ver) {
                    Some(major) if major >= 13 => {
                        CheckOutcome::pass(format!("macOS {ver} (validated on 13+)"))
                    }
                    Some(_) => CheckOutcome::fail(
                        format!("macOS {ver} is older than the validated floor (13.0)"),
                        ErrorClass::Environment,
                        "upgrade to macOS 13 or newer; TCC and AX behavior differ below that",
                    ),
                    None => CheckOutcome::warn(
                        format!("unparseable macOS version '{ver}'"),
                        ErrorClass::Bug,
                        "file a GitHub issue including this doctor output",
                    ),
                }
            }
            _ => CheckOutcome::warn(
                "sw_vers not runnable",
                ErrorClass::Environment,
                "check `sw_vers -productVersion` manually",
            ),
        }
    }
}

/// Extract the major version from strings like "14.5" or "26.0.1".
pub fn macos_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Status;

    // -- terminal detection --------------------------------------------------

    #[test]
    fn no_term_means_gui_launch() {
        assert_eq!(detect_terminal(&Env::from_pairs(&[])), None);
    }

    #[test]
    fn identifies_iterm() {
        let env = Env::from_pairs(&[("TERM", "xterm-256color"), ("TERM_PROGRAM", "iTerm.app")]);
        let info = detect_terminal(&env).unwrap();
        assert_eq!(info.name, "iTerm2");
        assert!(info.injectable);
        assert!(info.caveat.is_none());
    }

    #[test]
    fn tmux_adds_caveat_but_stays_injectable() {
        let env = Env::from_pairs(&[
            ("TERM", "screen-256color"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("TMUX", "/tmp/tmux-501/default,123,0"),
        ]);
        let info = detect_terminal(&env).unwrap();
        assert!(info.name.contains("tmux"));
        assert!(info.injectable);
        assert!(info.caveat.is_some());
    }

    #[test]
    fn screen_detected_via_sty() {
        let env = Env::from_pairs(&[("TERM", "screen"), ("STY", "1234.pts-0.host")]);
        let info = detect_terminal(&env).unwrap();
        assert!(info.name.contains("screen"));
    }

    #[test]
    fn ssh_kills_injectability_and_beats_tmux_label() {
        let env = Env::from_pairs(&[
            ("TERM", "xterm"),
            ("SSH_TTY", "/dev/ttys003"),
            ("TMUX", "x"),
        ]);
        let info = detect_terminal(&env).unwrap();
        assert!(!info.injectable);
        assert!(info.name.contains("SSH"));
    }

    // -- display detection ---------------------------------------------------

    #[test]
    fn ssh_wins_even_with_forwarded_display() {
        // DISPLAY forwarded over SSH points at a REMOTE X server; injecting
        // there is never what a local dictation user means.
        let env = Env::from_pairs(&[("SSH_CONNECTION", "1 2 3 4"), ("DISPLAY", ":10")]);
        assert_eq!(detect_display(&env, false), DisplayKind::HeadlessOrSsh);
    }

    #[test]
    fn wayland_beats_x11_when_both_present() {
        // XWayland sets DISPLAY too; WAYLAND_DISPLAY is the real session type.
        let env = Env::from_pairs(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]);
        assert_eq!(detect_display(&env, false), DisplayKind::Wayland);
    }

    #[test]
    fn bare_display_is_x11_and_nothing_is_headless() {
        assert_eq!(
            detect_display(&Env::from_pairs(&[("DISPLAY", ":0")]), false),
            DisplayKind::X11
        );
        assert_eq!(
            detect_display(&Env::from_pairs(&[]), false),
            DisplayKind::HeadlessOrSsh
        );
    }

    #[test]
    fn windows_is_a_desktop_unless_ssh() {
        // No env var says "there is a desktop" on Windows, so the platform
        // itself is the signal; SSH into Windows must still read headless,
        // because a remote session has no local window station to inject to.
        assert_eq!(
            detect_display_on(&Env::from_pairs(&[]), false, true),
            DisplayKind::WindowsDesktop
        );
        assert_eq!(
            detect_display_on(
                &Env::from_pairs(&[("SSH_CONNECTION", "1 2 3 4")]),
                false,
                true
            ),
            DisplayKind::HeadlessOrSsh
        );
        // macOS wins if both flags are somehow set: the cfg!s are mutually
        // exclusive in production, and this pins the precedence anyway.
        assert_eq!(
            detect_display_on(&Env::from_pairs(&[]), true, true),
            DisplayKind::MacOsAqua
        );
    }

    #[test]
    fn macos_is_aqua_unless_ssh() {
        assert_eq!(
            detect_display(&Env::from_pairs(&[]), true),
            DisplayKind::MacOsAqua
        );
        assert_eq!(
            detect_display(&Env::from_pairs(&[("SSH_TTY", "/dev/ttys0")]), true),
            DisplayKind::HeadlessOrSsh
        );
    }

    // -- codesign parsing ----------------------------------------------------

    #[test]
    fn adhoc_signature_warns_with_tccutil_remedy() {
        let stderr = "Executable=/x/AquaSpike\nIdentifier=dev.aquaoss.spike\n\
                      Format=bundle with Mach-O thin (arm64)\nSignature=adhoc\n";
        let out = classify_codesign_output(true, stderr);
        assert_eq!(out.status, Status::Warn);
        assert_eq!(out.class, Some(ErrorClass::Configuration));
        assert!(out.remedy.unwrap().contains("tccutil reset"));
    }

    #[test]
    fn developer_id_passes_and_names_identity() {
        let stderr = "Executable=/x\nIdentifier=dev.aquaoss.spike\n\
                      Authority=Developer ID Application: Example Corp (ABC123)\n\
                      Authority=Developer ID Certification Authority\n";
        let out = classify_codesign_output(true, stderr);
        assert_eq!(out.status, Status::Pass);
        assert!(out.detail.contains("Example Corp"));
    }

    #[test]
    fn unsigned_binary_fails_as_configuration() {
        let out = classify_codesign_output(false, "code object is not signed at all\n");
        assert_eq!(out.status, Status::Fail);
        assert_eq!(out.class, Some(ErrorClass::Configuration));
    }

    // -- misc pure helpers ---------------------------------------------------

    #[test]
    fn bundle_path_detection() {
        assert!(path_is_in_bundle(Path::new(
            "/x/dist/OutLoud.app/Contents/MacOS/AquaSpike"
        )));
        assert!(!path_is_in_bundle(Path::new("/x/target/debug/doctor")));
    }

    #[test]
    fn shell_launch_detected_from_term_markers() {
        assert!(launched_from_shell(&Env::from_pairs(&[("TERM", "xterm")])));
        assert!(!launched_from_shell(&Env::from_pairs(&[])));
    }

    #[test]
    fn launchservices_marker_overrides_leaked_term() {
        // scripts/doctor.sh uses `open --env`, which leaks the caller's TERM
        // into the launched app. The explicit marker must win, or a correct
        // launch would be misdiagnosed as a shell launch.
        let env = Env::from_pairs(&[("TERM", "xterm"), ("AQUA_LAUNCHED_VIA_LS", "1")]);
        assert!(!launched_from_shell(&env));
    }

    #[test]
    fn df_parsing_reads_available_column() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                  /dev/disk3s1s1 971350180 10485760 419430400 9% /\n";
        assert_eq!(parse_df_free_kib(df), Some(419430400));
        assert_eq!(parse_df_free_kib("garbage"), None);
    }

    #[test]
    fn macos_version_gate_parses_majors() {
        assert_eq!(macos_major("14.5"), Some(14));
        assert_eq!(macos_major("26.0.1"), Some(26));
        assert_eq!(macos_major("beta"), None);
    }

    /// The constant must match what the bundle script actually writes.
    ///
    /// A shell script cannot read a Rust constant, so this is the seam where
    /// the two can drift apart, and it has drifted before: after two product
    /// renames the diagnostics still named `dev.aquaoss.spike` while the app
    /// shipped as something else entirely. Nothing failed loudly, because
    /// `tccutil` prints "Successfully reset" for an identifier it has never
    /// heard of, so the doctor's advice looked like it worked while changing
    /// nothing at all.
    #[test]
    fn bundle_id_matches_the_bundle_script() {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/bundle-outloud-macos.sh");
        let Ok(text) = std::fs::read_to_string(&script) else {
            // Absent in a packaged crate; nothing to compare against.
            return;
        };

        let declared = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("BUNDLE_ID="))
            .map(|v| v.trim().trim_matches('"').to_string())
            .expect("bundle script no longer declares BUNDLE_ID");

        assert_eq!(
            declared,
            crate::BUNDLE_ID,
            "diag::BUNDLE_ID is {}, but {} signs the app as {}. Every remedy \
             string that tells a user to run `tccutil reset` is now wrong, and \
             wrong silently.",
            crate::BUNDLE_ID,
            script.display(),
            declared
        );
    }

    /// No remedy may hardcode a bundle identifier.
    ///
    /// The constant only helps if it is the single source. This catches a new
    /// literal being pasted back in, which is exactly how the last drift
    /// happened.
    #[test]
    fn no_remedy_hardcodes_a_bundle_id() {
        let source = include_str!("checks.rs");
        // Split the needle so this test does not match its own source.
        let needle = concat!("tccutil reset Accessibility ", "dev.");
        for (n, line) in source.lines().enumerate() {
            // Skip prose, including this test's own explanation of the bug.
            if line.trim_start().starts_with("//") {
                continue;
            }
            assert!(
                !line.contains(needle),
                "line {} hardcodes a bundle id in a remedy; use crate::BUNDLE_ID: {}",
                n + 1,
                line.trim()
            );
        }
    }
}
