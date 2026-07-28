//! Running the menu bar: load config, publish a model, perform clicks.
//!
//! Split out of [`crate::menubar`] on purpose. That module is pure (state +
//! settings in, menu model + action list out) and fully unit-tested; this
//! one is the I/O edge: it reads and writes `config.toml`, shells out to
//! `open`, runs the diagnostics, and quits the process. Keeping the two
//! apart is what lets the interesting half be tested without a filesystem.

use std::path::{Path, PathBuf};

use config::schema::Value;
use overlay::menu::{MenuId, MenuModel};

use crate::menubar::{self, Action, Settings, Status};
use crate::runtime::{Runtime, RuntimeShared};

/// Owns the configuration the menu reflects, and applies clicks to it.
pub struct MenuHost {
    settings: Settings,
    /// Problems from the last load, shown in the menu because a config that
    /// silently half-applied is the bug docs/ux/05 forbids.
    problems: Vec<String>,
    /// Cached so a rebuild does not have to re-derive it every frame.
    actions: Vec<Action>,
    model: Option<MenuModel>,
    /// Watches the config file so an edit made in a text editor shows up in
    /// the menu without a restart. docs/ux/05 promises "every setting change
    /// applies live"; a menu that only ever reflected its own writes would
    /// let the file and the UI disagree, which is exactly what the "GUI is a
    /// view over the files" design exists to prevent.
    watcher: Option<config::Watcher>,
    /// The live switches the running pipeline reads. Settings the process can
    /// adopt without a restart are pushed here on every reload, which is what
    /// makes the menu's Pause row take effect now rather than next launch.
    runtime: RuntimeShared,
}

impl MenuHost {
    /// Load configuration and build the initial state. Never fails: a
    /// daemon must never refuse to start over config (docs/ux/05), so every
    /// error becomes a problem line in the menu instead.
    pub fn new(runtime: RuntimeShared) -> MenuHost {
        let mut host = MenuHost {
            settings: Settings::default(),
            problems: Vec::new(),
            actions: Vec::new(),
            model: None,
            watcher: None,
            runtime,
        };
        host.reload();
        host
    }

    /// The hotkey the config asks for, so `main` can bind it instead of its
    /// own default. Command-line `--chord` still wins: an explicit
    /// invocation beats a file, matching the config crate's layer order.
    pub fn configured_hotkey(&self) -> &str {
        &self.settings.hotkey
    }

    /// Whether the floating overlay should be drawn at all.
    ///
    /// `overlay.position = "hidden"` is the one position value the daemon
    /// can honor today, and it is the one that matters: users who find the
    /// indicator distracting currently have no way to turn it off, so the
    /// setting existing in the schema and doing nothing is worse than not
    /// offering it. The remaining positions need layout work in the overlay
    /// crate and are deliberately not in the menu until then.
    pub fn overlay_visible(&self) -> bool {
        self.settings.overlay_position != "hidden"
    }

    /// Reload if the file changed underneath us. Called every frame by the
    /// render loop; the watcher does the work on its own thread, so this is
    /// a non-blocking channel drain.
    pub fn poll_file_changes(&mut self) {
        let changed = self
            .watcher
            .as_ref()
            .is_some_and(|w| w.events().try_recv().is_ok());
        if changed {
            self.reload();
        }
    }

    /// Re-read config from disk, collecting rather than propagating errors.
    pub fn reload(&mut self) {
        self.problems.clear();
        let user = match config::ensure_user_config() {
            Ok((path, text)) => Some((path, text)),
            Err(e) => {
                self.problems.push(format!("cannot read config: {e}"));
                None
            }
        };
        let system = std::fs::read_to_string(config::system_config_path())
            .ok()
            .map(|t| (config::system_config_path(), t));
        let env: std::collections::BTreeMap<String, String> = std::env::vars()
            .filter(|(k, _)| k.starts_with("AQUA_"))
            .collect();

        let built = config::Config::build(
            system.as_ref().map(|(p, t)| (p, t.as_str())),
            user.as_ref().map(|(p, t)| (p, t.as_str())),
            &env,
        );
        match built {
            Ok((cfg, warnings)) => {
                self.problems.extend(warnings.iter().map(|w| w.to_string()));
                // Settings the user explicitly set that no code reads yet.
                // Silently ignoring them is the file-level version of the
                // lie the menu refuses to tell by only offering wired keys:
                // someone who writes `microphone = "no-such-device"` gets a
                // daemon that records from a different device and never says
                // so. Reported to BOTH surfaces on purpose -- stderr for a
                // terminal launch, the menu for a bundled one, which has no
                // terminal to print to at all.
                for spec in cfg.inert_settings() {
                    eprintln!(
                        "hexad: config sets \"{}\" but nothing reads it yet; it has no effect",
                        spec.key
                    );
                    self.problems.push(format!(
                        "\"{}\" is set but not implemented yet; it has no effect",
                        spec.key
                    ));
                }
                self.settings = Settings::from_config(&cfg, user.map(|(p, _)| p));
            }
            Err(e) => {
                // Malformed TOML: keep the previous good settings, say so.
                self.problems.push(e.to_string());
                self.settings.config_path = user.map(|(p, _)| p);
            }
        }
        // Re-arm the watcher on the path actually read. Rebuilt rather than
        // reused because the path can change (HOME/XDG moved, or the file
        // was created by this very load).
        self.watcher = self
            .settings
            .config_path
            .clone()
            .map(|path| config::Watcher::spawn(vec![path], config::Watcher::DEFAULT_QUIET));
        // Push the settings the running process can adopt live. Everything
        // else needs a restart, and the menu says so rather than pretending.
        self.runtime.set_enabled(self.settings.enabled);
        // Force a rebuild on the next publish.
        self.model = None;
    }

    /// The menu model for the current engine state. Cheap enough to call
    /// every frame; the status item itself skips unchanged models.
    pub fn model(
        &mut self,
        state: overlay::OverlayState,
        detail: Option<String>,
        runtime: &Runtime,
    ) -> &MenuModel {
        let status = Status {
            state,
            detail,
            bound_hotkey: runtime.bound_hotkey.clone(),
            config_problems: self.problems.clone(),
            microphone: runtime.microphone.clone(),
            microphone_blocked: runtime.microphone_blocked,
            accessibility_blocked: runtime.accessibility_blocked,
        };
        let (model, actions) = menubar::build(&status, &self.settings);
        self.actions = actions;
        self.model.insert(model)
    }

    /// Perform a clicked row. Returns true when the daemon should exit.
    pub fn handle(&mut self, id: MenuId) -> bool {
        let Some(action) = self.actions.get(id.0 as usize).cloned() else {
            // Only possible if a click arrives for a menu built before a
            // reload shortened the table. Ignoring is right: acting on a
            // stale index would perform some *other* setting's write.
            return false;
        };
        match action {
            Action::Set { key, value } => {
                if let Err(e) = self.write_setting(&key, &value) {
                    eprintln!("hexad: could not save {key}: {e}");
                    self.problems.push(format!("could not save {key}: {e}"));
                }
                self.reload();
            }
            Action::OpenConfigFile => {
                if let Some(path) = self.settings.config_path.clone() {
                    // -t: the user's default *text* editor. Without it macOS
                    // may hand .toml to whatever last claimed the extension.
                    open_with(&["-t"], &path);
                }
            }
            Action::OpenVocabularyFolder => {
                if let Some(dir) = config::vocabulary_dir() {
                    // Created on demand: "open a folder that does not exist"
                    // is a dead end, and an empty folder teaches the format
                    // by being somewhere to put a file.
                    let _ = std::fs::create_dir_all(&dir);
                    open_with(&[], &dir);
                }
            }
            Action::OpenPrivacyPane { pane } => open_privacy_pane(pane),
            Action::RunDiagnostics => self.run_diagnostics(),
            Action::ReloadConfig => self.reload(),
            Action::Quit => return true,
        }
        false
    }

    /// Write one key through the config crate so comments and formatting
    /// survive, then leave the file for the watcher and the next reload.
    ///
    /// A write that would not change the effective value is skipped
    /// entirely. Without this, any spurious or duplicated click persists a
    /// key the user never chose, which turns a harmless no-op into a line in
    /// their config file (and, for `enabled`, silently disables dictation
    /// across restarts). Not writing is also the only way an unset key stays
    /// unset, which is what makes "delete the line to get the default" true.
    fn write_setting(&self, key: &str, value: &Value) -> anyhow::Result<()> {
        let path = self
            .settings
            .config_path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no writable config file"))?;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = config::update_file(&text, key, value)?;
        if updated == text {
            // Already says exactly this. Rewriting would still be correct,
            // but skipping keeps mtime stable so the watcher does not fire a
            // reload for a change that did not happen.
            return Ok(());
        }
        // Write via a temp file in the same directory, then rename: a crash
        // mid-write must never leave the user with half a config.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, updated)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Run every diagnostic check in *this* process and show the report.
    ///
    /// In-process, not by shelling out to `scripts/doctor.sh`: TCC judges the
    /// responsible process, and the whole point of asking from the menu bar
    /// is to learn what the *bundled app* can see. A script run would report
    /// on a shell instead (docs/macos-permissions.md).
    fn run_diagnostics(&self) {
        let reports = diag::run_all(&diag::Env::capture());
        let mut out = String::from("Aqua diagnostics\n\n");
        for r in &reports {
            out.push_str(&format!(
                "[{}] {:<26} {}\n",
                r.outcome.status, r.name, r.outcome.detail
            ));
            if let Some(remedy) = &r.outcome.remedy {
                out.push_str(&format!("       remedy: {remedy}\n"));
            }
        }
        // Beside the config file, not in the temp directory. A
        // LaunchServices launch gets a per-app sandboxed TMPDIR under
        // /var/folders that a user cannot find, cannot guess, and cannot
        // attach to a bug report; a diagnostics report nobody can lay hands
        // on is not a diagnostic.
        let path = match config::user_config_path().and_then(|p| p.parent().map(Path::to_path_buf))
        {
            Some(dir) => {
                let _ = std::fs::create_dir_all(&dir);
                dir.join("diagnostics.txt")
            }
            None => std::env::temp_dir().join("aqua-diagnostics.txt"),
        };
        match std::fs::write(&path, &out) {
            // A text file opened in the editor rather than an AppKit alert:
            // the report is long, copy-pasteable into an issue, and an alert
            // from an accessory app would need activation we refuse to take.
            Ok(()) => open_with(&["-t"], &path),
            Err(e) => eprintln!("hexad: could not write the diagnostics report: {e}"),
        }
    }
}

/// Deep-link into a System Settings privacy pane.
///
/// "Grant Accessibility" is useless advice on its own: the pane is four
/// levels down and users routinely grant it to the wrong app. This URL opens
/// the exact pane.
pub fn open_privacy_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    if let Err(e) = std::process::Command::new("open").arg(&url).spawn() {
        eprintln!("hexad: could not open {url}: {e}");
    }
}

/// `open`-equivalent with flags. Spawned, never waited on: the menu must not
/// block the main thread on a launching application. The launcher differs
/// per platform, but every host in this crate goes through here so the
/// difference exists exactly once.
fn open_with(flags: &[&str], path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.args(flags);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // `start` is a cmd builtin, so it needs a shell; the empty "" is the
        // window title argument start insists on before the path.
        let _ = flags;
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = {
        let _ = flags;
        std::process::Command::new("xdg-open")
    };

    cmd.arg(path);
    if let Err(e) = cmd.spawn() {
        eprintln!("hexad: could not open {}: {e}", path.display());
    }
}

/// The path the menu says it edits, for tests and for `--help` text.
pub fn config_path_for_display() -> Option<PathBuf> {
    config::user_config_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stale click (menu built, config reloaded, table shrank) must be
    /// inert rather than firing whatever now sits at that index.
    #[test]
    fn out_of_range_clicks_are_ignored() {
        let mut host = MenuHost {
            settings: Settings::default(),
            problems: Vec::new(),
            actions: Vec::new(),
            model: None,
            watcher: None,
            runtime: RuntimeShared::new(),
        };
        assert!(!host.handle(MenuId(999)));
    }

    /// Nothing but a click may write the config file.
    ///
    /// Reported twice during beta prep as "the daemon appended
    /// `enabled = false` on its own". Both sightings turned out to be real
    /// clicks (menu-driven verification runs), but the failure it describes
    /// is severe enough to pin down: `enabled` is the master switch, so a
    /// spurious write would leave a daemon that starts, shows its icon,
    /// reports idle, and never responds to the hotkey, across restarts, with
    /// no visible cause. This proves the passive paths -- construction,
    /// reload, watcher polling, model rebuilds -- leave the file untouched.
    #[test]
    fn nothing_but_a_click_writes_the_config() {
        // Deliberately does NOT touch XDG_CONFIG_HOME. An earlier version
        // did, and raced a config-crate test that sets the same variable:
        // the two crates each guarded it with their own mutex, which cannot
        // serialize anything because the variable is process-global and the
        // mutexes are not. Driving the write path directly needs no
        // environment at all, and tests that fight over global state fail on
        // whichever machine happens to interleave them.
        let dir = std::env::temp_dir().join(format!("hexa-nowrite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# mine\nhotkey = \"f13\"\n").unwrap();

        let mut host = MenuHost {
            settings: Settings {
                config_path: Some(path.clone()),
                ..Settings::default()
            },
            problems: Vec::new(),
            actions: Vec::new(),
            model: None,
            watcher: None,
            runtime: RuntimeShared::new(),
        };
        let before = std::fs::read(&path).unwrap();

        // Every passive path a running daemon takes between clicks: building
        // a model, polling the watcher, and reacting to a device change.
        for _ in 0..5 {
            host.poll_file_changes();
            let _ = host.model(
                overlay::OverlayState::Idle,
                Some("input device changed; rebuilding stream".into()),
                &Runtime {
                    bound_hotkey: Some("right-option".into()),
                    microphone: Some("Built-in".into()),
                    ..Runtime::default()
                },
            );
        }

        assert_eq!(
            before,
            std::fs::read(&path).unwrap(),
            "the daemon rewrote config.toml without a click"
        );

        // And a click that changes nothing must also leave the bytes alone,
        // which is the guard that stops a stray activation persisting a key
        // the user never chose.
        host.write_setting("hotkey", &Value::Str("f13".into()))
            .unwrap();
        assert_eq!(
            before,
            std::fs::read(&path).unwrap(),
            "a no-op write must not rewrite the file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
