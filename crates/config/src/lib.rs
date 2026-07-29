//! Layered configuration for the dictation daemon.
//!
//! Everything docs/ux/05 promises about settings lives here: one
//! human-readable `config.toml`, per-application profiles, an unlimited
//! plain-text vocabulary, live reload, and errors a person can act on.
//!
//! Layer precedence, lowest to highest:
//!
//! 1. built-in defaults ([`schema`])
//! 2. system file (managed deployments)
//! 3. user file
//! 4. matched per-app profile
//! 5. `AQUA_*` environment variables
//!
//! Every resolved value reports which layer set it ([`Provenance`]), because
//! "why is my hotkey not what I set" must be answerable from `hexa status`
//! rather than by bisecting files.
//!
//! I/O lives at the edges ([`watch`], [`update_file`]); parsing, merging,
//! matching, correction, validation, and migration are pure functions of
//! their inputs, which is what makes this crate near-100% testable in CI.

pub mod fuzzy;
pub mod layers;
pub mod migrate;
pub mod paths;
pub mod profile;
pub mod relocate;
pub mod schema;
pub mod validate;
pub mod vocab;
pub mod watch;

pub use layers::{Config, Layer, Provenance, ENV_PREFIX, LEGACY_ENV_PREFIX};
pub use migrate::{migrate, Migration};
pub use paths::{
    ensure_user_config, system_config_path, user_config_path, vocabulary_dir, APP_DIR,
};
pub use profile::{AppIdentity, Matcher, Profile, WinReason};
pub use schema::{schema, Value, SCHEMA_VERSION};
pub use validate::{validate_document, ConfigError};
pub use vocab::{Correction, Vocabulary};
pub use watch::{Debouncer, Reload, Watcher};

use toml_edit::DocumentMut;

/// Set one key in a config file's text, preserving the user's comments and
/// formatting. This is the primitive the GUI editor and `hexa set` share:
/// both are views over the same file, so both must be comment-safe
/// (docs/ux/05: "the GUI is a convenience view over the files").
///
/// Dotted keys write into their table form (`[insertion] mode = ...`) when
/// the table already exists, and dotted form otherwise, following whichever
/// spelling the user's file already uses.
pub fn update_file(text: &str, key: &str, value: &Value) -> Result<String, ConfigError> {
    let no_layer = Layer::UserFile(std::path::PathBuf::from("<in-memory>"));
    let Some(spec) = schema::spec_for(key) else {
        return Err(ConfigError::UnknownKey {
            key: key.to_string(),
            layer: no_layer,
            suggestion: fuzzy::closest(key, schema::key_names()).map(String::from),
        });
    };
    if value.type_name() != spec.default.type_name() {
        return Err(ConfigError::WrongType {
            key: key.to_string(),
            layer: no_layer,
            expected: spec.default.type_name(),
            got: value.type_name().to_string(),
        });
    }
    if let Err(message) = spec.constraint.check(value) {
        return Err(ConfigError::InvalidValue {
            key: key.to_string(),
            layer: no_layer,
            message,
        });
    }

    let mut doc: DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Syntax {
                layer: no_layer,
                message: e.to_string(),
            })?;

    let item = match value {
        Value::Str(s) => toml_edit::value(s.as_str()),
        Value::Bool(b) => toml_edit::value(*b),
        Value::Int(i) => toml_edit::value(*i),
        Value::List(items) => {
            let mut arr = toml_edit::Array::new();
            for s in items {
                arr.push(s.as_str());
            }
            toml_edit::value(arr)
        }
    };

    match key.split_once('.') {
        // Reuse an existing [section] table if the user wrote one, so the
        // rewrite lands where they expect to find it; otherwise write the
        // dotted form at top level, which is the least-surprising insert.
        Some((section, rest)) if doc.get(section).is_some_and(|i| i.is_table()) => {
            set_preserving_decor(&mut doc[section][rest], item);
        }
        _ => {
            set_preserving_decor(&mut doc[key], item);
        }
    }
    Ok(doc.to_string())
}

/// Assign a new value to an item, carrying over the old value's decoration
/// (whitespace and, crucially, any trailing `# inline comment`). A plain
/// `doc[key] = value` discards that comment, which breaks the "comments
/// preserved on rewrite" promise for exactly the lines users annotate.
fn set_preserving_decor(slot: &mut toml_edit::Item, mut item: toml_edit::Item) {
    if let (Some(old), Some(new)) = (slot.as_value(), item.as_value_mut()) {
        *new.decor_mut() = old.decor().clone();
    }
    *slot = item;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_preserves_comments_and_layout() {
        let original = "# my precious comment\nhotkey = \"fn\" # inline note\n\nmodel = \"fast\"\n";
        let updated = update_file(original, "hotkey", &Value::Str("f13".into())).unwrap();
        assert!(updated.contains("# my precious comment"), "{updated}");
        assert!(updated.contains("# inline note"), "{updated}");
        assert!(updated.contains("hotkey = \"f13\""), "{updated}");
        assert!(updated.contains("model = \"fast\""), "{updated}");
    }

    #[test]
    fn update_validates_before_writing() {
        let err = update_file("", "hotkey", &Value::Str("cmd+shift".into())).unwrap_err();
        assert!(err.to_string().contains("not a valid hotkey"));

        let err = update_file("", "hotkye", &Value::Str("fn".into())).unwrap_err();
        assert!(err.to_string().contains("did you mean \"hotkey\""));

        let err = update_file("", "hotkey", &Value::Int(13)).unwrap_err();
        assert!(err.to_string().contains("expects a string"));
    }

    #[test]
    fn update_uses_existing_table_spelling() {
        let original = "[insertion]\nmode = \"on-release\"\n";
        let updated =
            update_file(original, "insertion.mode", &Value::Str("stream".into())).unwrap();
        // Stays in the table the user wrote, not duplicated as a dotted key.
        assert!(updated.contains("[insertion]"), "{updated}");
        assert!(updated.contains("mode = \"stream\""), "{updated}");
        assert!(!updated.contains("insertion.mode"), "{updated}");
    }

    #[test]
    fn update_writes_dotted_form_when_no_table_exists() {
        let updated = update_file("", "insertion.mode", &Value::Str("stream".into())).unwrap();
        let reparsed = validate_document(
            &updated,
            &Layer::UserFile(std::path::PathBuf::from("/tmp/c.toml")),
        )
        .unwrap();
        assert_eq!(
            reparsed.values.get("insertion.mode"),
            Some(&Value::Str("stream".into()))
        );
    }

    #[test]
    fn round_trip_update_then_load() {
        let text = update_file("", "hotkey", &Value::Str("f13".into())).unwrap();
        let path = std::path::PathBuf::from("/tmp/config.toml");
        let (cfg, warnings) = Config::build(
            None,
            Some((&path, &text)),
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert!(warnings.is_empty());
        assert_eq!(cfg.get("hotkey").unwrap().value, Value::Str("f13".into()));
    }
}
