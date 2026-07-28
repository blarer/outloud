//! Moving the config directory between product names, without losing
//! anyone's settings.
//!
//! Separate from [`crate::migrate`] on purpose: that module migrates the
//! *schema* of a file, this one migrates its *location*. They are different
//! axes, they fail differently, and conflating them would make both harder
//! to test.
//!
//! The rules, and why each one exists:
//!
//! 1. **Copy, never move.** A move that is interrupted, or that turns out to
//!    have been the wrong call, leaves the user with no config at all. A copy
//!    leaves the original exactly where it was, so the worst case is a stale
//!    duplicate rather than lost settings.
//! 2. **Only when the new location is absent.** Otherwise a user who has
//!    already configured the new path would have it overwritten by a stale
//!    file they forgot about.
//! 3. **Only when the old file parses.** Promoting a corrupt file into the
//!    new location converts a recoverable mess ("my old config is broken")
//!    into a mysterious one ("my new config was broken on arrival").
//! 4. **Say it happened.** A silent copy means a user edits one file and
//!    watches the other take effect.

use std::path::{Path, PathBuf};

use crate::layers::Layer;
use crate::validate::validate_document;

/// The previous product's config directory name, kept so an upgrader's
/// settings survive the rename. Frozen: this is history, not configuration.
/// Every directory name this product has used, newest first.
///
/// A list rather than a single name because the product has been renamed
/// twice (aqua -> hexavoice -> outloud) and a user may be sitting on either
/// older generation. Checking newest-first means someone who upgraded once
/// already gets their most recent settings rather than their oldest.
///
/// Frozen history: entries are only ever appended to, never edited.
const LEGACY_DIRS: &[&str] = &["hexavoice", "aqua"];

/// What a location migration did, so the caller can tell the user.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing to do: the new path already exists, or there was no old one.
    NothingToDo,
    /// The old config was copied to the new path. Both paths are reported
    /// because the user needs to know the old file is still there.
    Copied { from: PathBuf, to: PathBuf },
    /// An old config exists but was left alone because it does not parse.
    /// Reported rather than silently skipped: the user is about to get
    /// defaults and deserves to know why.
    SkippedUnparsable { from: PathBuf, message: String },
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::NothingToDo => Ok(()),
            Outcome::Copied { from, to } => write!(
                f,
                "copied your settings from {} to {}; the original is untouched \
                 and can be deleted once you are happy",
                from.display(),
                to.display()
            ),
            Outcome::SkippedUnparsable { from, message } => write!(
                f,
                "found an old config at {} but it does not parse ({message}), \
                 so it was left alone and defaults are in use",
                from.display()
            ),
        }
    }
}

/// The legacy config path that sits beside `new_path`, if one can exist.
fn legacy_paths_for(new_path: &Path) -> Vec<PathBuf> {
    // .../<current>/config.toml -> .../<each older name>/config.toml
    let Some(dir) = new_path.parent() else {
        return Vec::new();
    };
    let Some(file) = new_path.file_name() else {
        return Vec::new();
    };
    let Some(root) = dir.parent() else {
        return Vec::new();
    };
    LEGACY_DIRS
        .iter()
        .map(|name| root.join(name).join(file))
        .collect()
}

/// Copy a previous-generation config into the current location if that is
/// the right thing to do. Pure decision-making lives in
/// [`decide`]; this is the thin I/O wrapper.
pub fn adopt_legacy_config(new_path: &Path) -> std::io::Result<Outcome> {
    if new_path.exists() {
        return Ok(Outcome::NothingToDo);
    }
    // Newest generation first: someone who already upgraded once should get
    // the settings they last edited, not the ones they abandoned.
    let Some((old, old_text)) = legacy_paths_for(new_path)
        .into_iter()
        .find_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t)))
    else {
        return Ok(Outcome::NothingToDo);
    };
    let new_exists = false;

    match decide(new_exists, Some(old_text.as_str()), &old, new_path) {
        Outcome::Copied { from, to } => {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // The text we validated, not a re-read: re-reading would open a
            // window where the file changes between check and copy.
            std::fs::write(&to, &old_text)?;
            Ok(Outcome::Copied { from, to })
        }
        other => Ok(other),
    }
}

/// The whole policy, as a pure function of what is on disk. Split out so
/// every branch is testable without a filesystem.
fn decide(new_exists: bool, old_text: Option<&str>, old: &Path, new: &Path) -> Outcome {
    if new_exists {
        return Outcome::NothingToDo;
    }
    let Some(text) = old_text else {
        return Outcome::NothingToDo;
    };
    // Rule 3: a file that does not parse must not be promoted.
    match validate_document(text, &Layer::UserFile(old.to_path_buf())) {
        Ok(_) => Outcome::Copied {
            from: old.to_path_buf(),
            to: new.to_path_buf(),
        },
        Err(e) => Outcome::SkippedUnparsable {
            from: old.to_path_buf(),
            message: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_path() -> PathBuf {
        PathBuf::from("/home/u/.config/outloud/config.toml")
    }
    /// The legacy list is FROZEN HISTORY, and a rename script cannot be
    /// trusted to know that: the aqua -> hexavoice -> outloud rename rewrote
    /// "hexavoice" to "outloud" inside these very fixtures, leaving tests that
    /// asserted outloud migrates to outloud. They passed, and migration from
    /// the real previous generation was silently untested.
    ///
    /// This asserts the names literally so the next blanket replace fails
    /// here instead of shipping. If you are renaming the product and this
    /// test fails, APPEND the previous directory name to LEGACY_DIRS and to
    /// the list below; do not edit the existing entries.
    #[test]
    fn legacy_generations_are_frozen_history() {
        assert_eq!(
            LEGACY_DIRS,
            ["hexavoice", "aqua"],
            "legacy directory names are history and may only be appended to; \
             a rename must not rewrite them"
        );
        assert!(
            !LEGACY_DIRS.contains(&"outloud"),
            "the CURRENT directory must never appear in the legacy list, or \
             migration would search the place it is migrating to"
        );
    }

    fn old_path() -> PathBuf {
        // The PREVIOUS product's directory. Deliberately spelled out rather
        // than derived from LEGACY_DIR: this fixture is the thing that
        // catches a careless rename sweeping the legacy name away, which is
        // exactly what happened once already.
        PathBuf::from("/home/u/.config/aqua/config.toml")
    }

    #[test]
    fn every_previous_generation_is_searched_newest_first() {
        // Two renames means two older locations. Order matters: a user who
        // already upgraded once must get the settings they last edited.
        let paths = legacy_paths_for(&new_path());
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/u/.config/hexavoice/config.toml"),
                PathBuf::from("/home/u/.config/aqua/config.toml"),
            ]
        );
    }

    #[test]
    fn the_newest_previous_generation_wins() {
        // Both older directories exist. The one the user most recently used
        // is the one whose settings they expect to survive.
        let root = std::env::temp_dir().join(format!("outloud-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, body) in [
            ("aqua", "hotkey = \"f13\"\n"),
            ("hexavoice", "hotkey = \"fn\"\n"),
        ] {
            let p = root.join(dir);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("config.toml"), body).unwrap();
        }
        let new = root.join("outloud").join("config.toml");
        let outcome = adopt_legacy_config(&new).unwrap();
        match outcome {
            Outcome::Copied { from, .. } => {
                assert!(from.ends_with("hexavoice/config.toml"), "{from:?}");
            }
            other => panic!("expected a copy from the newest generation, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "hotkey = \"fn\"\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The four cases ant asked for, as one table so a fifth cannot be
    /// forgotten: old-only, new-only, both, neither.
    #[test]
    fn neither_present_does_nothing() {
        assert_eq!(
            decide(false, None, &old_path(), &new_path()),
            Outcome::NothingToDo
        );
    }

    #[test]
    fn old_only_is_copied() {
        let outcome = decide(false, Some("hotkey = \"f13\"\n"), &old_path(), &new_path());
        assert_eq!(
            outcome,
            Outcome::Copied {
                from: old_path(),
                to: new_path()
            }
        );
    }

    #[test]
    fn new_only_is_left_alone() {
        assert_eq!(
            decide(true, None, &old_path(), &new_path()),
            Outcome::NothingToDo
        );
    }

    #[test]
    fn both_present_never_overwrites_the_new_one() {
        // The user has already configured the new location. A stale old file
        // must not clobber it.
        assert_eq!(
            decide(true, Some("hotkey = \"fn\"\n"), &old_path(), &new_path()),
            Outcome::NothingToDo
        );
    }

    #[test]
    fn a_corrupt_old_config_is_reported_not_promoted() {
        // Promoting it would turn "my old config is broken" into "my new
        // config was broken the moment it appeared".
        let outcome = decide(false, Some("hotkey = \n"), &old_path(), &new_path());
        match outcome {
            Outcome::SkippedUnparsable { from, message } => {
                assert_eq!(from, old_path());
                assert!(!message.is_empty());
            }
            other => panic!("expected the corrupt file to be skipped, got {other:?}"),
        }
    }

    #[test]
    fn an_old_config_with_a_bad_key_still_migrates() {
        // Unknown keys are warnings, not syntax errors: the file is valid
        // TOML and the user's other settings are real. Refusing to migrate
        // over a typo would strand a working config.
        let outcome = decide(
            false,
            Some("hotkye = \"fn\"\nhotkey = \"f13\"\n"),
            &old_path(),
            &new_path(),
        );
        assert!(matches!(outcome, Outcome::Copied { .. }), "{outcome:?}");
    }

    #[test]
    fn the_copy_message_names_both_paths_and_says_the_original_is_safe() {
        // A user who is told a file moved will go looking for the old one.
        let msg = Outcome::Copied {
            from: old_path(),
            to: new_path(),
        }
        .to_string();
        assert!(msg.contains("aqua/config.toml"), "{msg}");
        assert!(msg.contains("outloud/config.toml"), "{msg}");
        assert!(msg.contains("untouched"), "{msg}");
    }

    #[test]
    fn end_to_end_copy_leaves_the_original_in_place() {
        let root = std::env::temp_dir().join(format!("outloud-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("aqua").join("config.toml");
        let new = root.join("outloud").join("config.toml");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "# mine\nhotkey = \"f13\"\n").unwrap();

        let outcome = adopt_legacy_config(&new).unwrap();
        assert!(matches!(outcome, Outcome::Copied { .. }), "{outcome:?}");
        // Copy, not move: rule 1.
        assert!(old.exists(), "the original must survive");
        // Comments survive because the bytes are copied verbatim.
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "# mine\nhotkey = \"f13\"\n"
        );

        // Idempotent: running again must not re-copy or error.
        assert_eq!(adopt_legacy_config(&new).unwrap(), Outcome::NothingToDo);
        let _ = std::fs::remove_dir_all(&root);
    }
}
