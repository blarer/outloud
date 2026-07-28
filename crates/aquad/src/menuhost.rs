//! Running the menu bar: load config, publish a model, perform clicks.
//!
//! Split out of [`crate::menubar`] on purpose. That module is pure (state +
//! settings in, menu model + action list out) and fully unit-tested; this
//! one is the I/O edge: it reads and writes `config.toml`, shells out to
//! `open`, runs the diagnostics, and quits the process. Keeping the two
//! apart is what lets the interesting half be tested without a filesystem.

use std::path::PathBuf;

use config::schema::Value;
use overlay::menu::{MenuId, MenuModel};

use crate::menubar::{self, Action, Settings, Status};

/// Owns the configuration the menu reflects, and applies clicks to it.
pub struct MenuHost {
    settings: Settings,
    /// Problems from the last load, shown in the menu because a config that
    /// silently half-applied is the bug docs/ux/05 forbids.
    problems: Vec<String>,
    /// Cached so a rebuild does not have to re-derive it every frame.
    actions: Vec<Action>,
    model: Option<MenuModel>,
}

impl MenuHost {
    /// Load configuration and build the initial state. Never fails: a
    /// daemon must never refuse to start over config (docs/ux/05), so every
    /// error becomes a problem line in the menu instead.
    pub fn new() -> MenuHost {
        let mut host = MenuHost {
            settings: Settings {
                hotkey: "right-option".into(),
                model: "balanced".into(),
                insertion_mode: "on-release".into(),
                casing: "standard".into(),
                overlay_position: "bottom-center".into(),
                enabled: true,
                launch_at_login: false,
                config_path: None,
            },
            problems: Vec::new(),
            actions: Vec::new(),
            model: None,
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
                self.settings = Settings::from_config(&cfg, user.map(|(p, _)| p));
            }
            Err(e) => {
                // Malformed TOML: keep the previous good settings, say so.
                self.problems.push(e.to_string());
                self.settings.config_path = user.map(|(p, _)| p);
            }
        }
        // Force a rebuild on the next publish.
        self.model = None;
    }

    /// The menu model for the current engine state. Cheap enough to call
    /// every frame; the status item itself skips unchanged models.
    pub fn model(
        &mut self,
        state: overlay::OverlayState,
        detail: Option<String>,
        bound_hotkey: Option<String>,
    ) -> &MenuModel {
        let status = Status {
            state,
            detail,
            bound_hotkey,
            config_problems: self.problems.clone(),
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
                    eprintln!("aquad: could not save {key}: {e}");
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
    fn write_setting(&self, key: &str, value: &Value) -> anyhow::Result<()> {
        let path = self
            .settings
            .config_path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no writable config file"))?;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = config::update_file(&text, key, value)?;
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
        let path = std::env::temp_dir().join("aqua-diagnostics.txt");
        match std::fs::write(&path, &out) {
            // A text file opened in the editor rather than an AppKit alert:
            // the report is long, copy-pasteable into an issue, and an alert
            // from an accessory app would need activation we refuse to take.
            Ok(()) => open_with(&["-t"], &path),
            Err(e) => eprintln!("aquad: could not write the diagnostics report: {e}"),
        }
    }
}

impl Default for MenuHost {
    fn default() -> Self {
        Self::new()
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
        eprintln!("aquad: could not open {url}: {e}");
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
        eprintln!("aquad: could not open {}: {e}", path.display());
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
            settings: Settings {
                hotkey: "fn".into(),
                model: "fast".into(),
                insertion_mode: "stream".into(),
                casing: "standard".into(),
                overlay_position: "hidden".into(),
                enabled: true,
                launch_at_login: false,
                config_path: None,
            },
            problems: Vec::new(),
            actions: Vec::new(),
            model: None,
        };
        assert!(!host.handle(MenuId(999)));
    }
}
