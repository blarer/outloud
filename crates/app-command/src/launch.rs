//! Discovering installed applications, and launching one.
//!
//! The only part of this crate that touches the operating system, kept in
//! one file so the per-platform differences are visible side by side rather
//! than scattered behind `cfg` attributes in the grammar.
//!
//! # No shell, anywhere
//!
//! Every launch here goes through `Command` with the application as a
//! separate argument. Nothing is interpolated into a shell string, which
//! matters more than usual: the input originates in a speech recognizer, so
//! a shell would turn a mis-hearing into arbitrary command execution. An
//! application name containing a space, a quote, or a semicolon is an
//! argument, never syntax.
//!
//! The registry is a second, independent guard: a launch takes an [`App`]
//! that discovery produced, so even a parser bug cannot name a binary that
//! is not an installed application.

use std::io;
use std::process::Command;

use crate::registry::{App, Registry};

/// Discover the applications installed on this machine.
///
/// Returns an empty registry rather than an error when discovery finds
/// nothing. The caller reports that as `Refusal::NoRegistry`, which is
/// honest on a headless box with no desktop applications at all, and is
/// distinguishable from "this one application is missing".
pub fn discover() -> Registry {
    Registry::new(platform::discover())
}

/// Bring `app` to the foreground, starting it if it is not running.
///
/// One operation for both verbs. See the crate docs: this is delegated to
/// the application's own single-instance handling, which is the only route
/// available on all three platforms.
pub fn launch(app: &App) -> io::Result<()> {
    platform::launch(app)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::path::Path;

    /// `/Applications` and the system set, plus the per-user directory.
    ///
    /// Not an exhaustive search of the disk: Spotlight could find an app in
    /// a random folder, but a registry that includes every bundle anywhere
    /// is a registry with surprises in it. These three directories are
    /// where applications a user would name out loud actually live.
    const DIRS: &[&str] = &[
        "/Applications",
        "/Applications/Utilities",
        "/System/Applications",
        "/System/Applications/Utilities",
    ];

    pub fn discover() -> Vec<App> {
        let mut apps = Vec::new();
        let home = std::env::var("HOME").unwrap_or_default();
        let user_dir = format!("{home}/Applications");
        let dirs = DIRS
            .iter()
            .map(Path::new)
            .chain((!home.is_empty()).then_some(Path::new(&user_dir)));
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("app") {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                apps.push(App {
                    name: name.to_string(),
                    // The full path, not the name. `open -a Firefox` works
                    // by searching, which is slower and can resolve to a
                    // different copy than the one enumerated.
                    launch_id: path.to_string_lossy().into_owned(),
                });
            }
        }
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        apps.dedup_by(|a, b| a.name == b.name);
        apps
    }

    pub fn launch(app: &App) -> io::Result<()> {
        // `open -a <path>` both launches and raises: verified 2026-10-30
        // against a running Messages, same PID before and after.
        //
        // Chosen over the `System Events` AppleScript route specifically
        // because this needs NO Accessibility grant. The AppleScript path
        // would make app switching fail silently on a machine where the
        // user has not approved automation, which is the kind of
        // permission-dependent half-working feature this project keeps
        // finding and removing.
        let status = Command::new("/usr/bin/open")
            .arg("-a")
            .arg(&app.launch_id)
            .status()?;
        check(status, &app.name)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::path::{Path, PathBuf};

    pub fn discover() -> Vec<App> {
        let mut apps = Vec::new();
        for dir in entry_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                if let Some(app) = parse_entry(&path) {
                    apps.push(app);
                }
            }
        }
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        apps.dedup_by(|a, b| a.name == b.name);
        apps
    }

    /// The XDG search path for desktop entries, most specific first.
    fn entry_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            let data_home = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&home).join(".local/share"));
            dirs.push(data_home.join("applications"));
        }
        let data_dirs = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
        for d in data_dirs.split(':').filter(|d| !d.is_empty()) {
            dirs.push(PathBuf::from(d).join("applications"));
        }
        // Flatpak exports, which are not always on XDG_DATA_DIRS for a
        // non-login shell even though the desktop shows them.
        dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
        dirs
    }

    /// Read `Name` out of a desktop entry, skipping the ones a launcher
    /// should not offer.
    ///
    /// Measured on the test box: 144 entries, of which only 65 are
    /// user-visible. The rest are `NoDisplay=true` helpers like
    /// `apport-gtk.desktop` ("Report a problem..."). Offering those as
    /// voice-launchable targets would fill the registry with things no user
    /// would ever ask for by name, and every one of them is another chance
    /// for a fuzzy match to land somewhere absurd.
    fn parse_entry(path: &Path) -> Option<App> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut name = None;
        let mut hidden = false;
        let mut is_app = false;
        // Only the `[Desktop Entry]` group. Later groups are per-action
        // ("Open a New Window") and have their own Name keys, which would
        // otherwise overwrite the application's own name.
        let mut in_main = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_main = line == "[Desktop Entry]";
                continue;
            }
            if !in_main {
                continue;
            }
            match line.split_once('=') {
                // Plain `Name`, not `Name[de]`: the localized variants would
                // each overwrite the last, leaving whichever language sorted
                // last in the file.
                Some(("Name", v)) if name.is_none() => name = Some(v.trim().to_string()),
                Some(("NoDisplay", v)) | Some(("Hidden", v)) if v.trim() == "true" => {
                    hidden = true;
                }
                Some(("Type", v)) if v.trim() == "Application" => is_app = true,
                _ => {}
            }
        }
        if hidden || !is_app {
            return None;
        }
        let name = name?;
        if name.is_empty() {
            return None;
        }
        Some(App {
            name,
            launch_id: path.to_string_lossy().into_owned(),
        })
    }

    pub fn launch(app: &App) -> io::Result<()> {
        // `gio launch <entry>` runs the entry's Exec with the environment
        // and startup notification a desktop launcher would provide, and
        // hands activation to the application. An app implementing
        // `org.freedesktop.Application` or GApplication single-instance
        // raises its existing window instead of starting a second copy.
        //
        // Deliberately NOT parsing Exec and running it ourselves: Exec
        // carries field codes (%f, %U, %i) that must be expanded per the
        // spec, and getting that wrong means passing literal "%U" to a
        // browser as a URL.
        //
        // `gtk-launch` is the documented alternative and is NOT used: it
        // takes an entry *id* rather than a path, so two entries with the
        // same basename in different XDG directories are indistinguishable,
        // and it requires a display even to fail (verified on the headless
        // test box: "cannot open display").
        //
        // KNOWN LIMITATION, measured rather than assumed. `gio launch`
        // reports whether the process was SPAWNED, not whether it ran. On
        // the headless test box it exits 0 and the application then dies
        // with "Failed to open display", so `Ok(())` here means "handed off
        // to the launcher", not "the window is now up".
        //
        // Not worked around, because every available workaround is worse:
        // waiting to see if a window appears needs a compositor API that
        // Wayland deliberately does not expose to third parties, and
        // polling for the process is racy for an app that forks or whose
        // activation is handled by an already-running instance (which is
        // the single-instance case this whole design relies on). An honest
        // narrow contract beats a guess that is wrong in the common case.
        //
        // The practical consequence is small: on a real desktop session the
        // display exists, so the failure mode this hides does not occur. On
        // a headless box there is nothing to launch into anyway.
        //
        // That the Exec IS invoked was then verified without a display, by
        // pointing a desktop entry at a script that writes a timestamp file:
        // the file appeared, with clean argv (no literal `%U` leaking
        // through). Done with the shipped aarch64 release artifact rather
        // than a debug build.
        let status = Command::new("gio")
            .arg("launch")
            .arg(&app.launch_id)
            .status()
            .or_else(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    // glib's tooling is not guaranteed present. gtk-launch
                    // takes the entry id, which is the filename without the
                    // .desktop suffix.
                    let id = Path::new(&app.launch_id)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| app.launch_id.clone());
                    Command::new("gtk-launch").arg(id).status()
                } else {
                    Err(e)
                }
            })?;
        check(status, &app.name)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    pub fn discover() -> Vec<App> {
        // Start Menu shortcuts, which is what the Start menu itself lists.
        //
        // VERIFIED on a real windows-2025 runner by
        // `windows-app-command-runtime-smoke` in ci.yml: 158 applications
        // discovered, and the first of them ("7-Zip File Manager") resolves
        // through the whole parse-and-resolve path.
        //
        // This comment previously said the code was unverified and that the
        // `.lnk` display names were the part most likely to be wrong. They
        // were not, but that was a guess either way: nothing in CI ran the
        // test suite on Windows at all, which is why the job exists now.
        let mut apps = Vec::new();
        let dirs = [
            std::env::var("APPDATA")
                .map(|p| format!("{p}\\Microsoft\\Windows\\Start Menu\\Programs")),
            std::env::var("PROGRAMDATA")
                .map(|p| format!("{p}\\Microsoft\\Windows\\Start Menu\\Programs")),
        ];
        for dir in dirs.into_iter().flatten() {
            collect(std::path::Path::new(&dir), &mut apps, 0);
        }
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        apps.dedup_by(|a, b| a.name == b.name);
        apps
    }

    /// Start Menu entries nest (vendor folders), so this recurses, bounded.
    fn collect(dir: &std::path::Path, apps: &mut Vec<App>, depth: usize) {
        if depth > 3 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, apps, depth + 1);
            } else if path.extension().and_then(|e| e.to_str()) == Some("lnk") {
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    apps.push(App {
                        name: name.to_string(),
                        launch_id: path.to_string_lossy().into_owned(),
                    });
                }
            }
        }
    }

    pub fn launch(app: &App) -> io::Result<()> {
        // `cmd /C start "" <shortcut>` resolves the .lnk through the shell
        // the same way a double-click does, which is what gives
        // single-instance applications their chance to raise an existing
        // window.
        //
        // The empty "" is the window title argument. Without it, `start`
        // treats a quoted path as the title and silently does nothing,
        // which is a classic and very quiet failure.
        //
        // NOT `SetForegroundWindow`: Windows refuses it from a process that
        // does not already own the foreground (docs/windows-handoff.md),
        // so an explicit raise would fail exactly when it is wanted.
        //
        // VERIFIED to actually launch, on a real windows-2025 runner:
        // `open Notepad` took the notepad process count from 0 to 1. That
        // assertion counts PROCESSES rather than trusting the exit code,
        // because the Linux path demonstrated that a launcher can exit 0
        // having started nothing (see the gio note above).
        let status = Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&app.launch_id)
            .status()?;
        check(status, &app.name)
    }
}

/// Platforms with no desktop to launch into.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod platform {
    use super::*;

    pub fn discover() -> Vec<App> {
        Vec::new()
    }

    pub fn launch(app: &App) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("launching {} is not supported on this platform", app.name),
        ))
    }
}

/// Turn a non-zero exit into an error naming the application.
///
/// Shared by every platform: a launcher that exits non-zero has failed, and
/// the user needs to hear which application rather than a numeric code.
///
/// `pub(crate)` rather than private with an `#[allow(dead_code)]`, which is
/// what this was first written as. `scripts/ci-check.sh` has a guard for
/// exactly that shape and it fired: every caller is behind a `cfg(target_os)`
/// gate, so on a platform matching none of them the definition exists with no
/// caller, which is dead code that clippy's `-D warnings` rejects. Suppressing
/// the warning hides a real question; being honest that this is internal
/// surface for whichever backend compiles answers it.
pub(crate) fn check(status: std::process::ExitStatus, name: &str) -> io::Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "could not launch {name} (launcher exited with {status})"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discovery must never panic, on any platform, however odd the
    /// filesystem. It runs at startup, so a panic here takes the daemon
    /// down before the user has done anything.
    #[test]
    fn discovery_does_not_panic() {
        let r = discover();
        // Every discovered app must be usable: a blank name could never be
        // spoken, and a blank launch_id could not be launched.
        for app in r.apps() {
            assert!(!app.name.trim().is_empty(), "discovered a nameless app");
            assert!(
                !app.launch_id.trim().is_empty(),
                "{} has no launch id",
                app.name
            );
        }
    }

    /// On a desktop machine discovery should find *something*. Skipped
    /// rather than failed elsewhere, because an empty registry is the
    /// correct answer on a headless box and `NoRegistry` reports it.
    #[test]
    fn finds_applications_on_a_desktop() {
        let r = discover();
        if cfg!(target_os = "macos") {
            assert!(
                !r.is_empty(),
                "found no applications on macOS, where /System/Applications always has some"
            );
        } else {
            eprintln!("discovered {} applications", r.apps().len());
        }
    }
}
