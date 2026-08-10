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
/// Whether the running process is the doctor's own bundle rather than the
/// app being diagnosed.
///
/// TCC grants are pinned per bundle, so OutLoudDoctor.app answering "am I
/// trusted" describes itself, not OutLoud.app. Presenting that as the app's
/// permission state produces a confident wrong answer, which is worse than
/// no answer: I acted on one today and spent an hour chasing a permission
/// that was already granted.
fn is_separate_doctor_bundle() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains("OutLoudDoctor.app"))
        .unwrap_or(false)
}

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
        // TCC grants are per-bundle, and the doctor is its own bundle
        // (OutLoudDoctor.app). A FAIL here says nothing about whether
        // OutLoud.app is trusted, and reading it as if it did cost a long
        // detour today: the doctor reported both permissions missing while
        // the app had both.
        if !launched_from_shell && is_separate_doctor_bundle() {
            return CheckOutcome::warn(
                "this is the DOCTOR's own grant, not the app's: TCC pins \
                 permissions per bundle and these are different bundles",
                ErrorClass::Configuration,
                "ask the app itself: dist/OutLoud.app/Contents/MacOS/OutLoud --permissions",
            );
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
/// therefore sets OUTLOUD_LAUNCHED_VIA_LS=1 explicitly, which overrides the
/// shell markers: LaunchServices decides the responsible process regardless
/// of what env vars leaked through. The pre-rename AQUA_LAUNCHED_VIA_LS is
/// still accepted because older wrapper scripts in the wild set it.
pub fn launched_from_shell(env: &Env) -> bool {
    if env.get("OUTLOUD_LAUNCHED_VIA_LS").is_some() || env.get("AQUA_LAUNCHED_VIA_LS").is_some() {
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
        // -dvv, not -dv: `Authority=` lines only appear at the second
        // verbosity level. With -dv this check could never name ANY
        // identity and reported "signed by unknown identity" for a
        // correctly signed app, which reads like a problem and is not one.
        let out = match Command::new("codesign").arg("-dvv").arg(&exe).output() {
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
                "after every rebuild run `tccutil reset Accessibility {}` and re-grant. To \
                 stop this permanently, sign with ANY codesigning identity: a free Apple \
                 Development certificate is enough (Developer ID is for distributing to \
                 other machines, not for TCC identity on your own). \
                 scripts/bundle-outloud-macos.sh picks one up automatically if the keychain \
                 has it",
                crate::BUNDLE_ID
            ),
        );
    }
    // The FIRST Authority line is the leaf certificate (the signer). The
    // rest are the chain up to Apple's root, which nobody needs here.
    let identity = stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("Authority="))
        .unwrap_or("unknown identity");
    // The team identifier is what actually decides whether a grant survives
    // a rebuild: with one, the designated requirement names the certificate
    // rather than this build's hash. Saying so turns a bare "signed by X"
    // into an answer to the question the user actually has.
    let team = stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("TeamIdentifier="))
        .filter(|t| *t != "not set");
    match team {
        Some(t) => CheckOutcome::pass(format!(
            "signed by {identity} (team {t}); grants survive rebuilds"
        )),
        None => CheckOutcome::pass(format!("signed by {identity}")),
    }
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
            DisplayKind::Wayland => wayland_injection_outcome(),
            DisplayKind::HeadlessOrSsh => CheckOutcome::fail(
                "headless or SSH session: no local display to read or inject into",
                ErrorClass::Environment,
                "run this on the machine's own graphical session, not over SSH",
            ),
        }
    }
}

/// The Wayland arm of [`DisplayServer`], split out because it is no longer
/// a single fixed WARN: whether injection actually works here is a runtime
/// fact (is `wtype` on `PATH`, and separately, does the compositor
/// implement the virtual-keyboard protocol `wtype` needs), and this
/// function is where those two questions get an honest answer instead of a
/// blanket "not yet implemented" that was true when it was Windows-first
/// (see `crates/text-target/src/targets/keys.rs::WtypeTarget` for the
/// actual transport this now names).
///
/// What this CANNOT determine from here: whether the compositor itself
/// exposes `zwp_virtual_keyboard_manager_v1`. Hyprland and other wlroots
/// compositors do; GNOME (Mutter) and KDE (KWin) deliberately do not, for
/// the same reason a browser refuses an unprivileged
/// `document.execCommand('paste')` -- an unauthenticated virtual keyboard
/// is a keylogger-adjacent capability. There is no environment variable or
/// file to probe for that support; the only way to know is to actually run
/// `wtype`, which this check will not do (a doctor that types things is a
/// doctor with side effects). So a PASS here means "wtype is installed and
/// SHOULD work on a wlroots-family compositor", not "injection is proven to
/// work"; the WARN case names the gap plainly instead of asserting either
/// way.
fn wayland_injection_outcome() -> CheckOutcome {
    wayland_injection_outcome_for(command_on_path("wtype"), has_wl_clipboard_tools())
}

/// Same decision as [`wayland_injection_outcome`], with the two `PATH`
/// facts passed in rather than probed, so the WARN/PASS/FAIL split is
/// unit-tested directly (the same `_on`/`_for` split
/// `detect_display`/`detect_display_on` already use in this file): probing
/// the REAL `PATH` from a test would make the result depend on whatever
/// happens to be installed on the machine running `cargo test`, which is
/// exactly the flakiness this pattern exists to avoid.
fn wayland_injection_outcome_for(has_wtype: bool, has_wl_clipboard: bool) -> CheckOutcome {
    match (has_wtype, has_wl_clipboard) {
        (true, _) => CheckOutcome::pass(
            "Wayland session, wtype on PATH: synthetic-key injection available on \
             compositors implementing zwp_virtual_keyboard_v1 (Hyprland, Sway, other \
             wlroots compositors; GNOME and KDE do not expose this protocol, so wtype \
             will refuse there even though it is installed)",
        ),
        (false, true) => CheckOutcome::warn(
            "Wayland session: wl-clipboard is installed but wtype is not, so only the \
             clipboard-paste fallback is available (no synthetic-key typing)",
            ErrorClass::Configuration,
            "install wtype for the primary typing path (nixpkgs: `wtype`); \
             clipboard-paste alone still works for insertion but cannot address \
             existing text for edit-by-voice",
        ),
        (false, false) => CheckOutcome::fail(
            "Wayland session: neither wtype nor wl-clipboard is on PATH, so this build \
             has no way to deliver text to the focused window at all",
            ErrorClass::Configuration,
            "install wtype (primary path) and wl-clipboard (fallback + clipboard \
             checks): nixpkgs packages `wtype` and `wl-clipboard`",
        ),
    }
}

/// Whether `bin` resolves on `PATH`.
///
/// Deliberately duplicated rather than depending on
/// `text_target::detect::SystemEnv::has_command`: `diag` has no dependency
/// on `text-target` today (it talks to `ax-edit` and `hotkey` directly, see
/// `Cargo.toml`), and reaching across for one boolean-returning function
/// would add a whole crate edge -- with `text-target`'s own `display`
/// feature and its `ax-edit`/`windows` pulls -- to save four lines. Same
/// technique (`PATH` split, `.is_file()`) as the original; if this drifts
/// from `SystemEnv::has_command` in behavior later, that is the day to
/// promote it to a shared helper instead of before.
fn command_on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

/// Whether BOTH halves of `wl-clipboard` are on `PATH`. A copy tool with no
/// paste tool (or the reverse) is not a usable clipboard, so this is one
/// fact, not two, everywhere it is consulted.
fn has_wl_clipboard_tools() -> bool {
    command_on_path("wl-copy") && command_on_path("wl-paste")
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
        } else if cfg!(target_os = "windows") {
            // Windows has a native clipboard with nothing to install, and no
            // DISPLAY or WAYLAND_DISPLAY: those are X11 and Wayland variables
            // that are never set on a native session. Without this branch the
            // check fell through to the else arm below and told Windows users
            // "no display: no clipboard" as a FAIL, on a machine where the
            // clipboard demonstrably works.
            //
            // A confident wrong answer is worse than no answer, because it
            // stops the search. This project already lost an hour to the same
            // shape when the doctor reported another process's permissions as
            // the app's.
            CheckOutcome::pass("Windows clipboard is native; nothing to install")
        } else if env.get("WAYLAND_DISPLAY").is_some() {
            // Actually probe for the tool rather than always warning: a
            // Hyprland box with wl-clipboard installed (the common case
            // once the nix package ships it as a runtime dep) must read
            // PASS, not carry a permanent WARN that never resolves no
            // matter what is on PATH. The prior version warned
            // unconditionally on Wayland regardless of whether wl-copy/
            // wl-paste were present, which is the "reports FAIL/WARN under
            // SSH, correctly, but ALSO under a real graphical session with
            // everything installed" bug this rewrite closes.
            linux_clipboard_outcome_for(true, has_wl_clipboard_tools())
        } else if env.get("DISPLAY").is_some() {
            linux_clipboard_outcome_for(false, command_on_path("xclip") || command_on_path("xsel"))
        } else {
            CheckOutcome::fail(
                "no display: no clipboard",
                ErrorClass::Environment,
                "run inside a graphical session",
            )
        }
    }
}

/// Linux clipboard verdict as a pure function of "which session" and
/// "is the needed tool present", so the PASS/WARN split is directly
/// unit-tested rather than only exercisable by actually installing or
/// uninstalling `wl-clipboard`/`xclip` on the machine running the test
/// (the same reasoning behind `wayland_injection_outcome_for` above).
/// `is_wayland` picks which tool name and remedy string apply; `has_tool`
/// is the already-resolved "is a usable clipboard tool present" fact
/// (`wl-copy` AND `wl-paste` for Wayland, `xclip` OR `xsel` for X11 -- the
/// caller decides which combinator applies before calling this).
fn linux_clipboard_outcome_for(is_wayland: bool, has_tool: bool) -> CheckOutcome {
    if is_wayland {
        if has_tool {
            CheckOutcome::pass("Wayland: wl-clipboard (wl-copy/wl-paste) on PATH")
        } else {
            CheckOutcome::warn(
                "Wayland clipboard requires wl-clipboard",
                ErrorClass::Configuration,
                "install wl-clipboard (`wl-copy`/`wl-paste`)",
            )
        }
    } else if has_tool {
        CheckOutcome::pass("X11: xclip or xsel on PATH")
    } else {
        CheckOutcome::warn(
            "X11 clipboard requires xclip or xsel",
            ErrorClass::Configuration,
            "install xclip (`apt install xclip`) or xsel",
        )
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
    // Renamed to `~/.outloud` after all, by rename rather than by copy; see
    // config::paths::migrate_model_dir for why that is safe and for the
    // read-only fallback that keeps a pre-rename cache working.
    config::model_dir()
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
                judge_macos_version(&ver)
            }
            _ => CheckOutcome::warn(
                "sw_vers not runnable",
                ErrorClass::Environment,
                "check `sw_vers -productVersion` manually",
            ),
        }
    }
}

/// What a macOS version means for dictation.
///
/// Split from the `sw_vers` probe so every branch is reachable in a test.
/// Only the branch matching the developer's own machine was previously
/// observable, and the branch that matters most is the one they are least
/// likely to be running.
pub fn judge_macos_version(ver: &str) -> CheckOutcome {
    match macos_major(ver) {
        // 26 brought SpeechTranscriber: the default recognizer, no download.
        Some(major) if major >= 26 => {
            CheckOutcome::pass(format!("macOS {ver}: bundled SpeechTranscriber available"))
        }
        // 13..26 is supported, but NOT with the default backend. Warn rather
        // than pass: a pass here is what let someone grant Accessibility,
        // press the hotkey, get silence, and see every check report fine.
        Some(major) if major >= 13 => CheckOutcome::warn(
            format!("macOS {ver} has no bundled recognizer (SpeechTranscriber needs 26+)"),
            ErrorClass::Configuration,
            "dictation works here with the whisper backend: download a ggml model \
             from https://huggingface.co/ggerganov/whisper.cpp, set \
             OUTLOUD_WHISPER_MODEL to it, and run with `--asr whisper`. Without \
             that the recognizer never becomes ready and the hotkey appears to \
             do nothing.",
        ),
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

/// Extract the major version from strings like "14.5" or "26.0.1".
pub fn macos_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// Input Monitoring, the permission whose only symptom is silence.
///
/// Without it the hotkey never fires, so the app looks completely dead:
/// no overlay, no text, no error. It is a SEPARATE grant from
/// Accessibility and lives in a different System Settings pane, which is
/// why "I granted permission and it still does nothing" is such a common
/// report. The doctor did not check it at all, so its most likely cause
/// was the one thing the diagnostics could not name.
pub struct InputMonitoringPermission;

impl Check for InputMonitoringPermission {
    fn name(&self) -> &'static str {
        "input-monitoring-permission"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if !cfg!(target_os = "macos") {
            return CheckOutcome::pass("not macOS: no Input Monitoring grant exists here");
        }
        // Same per-bundle caveat as the accessibility check: this answers
        // for OutLoudDoctor.app, not for the app whose hotkey matters.
        if is_separate_doctor_bundle() {
            return CheckOutcome::warn(
                "this is the DOCTOR's own grant, not the app's: TCC pins \
                 permissions per bundle and these are different bundles",
                ErrorClass::Configuration,
                "ask the app itself: dist/OutLoud.app/Contents/MacOS/OutLoud --permissions",
            );
        }
        if hotkey::has_input_monitoring() {
            CheckOutcome::pass("hotkey can read key events")
        } else {
            CheckOutcome::fail(
                "no Input Monitoring access: the hotkey will never fire, so the app \
                 will appear to do nothing at all",
                ErrorClass::Permission,
                "System Settings > Privacy & Security > Input Monitoring: enable the \
                 toggle for this app. This is a DIFFERENT grant from Accessibility; \
                 both are needed, and ad-hoc rebuilds silently void both",
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Linux hotkey trigger (Wayland compositor-exec backend)
// ---------------------------------------------------------------------------

/// Whether the Linux hotkey-trigger daemon is reachable at all.
///
/// This check exists for the same reason `InputMonitoringPermission` does:
/// a hotkey that never fires must never present as "everything looks fine".
/// Off Linux there is no macOS-style permission to check, so the doctor
/// used to say "not macOS: no Input Monitoring grant exists here" and
/// leave it there, which is TRUE about the permission and silent about the
/// actual question a Linux user has: does the hotkey work AT ALL. On
/// Wayland (`docs/hotkeys.md` #7) the answer defaults to no, because
/// nothing binds a global key without either this crate's compositor-exec
/// transport or a portal neither Hyprland nor most wlroots compositors
/// implement, and the only way to know is to ask whether something is
/// listening on the trigger socket.
pub struct LinuxHotkeyTrigger;

impl Check for LinuxHotkeyTrigger {
    fn name(&self) -> &'static str {
        "linux-hotkey-trigger"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            return CheckOutcome::pass(
                "not applicable: this platform has its own hotkey backend \
                 (see input-monitoring-permission above)",
            );
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let socket = hotkey::backend::linux::default_socket_path();
            if hotkey::backend::linux::daemon_reachable(&socket) {
                return CheckOutcome::pass(format!(
                    "trigger daemon reachable at {}",
                    socket.display()
                ));
            }
            CheckOutcome::fail(
                format!(
                    "no hotkey-trigger daemon reachable at {} -- the hotkey will do \
                     NOTHING until a compositor keybind is wired up and outloud is running",
                    socket.display()
                ),
                ErrorClass::Configuration,
                "start outloud, then add to your Wayland compositor config (Hyprland \
                 example):\n  bind  = , F13, exec, outloud trigger press\n  \
                 bindr = , F13, exec, outloud trigger release\nsee docs/hotkeys.md \
                 section 7 for sway/river and the XDG portal alternative on KDE/GNOME",
            )
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            CheckOutcome::pass("not applicable on this platform")
        }
    }
}

/// Whether the installed bundle is older than the built binary.
///
/// A stale bundle wasted a long stretch of a debugging session: fixes were
/// verified against `target/release/outloud` while the user ran a
/// `dist/OutLoud.app` built ninety minutes earlier, so working code looked
/// broken and the disagreement was invisible from either side.
///
/// Only meaningful in a checkout, where both paths exist. An installed copy
/// has no build tree to compare against, and says so rather than warning
/// about a condition the user cannot act on.
pub struct BundleFreshness;

impl Check for BundleFreshness {
    fn name(&self) -> &'static str {
        "bundle-freshness"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        use std::time::SystemTime;

        // Walk up from the running binary to find a checkout. The doctor may
        // run from target/debug, target/release, or its own bundle, so the
        // repo root is wherever both dist/ and target/ sit together.
        let Ok(exe) = std::env::current_exe() else {
            return CheckOutcome::pass("cannot locate the running binary");
        };
        let root = exe
            .ancestors()
            .find(|a| a.join("dist/OutLoud.app").exists() && a.join("target/release").exists());
        let Some(root) = root else {
            return CheckOutcome::pass(
                "not a build checkout (no dist/ and target/ pair to compare)",
            );
        };
        let bundle = root.join("dist/OutLoud.app/Contents/MacOS/OutLoud");
        let built = root.join("target/release/outloud");

        let mtime = |p: &std::path::Path| -> Option<SystemTime> {
            std::fs::metadata(p).ok()?.modified().ok()
        };
        let (Some(b), Some(t)) = (mtime(&bundle), mtime(&built)) else {
            return CheckOutcome::pass(
                "not a build checkout (no dist/ and target/ pair to compare)",
            );
        };

        match t.duration_since(b) {
            // The binary is newer: the bundle does not contain it.
            // Five minutes, not one: a rebuild takes a while and touching a
            // source file is not a stale bundle. The failure this catches is
            // "I fixed that an hour ago", so the threshold only needs to be
            // well under an hour to be useful, and a false warning teaches
            // people to ignore the check.
            Ok(gap) if gap.as_secs() > 300 => CheckOutcome::warn(
                format!(
                    "the installed bundle is {} minutes older than the built \
                     binary, so it does not contain your latest changes",
                    gap.as_secs() / 60
                ),
                ErrorClass::Configuration,
                "run ./scripts/bundle-outloud-macos.sh, then re-grant \
                 Accessibility and Input Monitoring (an ad-hoc rebuild voids both)",
            ),
            _ => CheckOutcome::pass("bundle is at least as new as the built binary"),
        }
    }
}

/// Judge how many OutLoud daemons are running, from their executable paths.
///
/// WHY THIS CHECK EXISTS: a user reported "it doesn't appear on the menu
/// bar, and it asks for permissions but doesn't work". Nothing was wrong
/// with the permissions. TWO daemons were running -- an installed copy and
/// a freshly built one -- both binding the same hotkey and both opening the
/// microphone. The doctor, whose entire job is answering "it doesn't work",
/// had no check for this and reported everything healthy.
///
/// It was possible because the single-instance lock lived in the temp
/// directory, and on macOS that path is per-context: a Finder launch and a
/// shell launch resolve different directories, so each copy took its own
/// lock and neither saw the other. That is fixed, but a daemon started
/// BEFORE the fix still holds the old path, so the situation outlives the
/// bug and stays worth detecting.
///
/// Pure over a list of paths so it is testable without spawning anything.
pub fn judge_running_daemons(exe_paths: &[String]) -> CheckOutcome {
    // The speech helper is a child process, not a second daemon. Counting
    // it would report two copies for every healthy single run.
    let daemons: Vec<&String> = exe_paths
        .iter()
        .filter(|p| !p.contains("outloud-speech-helper"))
        .collect();

    match daemons.len() {
        0 => CheckOutcome::pass("no OutLoud daemon is running"),
        1 => CheckOutcome::pass(format!("one OutLoud daemon: {}", daemons[0])),
        n => {
            // Name the paths. "Two copies are running" sends someone hunting
            // through Activity Monitor; the paths say which to quit, and
            // usually reveal that one is a build directory.
            let list = daemons
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            CheckOutcome::fail(
                format!("{n} OutLoud daemons are running: {list}"),
                ErrorClass::Configuration,
                "Quit all but one. Two copies both bind the hotkey and both open \
                 the microphone, so one keypress records twice and types twice. \
                 A copy started before the single-instance fix holds the old \
                 lock path and cannot see the others.",
            )
        }
    }
}

/// Turn `pgrep -lf OutLoud` output into a verdict.
///
/// Pure so the interesting cases can be tested without spawning daemons on
/// the developer's machine -- which is itself the bug this check exists to
/// find.
pub fn judge_pgrep_output(text: &str) -> CheckOutcome {
    let paths: Vec<String> = text
        .lines()
        .filter_map(|line| {
            // "PID /path/to/exe [args]" -> the path.
            let rest = line.split_once(' ')?.1;
            let path = rest.split_whitespace().next()?;
            // Only real executables: this must not count the osascript
            // helper that reads the bundle's icon, or the doctor itself.
            path.contains("OutLoud.app/Contents/MacOS/")
                .then(|| path.to_string())
        })
        .collect();

    // pgrep matched processes but nothing parsed into a bundle path. That is
    // NOT "no daemons are running"; it is this check having lost the ability
    // to tell. Saying so beats a green PASS that means nothing -- precisely
    // the failure `pgrep -af` produced, where macOS returns bare pids and
    // the check cheerfully reported an empty machine.
    if paths.is_empty() && !text.trim().is_empty() {
        return CheckOutcome::warn(
            format!(
                "pgrep matched {} process(es) but none looked like an OutLoud bundle; \
                 cannot count daemons",
                text.lines().count()
            ),
            ErrorClass::Bug,
            "Report this with the output of `pgrep -lf OutLoud`: the doctor can no \
             longer detect two copies running, which is a real and confusing failure.",
        );
    }
    judge_running_daemons(&paths)
}

/// Are two OutLoud daemons running at once?
pub struct RunningInstances;

impl Check for RunningInstances {
    fn name(&self) -> &'static str {
        "running-instances"
    }

    fn run(&self, _env: &Env) -> CheckOutcome {
        // `pgrep -lf`, NOT `-af`: on macOS -a is not the "full command
        // line" flag it is on Linux, and returns bare pids. Using it made
        // this check report "no daemon is running" on a machine with one
        // plainly running, which is worse than having no check.
        //
        // Absent or failing pgrep is not a diagnosis, so it degrades to a
        // pass rather than inventing a fault the user cannot act on.
        let out = match Command::new("pgrep").args(["-lf", "OutLoud"]).output() {
            Ok(o) => o,
            Err(_) => return CheckOutcome::pass("pgrep unavailable; cannot count daemons"),
        };
        let text = String::from_utf8_lossy(&out.stdout);
        judge_pgrep_output(&text)
    }
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
        // `detect_display_on(.., is_macos, is_windows)` rather than the
        // ambient wrapper: these assert LINUX session detection, and on a
        // Windows host the wrapper correctly answers WindowsDesktop, so the
        // test failed for being run on the wrong platform rather than for
        // anything being wrong.
        assert_eq!(detect_display_on(&env, false, false), DisplayKind::Wayland);
    }

    #[test]
    fn bare_display_is_x11_and_nothing_is_headless() {
        assert_eq!(
            detect_display_on(&Env::from_pairs(&[("DISPLAY", ":0")]), false, false),
            DisplayKind::X11
        );
        assert_eq!(
            detect_display_on(&Env::from_pairs(&[]), false, false),
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

    // -- Wayland injection / clipboard verdicts -------------------------------

    /// wtype present is a PASS regardless of wl-clipboard: it is the
    /// primary path and does not need the clipboard fallback to be usable.
    #[test]
    fn wtype_present_passes_even_without_wl_clipboard() {
        let out = wayland_injection_outcome_for(true, false);
        assert_eq!(out.status, Status::Pass);
    }

    /// The exact gap this rewrite closes: a Hyprland box with BOTH tools
    /// installed must PASS, not carry the permanent WARN the old
    /// unconditional-warn version gave every Wayland session regardless of
    /// what was on PATH.
    #[test]
    fn wtype_present_with_wl_clipboard_passes() {
        let out = wayland_injection_outcome_for(true, true);
        assert_eq!(out.status, Status::Pass);
    }

    /// No wtype but wl-clipboard present: still usable (clipboard-paste
    /// fallback), but WARN because the primary typing path is missing.
    #[test]
    fn wl_clipboard_without_wtype_warns_not_fails() {
        let out = wayland_injection_outcome_for(false, true);
        assert_eq!(out.status, Status::Warn);
        assert!(out.remedy.unwrap().contains("wtype"));
    }

    /// Neither tool: this build has no way to deliver text at all, which is
    /// a FAIL (matches the SSH/headless case being correctly a FAIL too),
    /// not a WARN that undersells how broken the setup is.
    #[test]
    fn neither_tool_fails() {
        let out = wayland_injection_outcome_for(false, false);
        assert_eq!(out.status, Status::Fail);
        assert!(out.remedy.unwrap().contains("wtype"));
    }

    #[test]
    fn wayland_clipboard_pass_and_warn_track_the_tool() {
        assert_eq!(linux_clipboard_outcome_for(true, true).status, Status::Pass);
        assert_eq!(
            linux_clipboard_outcome_for(true, false).status,
            Status::Warn
        );
    }

    #[test]
    fn x11_clipboard_pass_and_warn_track_the_tool() {
        assert_eq!(
            linux_clipboard_outcome_for(false, true).status,
            Status::Pass
        );
        assert_eq!(
            linux_clipboard_outcome_for(false, false).status,
            Status::Warn
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
        let env = Env::from_pairs(&[("TERM", "xterm"), ("OUTLOUD_LAUNCHED_VIA_LS", "1")]);
        assert!(!launched_from_shell(&env));
    }

    #[test]
    fn legacy_launchservices_marker_still_wins() {
        // Pre-rename wrapper scripts set the AQUA_ name; a stale copy of
        // doctor.sh must not be misdiagnosed as a shell launch.
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

    // -- macOS version judgement --------------------------------------------

    /// The branch that shipped wrong: macOS 13..26 passed silently.
    ///
    /// SpeechTranscriber is the default recognizer and needs macOS 26. Below
    /// that it never becomes ready, so the hotkey appears to do nothing. The
    /// version row said PASS on the way past, which is worse than saying
    /// nothing: a user who has just granted Accessibility permission and then
    /// gets silence, with every check green, concludes the app is broken.
    #[test]
    fn macos_below_26_warns_and_names_the_working_backend() {
        for ver in ["13.0", "14.6.1", "15.2", "25.9"] {
            let outcome = judge_macos_version(ver);
            assert_eq!(
                outcome.status,
                Status::Warn,
                "macOS {ver} must not pass silently: {outcome:?}"
            );
            let remedy = outcome.remedy.unwrap_or_default();
            assert!(
                remedy.contains("--asr whisper"),
                "the remedy must name the backend that works: {remedy}"
            );
            assert!(
                remedy.contains("OUTLOUD_WHISPER_MODEL"),
                "and the variable that points at the model: {remedy}"
            );
        }
    }

    /// 26+ has the bundled recognizer, so nothing needs configuring.
    #[test]
    fn macos_26_and_later_passes() {
        for ver in ["26.0", "26.5.2", "27.0"] {
            assert_eq!(
                judge_macos_version(ver).status,
                Status::Pass,
                "macOS {ver} has SpeechTranscriber"
            );
        }
    }

    /// Below 13 the doctor's own remedies are wrong (different TCC layout),
    /// so this stays a hard failure rather than a warning.
    #[test]
    fn macos_below_the_validated_floor_still_fails() {
        assert_eq!(judge_macos_version("12.7").status, Status::Fail);
    }

    /// One daemon plus its speech helper is a HEALTHY single instance.
    ///
    /// The helper is a child process whose path also contains "outloud", so
    /// a naive count reports two copies for every normal run. A check that
    /// cries wolf on the healthy case gets ignored, and then it protects
    /// nothing.
    #[test]
    fn a_daemon_and_its_helper_is_one_instance() {
        let outcome = judge_running_daemons(&[
            "/Applications/OutLoud.app/Contents/MacOS/OutLoud".into(),
            "/Applications/OutLoud.app/Contents/MacOS/outloud-speech-helper".into(),
        ]);
        assert_eq!(outcome.status, Status::Pass, "{}", outcome.detail);
    }

    /// Two daemons is the reported failure, and must FAIL, not warn.
    ///
    /// The user saw no menu bar icon and permissions that did nothing. Both
    /// symptoms came from two copies fighting over the hotkey and the
    /// microphone, while the doctor reported everything healthy.
    #[test]
    fn two_daemons_fail_and_name_both_paths() {
        let outcome = judge_running_daemons(&[
            "/Applications/OutLoud.app/Contents/MacOS/OutLoud".into(),
            "/Users/x/outloud/dist/OutLoud.app/Contents/MacOS/OutLoud".into(),
        ]);
        assert_eq!(outcome.status, Status::Fail);
        assert!(
            outcome.detail.contains("/Applications/") && outcome.detail.contains("/dist/"),
            "both paths must be named so the user knows which to quit: {}",
            outcome.detail
        );
        assert!(
            outcome.remedy.is_some(),
            "a failure without a remedy is a complaint"
        );
    }

    /// No daemon is not a failure: the doctor runs before a first launch.
    #[test]
    fn no_daemon_is_not_a_failure() {
        assert_eq!(judge_running_daemons(&[]).status, Status::Pass);
    }

    /// Bare pids -- what `pgrep -af` returns on macOS -- must NOT read as
    /// an empty machine.
    ///
    /// This is the exact bug that shipped for one commit: the flag was
    /// wrong, every line failed to parse, and the check reported "no
    /// OutLoud daemon is running" on a machine running one. A check that
    /// silently sees nothing is worse than no check, because it actively
    /// tells you the thing you are looking for is not there.
    #[test]
    fn unparseable_output_warns_instead_of_claiming_an_empty_machine() {
        let outcome = judge_pgrep_output("66061\n66065\n");
        assert_eq!(
            outcome.status,
            Status::Warn,
            "bare pids must not read as 'no daemons': {}",
            outcome.detail
        );
        assert_eq!(outcome.class, Some(ErrorClass::Bug));
    }

    /// Genuinely empty output is genuinely no daemons.
    #[test]
    fn empty_output_is_an_empty_machine() {
        assert_eq!(judge_pgrep_output("").status, Status::Pass);
        assert_eq!(judge_pgrep_output("   \n").status, Status::Pass);
    }

    /// The real macOS format parses, and the helper does not inflate it.
    #[test]
    fn real_pgrep_output_counts_one_daemon() {
        let outcome = judge_pgrep_output(
            "66061 /Applications/OutLoud.app/Contents/MacOS/OutLoud\n\
             66065 /Applications/OutLoud.app/Contents/MacOS/outloud-speech-helper\n",
        );
        assert_eq!(outcome.status, Status::Pass, "{}", outcome.detail);
        assert!(
            outcome.detail.contains("one OutLoud daemon"),
            "{}",
            outcome.detail
        );
    }

    /// Two bundles is the reported failure.
    #[test]
    fn two_bundles_in_pgrep_output_fail() {
        let outcome = judge_pgrep_output(
            "1 /Applications/OutLoud.app/Contents/MacOS/OutLoud\n\
             2 /Users/x/outloud/dist/OutLoud.app/Contents/MacOS/OutLoud\n",
        );
        assert_eq!(outcome.status, Status::Fail, "{}", outcome.detail);
    }
}
