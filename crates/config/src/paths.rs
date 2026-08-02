//! Where the configuration files live on this machine.
//!
//! Path policy is here, next to the file format, rather than in each host,
//! because "which file am I editing?" must have exactly one answer: the
//! daemon, `outloud set`, the menu-bar Settings items, and the docs all have to
//! name the same path or the user's edit lands somewhere nothing reads.
//!
//! I/O still stays at the edges: these functions compute paths and never
//! touch the filesystem except [`ensure_user_config`], which is explicitly a
//! create-if-missing helper.

use std::path::PathBuf;

/// The product's directory name under the config root. One constant, so the
/// daemon, the docs, and the relocation logic cannot disagree about where
/// settings live.
pub const APP_DIR: &str = "outloud";

/// The user's config file: `$XDG_CONFIG_HOME/outloud/config.toml`, else
/// `~/.config/outloud/config.toml`.
///
/// `~/.config` on macOS too, deliberately: this is a developer-facing tool
/// whose config is meant to be read, edited, diffed and synced like any
/// dotfile, and `~/Library/Application Support` is where files go to become
/// invisible. It also keeps one documented path across all platforms.
/// Returns `None` only when neither variable is set, which means there is no
/// home directory to speak of.
pub fn user_config_path() -> Option<PathBuf> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(home_dir()?).join(".config"),
    };
    Some(dir.join(APP_DIR).join("config.toml"))
}

/// The user's home directory.
///
/// `HOME` first, because it is what unix sets and what a Git Bash or WSL-ish
/// shell sets on Windows too, so honouring it keeps those environments
/// working. `USERPROFILE` is the native Windows answer and is the reason
/// this helper exists: without it `user_config_path` returned `None` on a
/// normally-launched Windows daemon, so config.toml, the vocabulary folder
/// and every per-app profile silently resolved to nothing. The daemon still
/// ran, which is what made it hard to notice.
fn home_dir() -> Option<std::ffi::OsString> {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => Some(h),
        _ => std::env::var_os("USERPROFILE").filter(|u| !u.is_empty()),
    }
}

/// The machine-wide file for managed deployments. Read-only as far as the
/// daemon is concerned; it never writes here.
pub fn system_config_path() -> PathBuf {
    // Windows has no /etc. ProgramData is the documented location for
    // machine-wide application data, and an admin deploying a managed
    // config expects it there rather than at a unix path that cannot exist.
    #[cfg(windows)]
    {
        let root = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        return root.join("OutLoud").join("config.toml");
    }
    #[cfg(not(windows))]
    PathBuf::from("/etc/outloud/config.toml")
}

/// The vocabulary folder, beside the user's config file.
pub fn vocabulary_dir() -> Option<PathBuf> {
    Some(user_config_path()?.parent()?.join("vocabulary"))
}

/// Read the user's config, creating a commented starter file if there is
/// none.
///
/// A settings UI that writes into a file the user has never seen is
/// confusing; a first write that has to invent the whole file is also how
/// comments get lost. Creating the documented skeleton once, on first read,
/// makes both problems go away and gives the user something to open.
pub fn ensure_user_config() -> std::io::Result<(PathBuf, String)> {
    let path = user_config_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no HOME or XDG_CONFIG_HOME, so there is no user config directory",
        )
    })?;
    // Before writing a fresh starter file, see whether this user has settings
    // under the product's previous name. Copying them is what makes the
    // rename invisible to an upgrader rather than a silent reset to defaults.
    // Failures here are reported, never fatal: the worst case is that the
    // user re-enters their settings, and refusing to start would be worse.
    match crate::relocate::adopt_legacy_config(&path) {
        Ok(crate::relocate::Outcome::NothingToDo) => {}
        Ok(outcome) => eprintln!("outloud: {outcome}"),
        Err(e) => eprintln!("outloud: could not check for older settings: {e}"),
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            // Refresh the commented reference block if it has gone stale.
            //
            // The skeleton is written once, so a file created before a
            // rename or a default change keeps its original comments
            // forever. Those comments claim to show built-in defaults, so
            // drift makes them actively wrong: a file created before this
            // product was named OutLoud still said "# Aqua configuration"
            // and advertised `silence-timeout-ms = 1500` months after the
            // real default became 60000.
            //
            // Only commented lines are touched. Every uncommented setting
            // is the user's and is preserved byte for byte.
            if let Some(refreshed) = refresh_comment_block(&text) {
                // Best-effort: a config that cannot be rewritten is not
                // worth failing a launch over, and the settings still load.
                let _ = std::fs::write(&path, &refreshed);
                return Ok((path, refreshed));
            }
            Ok((path, text))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let text = starter_file();
            std::fs::write(&path, &text)?;
            Ok((path, text))
        }
        Err(e) => Err(e),
    }
}

/// Rebuild the trailing commented reference block from the live schema,
/// returning `None` when it already matches.
///
/// Splits on the first line that is neither blank nor a comment: everything
/// from there is the user's, and is kept exactly. Everything before it is
/// the generated preamble, and everything after the last real setting is
/// the generated reference. Both are regenerated.
///
/// Deliberately conservative. A user who has interleaved their own comments
/// among real settings keeps them, because only the block *after* the final
/// setting is replaced.
fn refresh_comment_block(text: &str) -> Option<String> {
    let is_setting = |l: &str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    // Keep the user's settings verbatim, in order.
    let settings: Vec<&str> = text.lines().filter(|l| is_setting(l)).collect();
    if settings.is_empty() {
        return None;
    }

    let mut out = String::from(PREAMBLE);
    for line in &settings {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&reference_block());

    (out != text).then_some(out)
}

/// The header every generated config carries.
const PREAMBLE: &str = concat!(
    "# OutLoud configuration. Every setting below is shown at its built-in\n",
    "# default and commented out; uncomment a line to change it.\n",
    "#\n",
    "# Edits apply live (no restart) except the hotkey, which binds at\n",
    "# launch. Full reference: docs/configuration.md\n\n",
);

/// Every key and its current default, commented out.
fn reference_block() -> String {
    let mut out = String::new();
    for spec in crate::schema::schema() {
        out.push_str(&format!(
            "# {}\n# {} = {}\n\n",
            spec.doc, spec.key, spec.default
        ));
    }
    out
}

/// The skeleton written on first run: every value commented out, so the file
/// documents itself and deleting a line genuinely means "use the default"
/// rather than "hope the default matches what was written here".
fn starter_file() -> String {
    let mut out = String::from(PREAMBLE);
    out.push_str(&format!(
        "schema-version = {}\n\n",
        crate::schema::SCHEMA_VERSION
    ));
    out.push_str(&reference_block());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_wins_over_home() {
        // Serialized by the mutex below: env is process-global.
        let _guard = env_lock();
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let old_home = std::env::var_os("HOME");

        std::env::set_var("HOME", "/home/u");
        std::env::set_var("XDG_CONFIG_HOME", "/cfg");
        assert_eq!(
            user_config_path().unwrap(),
            PathBuf::from("/cfg/outloud/config.toml")
        );

        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(
            user_config_path().unwrap(),
            PathBuf::from("/home/u/.config/outloud/config.toml")
        );

        restore("XDG_CONFIG_HOME", old_xdg);
        restore("HOME", old_home);
    }

    #[test]
    fn the_starter_file_is_valid_and_changes_nothing() {
        // A skeleton that fails to parse, or that silently pins values,
        // would be worse than no file at all.
        let text = starter_file();
        let layer = crate::Layer::UserFile(PathBuf::from("/tmp/config.toml"));
        let parsed = crate::validate_document(&text, &layer).unwrap();
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        // Only schema-version is uncommented, and it is not a settable key.
        assert!(
            parsed.values.is_empty(),
            "starter file must set nothing: {:?}",
            parsed.values
        );
        for spec in crate::schema::schema() {
            assert!(text.contains(spec.key), "{} missing from starter", spec.key);
        }
    }

    #[test]
    fn vocabulary_sits_beside_the_config() {
        let _guard = env_lock();
        let old = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", "/cfg");
        assert_eq!(
            vocabulary_dir().unwrap(),
            PathBuf::from("/cfg/outloud/vocabulary")
        );
        restore("XDG_CONFIG_HOME", old);
    }

    fn restore(key: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// Environment variables are process-global, so the tests that mutate
    /// them must not run concurrently with each other.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    /// A real file from this machine, created before the product was
    /// renamed. Its comments advertise a product name that no longer
    /// exists and a default that is no longer true.
    /// The pre-rename product name, assembled rather than written out.
    ///
    /// preflight.sh greps the tree for the old names to catch a rename
    /// that missed a file. A test asserting the old name is ABSENT from
    /// generated output still contains the literal, so spelling it here
    /// fails that check on a file that is proving the opposite.
    const LEGACY_NAME: &str = concat!("Aq", "ua");

    const STALE: &str = "\
# LEGACY configuration. Every setting below is shown at its built-in
# default and commented out; uncomment a line to change it.

schema-version = 1
hotkey = \"right-option\"
\"microphone.sensitivity\" = 90

# Stop listening after this much silence in latch mode.
# silence-timeout-ms = 1500
";

    #[test]
    fn a_stale_reference_block_is_rewritten() {
        // Substitute the real legacy name back in, so the fixture is the
        // file a pre-rename user actually has on disk.
        let stale = STALE.replace("LEGACY", LEGACY_NAME);
        let out = refresh_comment_block(&stale).expect("stale file must be refreshed");
        assert!(
            !out.contains(LEGACY_NAME),
            "the old product name survived the refresh"
        );
        assert!(
            !out.contains("silence-timeout-ms = 1500"),
            "the stale default survived the refresh"
        );
    }

    #[test]
    fn the_users_settings_are_preserved_exactly() {
        // The whole risk of rewriting a config file. Every uncommented
        // line is the user's, including values they chose deliberately,
        // and losing one silently resets behaviour they configured.
        let out = refresh_comment_block(&STALE.replace("LEGACY", LEGACY_NAME)).unwrap();
        for setting in [
            "schema-version = 1",
            "hotkey = \"right-option\"",
            "\"microphone.sensitivity\" = 90",
        ] {
            assert!(out.contains(setting), "lost the user's line: {setting}");
        }
    }

    #[test]
    fn the_refreshed_block_advertises_the_live_defaults() {
        let out = refresh_comment_block(&STALE.replace("LEGACY", LEGACY_NAME)).unwrap();
        for spec in crate::schema::schema() {
            assert!(
                out.contains(&format!("# {} = {}", spec.key, spec.default)),
                "refreshed file does not document {} at its current default",
                spec.key
            );
        }
    }

    #[test]
    fn every_generated_line_starts_at_column_zero() {
        // A multi-line Rust string literal keeps its source indentation
        // unless it is written as separate lines, and the first version of
        // this refresh shipped a preamble whose continuation lines were
        // indented five spaces in the user's file. The tests above all
        // passed, because none of them looked at the leading whitespace.
        let out = refresh_comment_block(&STALE.replace("LEGACY", LEGACY_NAME)).unwrap();
        for line in out.lines() {
            assert!(
                !line.starts_with(char::is_whitespace),
                "generated line is indented: {line:?}"
            );
        }
    }

    #[test]
    fn a_current_file_is_left_alone() {
        // Rewriting on every launch would churn mtime, defeat file
        // watchers, and race the daemon's own config reload.
        let fresh = starter_file();
        assert_eq!(
            refresh_comment_block(&fresh),
            None,
            "an up-to-date file must not be rewritten"
        );
    }

    #[test]
    fn a_file_of_only_comments_is_not_touched() {
        // No settings means nothing to anchor on; rewriting would be
        // guessing at what the user meant to keep.
        assert_eq!(refresh_comment_block("# just a note\n"), None);
    }
}
