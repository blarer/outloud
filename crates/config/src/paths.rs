//! Where the configuration files live on this machine.
//!
//! Path policy is here, next to the file format, rather than in each host,
//! because "which file am I editing?" must have exactly one answer: the
//! daemon, `hexa set`, the menu-bar Settings items, and the docs all have to
//! name the same path or the user's edit lands somewhere nothing reads.
//!
//! I/O still stays at the edges: these functions compute paths and never
//! touch the filesystem except [`ensure_user_config`], which is explicitly a
//! create-if-missing helper.

use std::path::PathBuf;

/// The product's directory name under the config root. One constant, so the
/// daemon, the docs, and the relocation logic cannot disagree about where
/// settings live.
pub const APP_DIR: &str = "hexavoice";

/// The user's config file: `$XDG_CONFIG_HOME/hexavoice/config.toml`, else
/// `~/.config/hexavoice/config.toml`.
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
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(dir.join(APP_DIR).join("config.toml"))
}

/// The machine-wide file for managed deployments. Read-only as far as the
/// daemon is concerned; it never writes here.
pub fn system_config_path() -> PathBuf {
    PathBuf::from("/etc/hexavoice/config.toml")
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
        Ok(outcome) => eprintln!("hexad: {outcome}"),
        Err(e) => eprintln!("hexad: could not check for older settings: {e}"),
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok((path, text)),
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

/// The skeleton written on first run: every value commented out, so the file
/// documents itself and deleting a line genuinely means "use the default"
/// rather than "hope the default matches what was written here".
fn starter_file() -> String {
    let mut out = String::from(
        "# Hexavoice configuration. Every setting below is shown at its built-in\n\
         # default and commented out; uncomment a line to change it.\n\
         #\n\
         # Edits apply live (no restart) except the hotkey, which binds at\n\
         # launch. Full reference: docs/configuration.md\n\n",
    );
    out.push_str(&format!(
        "schema-version = {}\n\n",
        crate::schema::SCHEMA_VERSION
    ));
    for spec in crate::schema::schema() {
        out.push_str(&format!(
            "# {}\n# {} = {}\n\n",
            spec.doc, spec.key, spec.default
        ));
    }
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
            PathBuf::from("/cfg/hexavoice/config.toml")
        );

        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(
            user_config_path().unwrap(),
            PathBuf::from("/home/u/.config/hexavoice/config.toml")
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
            PathBuf::from("/cfg/hexavoice/vocabulary")
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
