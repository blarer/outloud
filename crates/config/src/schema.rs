//! The schema: every key the config system understands, its type, default,
//! and validation rule, in one table.
//!
//! Centralizing this is what makes three promises cheap to keep at once:
//! unknown keys get did-you-mean suggestions (the candidate list is right
//! here), every key has a documented default (docs/configuration.md is
//! generated from the same facts), and validation errors can say what *would*
//! be valid because the spec knows.

use std::fmt;
use std::str::FromStr;

use hotkey::Chord;

/// A configuration value. Deliberately a tiny closed set: config is settings,
/// not a programming language, and every type here has an obvious TOML and
/// environment-variable spelling.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Bool(bool),
    Int(i64),
    /// A list of strings, e.g. active vocabulary sets.
    List(Vec<String>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "\"{s}\""),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "\"{item}\"")?;
                }
                write!(f, "]")
            }
        }
    }
}

impl Value {
    /// The type name used in error messages ("expected a string").
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Str(_) => "string",
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::List(_) => "list of strings",
        }
    }

    /// Parse from an environment-variable string, guided by the expected
    /// type. Environment values are always strings, so the schema decides
    /// how to read them; lists split on commas because that is the least
    /// surprising convention for PATH-shaped variables.
    pub fn parse_env(spec: &KeySpec, raw: &str) -> Result<Value, String> {
        match spec.default {
            Value::Str(_) => Ok(Value::Str(raw.to_string())),
            Value::Bool(_) => match raw.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
                "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
                _ => Err(format!("expected a boolean (true/false), got \"{raw}\"")),
            },
            Value::Int(_) => raw
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| format!("expected an integer, got \"{raw}\"")),
            Value::List(_) => Ok(Value::List(
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )),
        }
    }
}

/// A validation rule beyond type-checking. Kept as an enum, not closures, so
/// the rule can also *describe itself* in errors and documentation.
#[derive(Debug, Clone, Copy)]
pub enum Constraint {
    /// Any value of the right type.
    None,
    /// One of a fixed set of strings.
    OneOf(&'static [&'static str]),
    /// Must parse as a hotkey chord (validated by the `hotkey` crate, the
    /// same parser the runtime binds with, so "valid here" means "binds
    /// there").
    HotkeyChord,
    /// Integer within an inclusive range.
    IntRange(i64, i64),
}

impl Constraint {
    /// Check a value, returning a message that names what would be valid.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        match (self, value) {
            (Constraint::None, _) => Ok(()),
            (Constraint::OneOf(options), Value::Str(s)) => {
                if options.contains(&s.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "\"{s}\" is not one of the allowed values: {}",
                        options.join(", ")
                    ))
                }
            }
            (Constraint::HotkeyChord, Value::Str(s)) => match Chord::from_str(s) {
                Ok(_) => Ok(()),
                Err(e) => Err(format!(
                    "\"{s}\" is not a valid hotkey: {e}. Valid examples: \
                     \"right-option\", \"fn\", \"cmd+shift+space\", \"f13\""
                )),
            },
            (Constraint::IntRange(lo, hi), Value::Int(i)) => {
                if i >= lo && i <= hi {
                    Ok(())
                } else {
                    Err(format!("{i} is out of range ({lo}..={hi})"))
                }
            }
            // A constraint applied to the wrong type is a schema-table bug;
            // type mismatches are caught before constraints run.
            _ => Err(format!(
                "internal: constraint {self:?} cannot check a {}",
                value.type_name()
            )),
        }
    }
}

/// One entry in the schema table.
#[derive(Debug, Clone)]
pub struct KeySpec {
    /// Dotted key path as written in TOML, e.g. "insertion.mode".
    pub key: &'static str,
    pub default: Value,
    pub constraint: Constraint,
    /// One-line effect description, reused by docs/configuration.md.
    pub doc: &'static str,
    /// Does any code actually READ this setting yet?
    ///
    /// A config file that accepts a setting nothing reads is worse than one
    /// that rejects it: the user changes a value, sees no error, and
    /// concludes the feature is broken rather than absent. The menu bar
    /// already guards against this by refusing to offer unwired keys; the
    /// file had no equivalent protection, so 13 of these 16 settings were
    /// silently inert.
    ///
    /// Deliberately NOT defaulted. Adding a schema row forces an explicit
    /// true/false, so the next person to add a setting has to answer "does
    /// anything read this" at the moment they can still answer it cheaply.
    /// Flip to true in the same commit that wires the setting up.
    pub wired: bool,
}

/// Settings the user can set today that no code reads yet.
///
/// Callers use this to warn ONCE at startup, and only for keys the user
/// actually set (a defaulted unwired key is not a lie, it is just a
/// placeholder). Empty means every setting in the schema does something,
/// which is the state to aim for.
pub fn unwired_keys() -> impl Iterator<Item = &'static KeySpec> {
    schema().iter().filter(|s| !s.wired)
}

/// The full schema. Adding a setting means adding one row here; everything
/// else (validation, docs, env overrides, provenance) follows.
pub fn schema() -> &'static [KeySpec] {
    use Constraint::*;
    // `LazyLock` over `const` because `Value::Str` allocates.
    static SCHEMA: std::sync::LazyLock<Vec<KeySpec>> = std::sync::LazyLock::new(|| {
        let s = |v: &str| Value::Str(v.to_string());
        vec![
            KeySpec {
                key: "hotkey",
                default: s("right-option"),
                constraint: HotkeyChord,
                doc: "Push-to-talk key. Hold to dictate, tap to latch.",
                wired: true,
            },
            KeySpec {
                key: "microphone",
                default: s("auto"),
                constraint: None,
                doc: "Input device name, or \"auto\" to follow the system default.",
                wired: false,
            },
            KeySpec {
                key: "language",
                default: s("auto"),
                constraint: None,
                doc: "Recognition language code (e.g. \"en\"), or \"auto\" to detect.",
                wired: false,
            },
            KeySpec {
                key: "model",
                default: s("balanced"),
                constraint: OneOf(&["fast", "balanced", "accurate"]),
                doc: "Recognizer size/speed trade-off.",
                wired: false,
            },
            KeySpec {
                key: "enabled",
                default: Value::Bool(true),
                constraint: None,
                doc: "Master switch. Profiles set this to false to mute the tool in an app.",
                wired: true,
            },
            KeySpec {
                key: "insertion.mode",
                default: s("on-release"),
                constraint: OneOf(&["on-release", "stream"]),
                doc: "Insert the whole utterance on key release, or stream words as spoken.",
                wired: true,
            },
            KeySpec {
                key: "insertion.paste-fallback",
                default: Value::Bool(false),
                constraint: None,
                doc: "Force clipboard-paste insertion for apps with broken accessibility.",
                wired: false,
            },
            KeySpec {
                key: "formatting.casing",
                default: s("standard"),
                constraint: OneOf(&["standard", "casual-lowercase"]),
                doc: "Sentence casing style; chat apps often read better casual-lowercase.",
                wired: false,
            },
            KeySpec {
                key: "formatting.smart-quotes",
                default: Value::Bool(true),
                constraint: None,
                doc: "Convert straight quotes to typographic quotes.",
                wired: false,
            },
            KeySpec {
                key: "formatting.trailing-punctuation",
                default: Value::Bool(true),
                constraint: None,
                doc: "End utterances with inferred punctuation.",
                wired: false,
            },
            KeySpec {
                key: "history.enabled",
                default: Value::Bool(true),
                constraint: None,
                doc: "Keep a local plain-text transcription history.",
                wired: false,
            },
            KeySpec {
                key: "microphone.sensitivity",
                default: Value::Int(50),
                constraint: IntRange(1, 100),
                doc: "How quiet a voice still counts as speech. Raise it if you \
                      sit back from the microphone or speak softly; lower it if \
                      room noise is being transcribed as words.",
                wired: true,
            },
            KeySpec {
                key: "silence-timeout-ms",
                default: Value::Int(60_000),
                constraint: IntRange(1_000, 600_000),
                doc: "Safety net: force-commit and close the microphone after \
                      capture has run this long. Push-to-talk ends on key \
                      release; tap-to-latch waits for a second tap that may \
                      never come.",
                wired: true,
            },
            KeySpec {
                key: "overlay.position",
                default: s("bottom-center"),
                constraint: OneOf(&[
                    "bottom-center",
                    "bottom-left",
                    "bottom-right",
                    "top-center",
                    "hidden",
                ]),
                doc: "Where the listening overlay appears, or hidden.",
                wired: true,
            },
            KeySpec {
                key: "vocabulary.sets",
                default: Value::List(vec![]),
                constraint: None,
                doc: "Named vocabulary sets active by default; profiles override per app.",
                wired: false,
            },
            KeySpec {
                key: "telemetry.enabled",
                default: Value::Bool(false),
                constraint: None,
                doc: "Anonymous usage reporting. Off by default, forever.",
                wired: false,
            },
            KeySpec {
                key: "launch-at-login",
                default: Value::Bool(false),
                constraint: None,
                doc: "Start the daemon when the user logs in.",
                wired: false,
            },
        ]
    });
    &SCHEMA
}

/// Look up a key's spec, exact match on the dotted path.
pub fn spec_for(key: &str) -> Option<&'static KeySpec> {
    schema().iter().find(|s| s.key == key)
}

/// All key names, for did-you-mean candidate lists.
pub fn key_names() -> impl Iterator<Item = &'static str> {
    schema().iter().map(|s| s.key)
}

/// Current schema version. Bump when a key is renamed/removed/retyped, and
/// add a step in `migrate.rs`. Deciding this before shipping means the first
/// released file already carries `schema-version = 1` and future upgrades
/// have something to read.
pub const SCHEMA_VERSION: i64 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_satisfies_its_own_constraint() {
        // A default that fails validation would make a fresh install invalid.
        for spec in schema() {
            assert!(
                spec.constraint.check(&spec.default).is_ok(),
                "default for {} violates its constraint",
                spec.key
            );
        }
    }

    #[test]
    fn keys_are_unique_and_kebab_dotted() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in schema() {
            assert!(seen.insert(spec.key), "duplicate key {}", spec.key);
            assert!(
                spec.key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.'),
                "key {} not kebab-case",
                spec.key
            );
        }
    }

    #[test]
    fn constraint_messages_name_the_valid_values() {
        let err = Constraint::OneOf(&["fast", "balanced"])
            .check(&Value::Str("turbo".into()))
            .unwrap_err();
        assert!(err.contains("fast, balanced"), "{err}");

        let err = Constraint::HotkeyChord
            .check(&Value::Str("cmd+shift".into()))
            .unwrap_err();
        assert!(err.contains("right-option"), "{err}");

        let err = Constraint::IntRange(200, 30_000)
            .check(&Value::Int(5))
            .unwrap_err();
        assert!(err.contains("200..=30000"), "{err}");
    }

    #[test]
    fn env_parsing_follows_the_schema_type() {
        let b = spec_for("telemetry.enabled").unwrap();
        assert_eq!(Value::parse_env(b, "on").unwrap(), Value::Bool(true));
        assert_eq!(Value::parse_env(b, "0").unwrap(), Value::Bool(false));
        assert!(Value::parse_env(b, "maybe").is_err());

        let i = spec_for("silence-timeout-ms").unwrap();
        assert_eq!(Value::parse_env(i, "800").unwrap(), Value::Int(800));
        assert!(Value::parse_env(i, "fast").is_err());

        let l = spec_for("vocabulary.sets").unwrap();
        assert_eq!(
            Value::parse_env(l, "code, k8s").unwrap(),
            Value::List(vec!["code".into(), "k8s".into()])
        );
    }

    #[test]
    fn the_wired_set_matches_what_the_daemon_actually_reads() {
        // Mirrors the WIRED gate in crates/outloud/src/menubar.rs. Two lists
        // that must agree is a smell, but they protect different surfaces
        // (the menu vs the file) and live in different crates, so the
        // duplication is deliberate and this test is what keeps them honest.
        //
        // When a setting is wired up, flip its `wired` flag and update this
        // list in the same commit. A test failure here means someone either
        // wired a setting without telling the config layer, or added a
        // setting the menu will silently refuse to offer.
        let wired: Vec<&str> = schema().iter().filter(|s| s.wired).map(|s| s.key).collect();
        assert_eq!(
            wired,
            vec![
                "hotkey",
                "enabled",
                "insertion.mode",
                "microphone.sensitivity",
                "silence-timeout-ms",
                "overlay.position"
            ]
        );
    }

    #[test]
    fn unwired_keys_reports_exactly_the_inert_settings() {
        let inert: Vec<&str> = unwired_keys().map(|s| s.key).collect();
        // Not asserted as a fixed list on purpose: this number should only
        // ever go DOWN, and pinning it exactly would make wiring a setting
        // fail an unrelated-looking test. What must hold is the invariant.
        assert_eq!(inert.len(), schema().len() - 6);
        assert!(
            !inert.contains(&"silence-timeout-ms"),
            "silence-timeout-ms is wired: the pipeline force-closes a hot mic on it"
        );
        assert!(
            !inert.contains(&"microphone.sensitivity"),
            "microphone.sensitivity is wired: the pipeline builds its VAD from it"
        );
        assert!(
            !inert.contains(&"insertion.mode"),
            "insertion.mode is wired: the pipeline streams partials when it is \"stream\""
        );
        assert!(
            !inert.contains(&"hotkey"),
            "hotkey is the one setting that demonstrably works end to end"
        );
    }

    #[test]
    fn every_key_documents_itself() {
        // The doc string is the only explanation a user gets in the
        // generated starter file, so an empty one ships a mystery setting.
        for spec in schema() {
            assert!(!spec.doc.trim().is_empty(), "{} has no doc", spec.key);
        }
    }
}
