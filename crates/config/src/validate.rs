//! Parse and validate a config.toml, with errors good enough to act on.
//!
//! Doctrine: never silently ignore a bad key. Every unknown key gets a
//! did-you-mean; every invalid value says what would be valid; every
//! profile problem names the profiles involved. Errors are *collected*, not
//! short-circuited, so one typo does not hide the other three, and so the
//! rest of the file still applies (docs/ux/05: "never refuse to start over
//! config").

use std::collections::BTreeMap;
use std::fmt;

use toml_edit::{DocumentMut, Item, TomlError, Value as TomlValue};

use crate::fuzzy;
use crate::layers::Layer;
use crate::profile::{Matcher, Profile};
use crate::schema::{self, Value, SCHEMA_VERSION};

/// Everything wrong (or worth flagging) about a config source. Displayed
/// text is the product surface here: these strings go straight to the tray
/// notice and `aqua doctor`.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    /// The file is not valid TOML at all. Fatal for that file (there is no
    /// safe partial reading of malformed TOML); the caller falls back to the
    /// remaining layers and preserves the broken file.
    Syntax { layer: Layer, message: String },
    /// A key we do not recognize, with a suggestion when one is close.
    UnknownKey {
        key: String,
        layer: Layer,
        suggestion: Option<String>,
    },
    /// Right key, wrong type ("expected a string, got an integer").
    WrongType {
        key: String,
        layer: Layer,
        expected: &'static str,
        got: String,
    },
    /// Right key and type, but the value fails its constraint. The message
    /// comes from [`schema::Constraint::check`], which names valid values.
    InvalidValue {
        key: String,
        layer: Layer,
        message: String,
    },
    /// A profile table missing its matcher, or with a malformed one.
    BadProfile {
        name: String,
        layer: Layer,
        message: String,
    },
    /// Two profiles can match the same app. Not fatal (precedence resolves
    /// it deterministically) but reported, with the resolution spelled out,
    /// because a user who wrote both almost certainly expects only one.
    ProfileOverlap {
        winner: String,
        loser: String,
        layer: Layer,
        why: String,
    },
    /// The file declares a schema version newer than this binary knows.
    /// The file is still read best-effort, but the mismatch is named so
    /// "downgraded binary, upgraded config" is diagnosable.
    FutureSchema { layer: Layer, found: i64 },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Syntax { layer, message } => {
                write!(f, "{layer}: not valid TOML: {message}")
            }
            ConfigError::UnknownKey {
                key,
                layer,
                suggestion,
            } => {
                write!(f, "{layer}: unknown setting \"{key}\"")?;
                match suggestion {
                    Some(s) => write!(f, "; did you mean \"{s}\"?"),
                    None => write!(f, " (run `aqua set --list` for all settings)"),
                }
            }
            ConfigError::WrongType {
                key,
                layer,
                expected,
                got,
            } => write!(f, "{layer}: \"{key}\" expects a {expected}, got {got}"),
            ConfigError::InvalidValue {
                key,
                layer,
                message,
            } => write!(f, "{layer}: \"{key}\": {message}"),
            ConfigError::BadProfile {
                name,
                layer,
                message,
            } => write!(f, "{layer}: profile \"{name}\": {message}"),
            ConfigError::ProfileOverlap {
                winner,
                loser,
                layer,
                why,
            } => write!(
                f,
                "{layer}: profiles \"{winner}\" and \"{loser}\" can match the same \
                 application; \"{winner}\" wins because {why}"
            ),
            ConfigError::FutureSchema { layer, found } => write!(
                f,
                "{layer}: written by a newer version (schema-version {found}, this \
                 build understands {SCHEMA_VERSION}); unknown settings will be reported \
                 but preserved"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The result of validating one file: the values and profiles that were
/// readable, plus every problem found. Values and errors together, because
/// partial application is the contract.
#[derive(Debug)]
pub struct ValidatedDocument {
    pub values: BTreeMap<String, Value>,
    pub profiles: Vec<Profile>,
    pub errors: Vec<ConfigError>,
    /// The file's declared schema version (defaults to current when absent,
    /// which is correct for files we wrote and harmless for hand-made ones).
    pub schema_version: i64,
}

/// Parse and validate one TOML document. Returns `Err` only for TOML syntax
/// failures; every semantic problem is collected in `errors`.
pub fn validate_document(text: &str, layer: &Layer) -> Result<ValidatedDocument, ConfigError> {
    let doc: DocumentMut = text.parse().map_err(|e: TomlError| ConfigError::Syntax {
        layer: layer.clone(),
        message: e.to_string(),
    })?;

    let mut out = ValidatedDocument {
        values: BTreeMap::new(),
        profiles: Vec::new(),
        errors: Vec::new(),
        schema_version: SCHEMA_VERSION,
    };

    // Schema version first, so later errors can be interpreted in its light.
    if let Some(item) = doc.get("schema-version") {
        match item.as_integer() {
            Some(v) => {
                out.schema_version = v;
                if v > SCHEMA_VERSION {
                    out.errors.push(ConfigError::FutureSchema {
                        layer: layer.clone(),
                        found: v,
                    });
                }
            }
            None => out.errors.push(ConfigError::WrongType {
                key: "schema-version".into(),
                layer: layer.clone(),
                expected: "integer",
                got: toml_type_name(item).into(),
            }),
        }
    }

    // Walk the document. Keys arrive either dotted (`insertion.mode = ..`)
    // or as tables (`[insertion] mode = ..`); flattening first means the
    // schema does not care which spelling the user chose.
    let mut flat: Vec<(String, &Item)> = Vec::new();
    flatten("", doc.as_table(), &mut flat);

    for (key, item) in flat {
        if key == "schema-version" || key.starts_with("profile.") {
            continue; // handled separately
        }
        check_setting(&key, item, layer, &mut out.values, &mut out.errors);
    }

    // Profiles: [profile.<name>] with a match.* key and setting overrides.
    if let Some(profiles_item) = doc.get("profile") {
        match profiles_item.as_table() {
            Some(table) => {
                for (name, body) in table {
                    parse_profile(name, body, layer, &mut out);
                }
            }
            None => out.errors.push(ConfigError::WrongType {
                key: "profile".into(),
                layer: layer.clone(),
                expected: "table of profiles ([profile.myapp])",
                got: toml_type_name(profiles_item).into(),
            }),
        }
    }

    report_overlaps(&out.profiles, layer, &mut out.errors);
    Ok(out)
}

/// Flatten nested tables into dotted keys, stopping at leaves. `[profile]`
/// subtrees are kept intact (their keys are profile-local, not schema keys).
fn flatten<'a>(prefix: &str, table: &'a toml_edit::Table, out: &mut Vec<(String, &'a Item)>) {
    for (key, item) in table {
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };
        if path == "profile" || path.starts_with("profile.") {
            continue;
        }
        match item {
            Item::Table(t) => flatten(&path, t, out),
            // Inline tables used as sections ({ mode = "stream" }) are kept
            // as leaves: check_setting reports them as unknown/wrong type,
            // which names the key, rather than recursing into a second
            // representation of "table" and duplicating that logic.
            _ => out.push((path, item)),
        }
    }
}

fn check_setting(
    key: &str,
    item: &Item,
    layer: &Layer,
    values: &mut BTreeMap<String, Value>,
    errors: &mut Vec<ConfigError>,
) {
    let Some(spec) = schema::spec_for(key) else {
        errors.push(ConfigError::UnknownKey {
            key: key.to_string(),
            layer: layer.clone(),
            suggestion: fuzzy::closest(key, schema::key_names()).map(String::from),
        });
        return;
    };
    match coerce(item, &spec.default) {
        Ok(v) => match spec.constraint.check(&v) {
            Ok(()) => {
                values.insert(key.to_string(), v);
            }
            Err(message) => errors.push(ConfigError::InvalidValue {
                key: key.to_string(),
                layer: layer.clone(),
                message,
            }),
        },
        Err(got) => errors.push(ConfigError::WrongType {
            key: key.to_string(),
            layer: layer.clone(),
            expected: spec.default.type_name(),
            got,
        }),
    }
}

/// Convert a TOML item to our [`Value`], guided by the expected type from
/// the schema (`template`). Returns the actual type name on mismatch so the
/// error can say both sides.
fn coerce(item: &Item, template: &Value) -> Result<Value, String> {
    match template {
        Value::Str(_) => item
            .as_str()
            .map(|s| Value::Str(s.to_string()))
            .ok_or_else(|| toml_type_name(item).to_string()),
        Value::Bool(_) => item
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| toml_type_name(item).to_string()),
        Value::Int(_) => item
            .as_integer()
            .map(Value::Int)
            .ok_or_else(|| toml_type_name(item).to_string()),
        Value::List(_) => {
            let arr = item
                .as_array()
                .ok_or_else(|| toml_type_name(item).to_string())?;
            let mut items = Vec::with_capacity(arr.len());
            for v in arr {
                match v.as_str() {
                    Some(s) => items.push(s.to_string()),
                    None => return Err(format!("array containing a non-string ({v})")),
                }
            }
            Ok(Value::List(items))
        }
    }
}

fn toml_type_name(item: &Item) -> &'static str {
    match item {
        Item::None => "nothing",
        Item::Table(_) => "table",
        Item::ArrayOfTables(_) => "array of tables",
        Item::Value(v) => match v {
            TomlValue::String(_) => "string",
            TomlValue::Integer(_) => "integer",
            TomlValue::Float(_) => "float",
            TomlValue::Boolean(_) => "boolean",
            TomlValue::Datetime(_) => "datetime",
            TomlValue::Array(_) => "array",
            TomlValue::InlineTable(_) => "inline table",
        },
    }
}

fn parse_profile(name: &str, body: &Item, layer: &Layer, out: &mut ValidatedDocument) {
    let Some(table) = body.as_table() else {
        out.errors.push(ConfigError::BadProfile {
            name: name.to_string(),
            layer: layer.clone(),
            message: format!(
                "expected a table ([profile.{name}]), got {}",
                toml_type_name(body)
            ),
        });
        return;
    };

    // The matcher lives under `match.*`; exactly one matcher key.
    let matcher = match table.get("match").and_then(Item::as_table_like) {
        Some(m) => {
            let mut found: Vec<Matcher> = Vec::new();
            for (k, v) in m.iter() {
                let Some(pattern) = v.as_str() else {
                    out.errors.push(ConfigError::BadProfile {
                        name: name.to_string(),
                        layer: layer.clone(),
                        message: format!("match.{k} must be a string, got {}", toml_type_name(v)),
                    });
                    return;
                };
                match k {
                    "bundle-id" => found.push(Matcher::BundleId(pattern.into())),
                    "process-name" => found.push(Matcher::ProcessName(pattern.into())),
                    "window-class" => found.push(Matcher::WindowClass(pattern.into())),
                    other => {
                        let suggestion =
                            fuzzy::closest(other, ["bundle-id", "process-name", "window-class"]);
                        out.errors.push(ConfigError::BadProfile {
                            name: name.to_string(),
                            layer: layer.clone(),
                            message: match suggestion {
                                Some(s) => {
                                    format!("unknown matcher \"match.{other}\"; did you mean \"match.{s}\"?")
                                }
                                None => format!(
                                    "unknown matcher \"match.{other}\"; valid matchers are \
                                     match.bundle-id, match.process-name, match.window-class"
                                ),
                            },
                        });
                        return;
                    }
                }
            }
            match found.len() {
                1 => found.remove(0),
                0 => {
                    out.errors.push(ConfigError::BadProfile {
                        name: name.to_string(),
                        layer: layer.clone(),
                        message: "match table has no matcher; set match.bundle-id, \
                                  match.process-name, or match.window-class"
                            .into(),
                    });
                    return;
                }
                _ => {
                    // Multiple matchers in one profile would need and/or
                    // semantics nobody can guess from the file. Split into
                    // two profiles instead; the error says so.
                    out.errors.push(ConfigError::BadProfile {
                        name: name.to_string(),
                        layer: layer.clone(),
                        message: "profile has multiple matchers; use one matcher per \
                                  profile (make a second profile for the other)"
                            .into(),
                    });
                    return;
                }
            }
        }
        None => {
            out.errors.push(ConfigError::BadProfile {
                name: name.to_string(),
                layer: layer.clone(),
                message: "missing matcher; set match.bundle-id, match.process-name, \
                          or match.window-class under [profile.<name>]"
                    .into(),
            });
            return;
        }
    };

    // Remaining keys are overrides, validated against the same schema as
    // top-level settings, because a profile is just a scoped layer.
    let mut overrides = BTreeMap::new();
    let mut flat: Vec<(String, &Item)> = Vec::new();
    for (k, v) in table {
        if k == "match" {
            continue;
        }
        match v {
            Item::Table(t) => flatten(k, t, &mut flat),
            _ => flat.push((k.to_string(), v)),
        }
    }
    let profile_layer = Layer::Profile(name.to_string());
    for (k, v) in flat {
        check_setting(&k, v, &profile_layer, &mut overrides, &mut out.errors);
    }

    out.profiles.push(Profile {
        name: name.to_string(),
        matcher,
        overrides,
    });
}

/// Detect statically-overlapping profiles and report which wins. Two
/// matchers overlap when one's pattern set intersects the other's; with our
/// exact-or-prefix grammar that is decidable by string comparison. Different
/// matcher kinds can also co-fire on one app, but that is the *designed*
/// layering (bundle id refines process name), so only same-kind overlaps are
/// flagged.
fn report_overlaps(profiles: &[Profile], layer: &Layer, errors: &mut Vec<ConfigError>) {
    for (i, a) in profiles.iter().enumerate() {
        for b in &profiles[i + 1..] {
            if a.matcher.kind() != b.matcher.kind() {
                continue;
            }
            let (pa, pb) = (
                a.matcher.pattern().to_lowercase(),
                b.matcher.pattern().to_lowercase(),
            );
            if !patterns_overlap(&pa, &pb) {
                continue;
            }
            // Decide the winner with the same ranking select() uses: an
            // exact pattern or longer prefix wins; a full tie goes to file
            // order (a is earlier).
            let exact = |p: &str| !p.ends_with('*');
            let plen = |p: &str| p.trim_end_matches('*').len();
            let (winner, loser, why) = if exact(&pa) != exact(&pb) {
                if exact(&pa) {
                    (a, b, "an exact match beats a prefix pattern".to_string())
                } else {
                    (b, a, "an exact match beats a prefix pattern".to_string())
                }
            } else if plen(&pa) != plen(&pb) {
                if plen(&pa) > plen(&pb) {
                    (a, b, "a longer prefix is more specific".to_string())
                } else {
                    (b, a, "a longer prefix is more specific".to_string())
                }
            } else {
                (a, b, "it comes first in the file".to_string())
            };
            errors.push(ConfigError::ProfileOverlap {
                winner: winner.name.clone(),
                loser: loser.name.clone(),
                layer: layer.clone(),
                why,
            });
        }
    }
}

/// Whether two exact-or-prefix patterns can match a common string.
fn patterns_overlap(a: &str, b: &str) -> bool {
    match (a.strip_suffix('*'), b.strip_suffix('*')) {
        (None, None) => a == b,
        (Some(pa), None) => b.starts_with(pa),
        (None, Some(pb)) => a.starts_with(pb),
        (Some(pa), Some(pb)) => pa.starts_with(pb) || pb.starts_with(pa),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn layer() -> Layer {
        Layer::UserFile(PathBuf::from("/tmp/config.toml"))
    }

    fn validate(text: &str) -> ValidatedDocument {
        validate_document(text, &layer()).unwrap()
    }

    #[test]
    fn malformed_toml_is_a_syntax_error_naming_the_file() {
        let err = validate_document("hotkey = \"unterminated", &layer()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/tmp/config.toml"), "{msg}");
        assert!(msg.contains("not valid TOML"), "{msg}");
    }

    #[test]
    fn unknown_key_gets_did_you_mean() {
        let doc = validate("hotkye = \"fn\"\n");
        assert_eq!(doc.errors.len(), 1);
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("did you mean \"hotkey\""), "{msg}");
        // And the bad key must not sneak into values.
        assert!(doc.values.is_empty());
    }

    #[test]
    fn unknown_key_without_a_near_miss_points_at_the_list() {
        let doc = validate("frobnicator = 7\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("aqua set --list"), "{msg}");
    }

    #[test]
    fn wrong_type_names_both_types() {
        let doc = validate("hotkey = 13\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("expects a string"), "{msg}");
        assert!(msg.contains("integer"), "{msg}");
    }

    #[test]
    fn invalid_hotkey_says_what_is_valid() {
        let doc = validate("hotkey = \"cmd+shift\"\n");
        let msg = doc.errors[0].to_string();
        // The message must teach the grammar, not just reject.
        assert!(msg.contains("right-option"), "{msg}");
        assert!(msg.contains("cmd+shift+space"), "{msg}");
    }

    #[test]
    fn invalid_enum_lists_the_choices() {
        let doc = validate("model = \"turbo\"\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("fast, balanced, accurate"), "{msg}");
    }

    #[test]
    fn out_of_range_int_reports_the_range() {
        let doc = validate("silence-timeout-ms = 50\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("200..=30000"), "{msg}");
    }

    #[test]
    fn good_keys_apply_even_when_others_fail() {
        // One typo must not take down the whole file.
        let doc = validate("hotkey = \"f13\"\nmodle = \"fast\"\n");
        assert_eq!(doc.values.get("hotkey"), Some(&Value::Str("f13".into())));
        assert_eq!(doc.errors.len(), 1);
    }

    #[test]
    fn dotted_and_table_spellings_are_equivalent() {
        let dotted = validate("insertion.mode = \"stream\"\n");
        let table = validate("[insertion]\nmode = \"stream\"\n");
        assert_eq!(dotted.values, table.values);
        assert!(dotted.errors.is_empty() && table.errors.is_empty());
    }

    #[test]
    fn list_values_parse_and_reject_non_strings() {
        let ok = validate("vocabulary.sets = [\"code\", \"k8s\"]\n");
        assert_eq!(
            ok.values.get("vocabulary.sets"),
            Some(&Value::List(vec!["code".into(), "k8s".into()]))
        );
        let bad = validate("vocabulary.sets = [\"code\", 3]\n");
        assert!(matches!(bad.errors[0], ConfigError::WrongType { .. }));
    }

    #[test]
    fn profile_parses_matcher_and_overrides() {
        let doc = validate(
            "[profile.terminal]\nmatch.bundle-id = \"com.apple.terminal\"\nformatting.smart-quotes = false\n",
        );
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
        assert_eq!(doc.profiles.len(), 1);
        let p = &doc.profiles[0];
        assert_eq!(p.matcher, Matcher::BundleId("com.apple.terminal".into()));
        assert_eq!(
            p.overrides.get("formatting.smart-quotes"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn profile_without_matcher_is_an_error_naming_the_fix() {
        let doc = validate("[profile.broken]\nenabled = false\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("profile \"broken\""), "{msg}");
        assert!(msg.contains("match.bundle-id"), "{msg}");
        assert!(doc.profiles.is_empty());
    }

    #[test]
    fn profile_matcher_typo_gets_did_you_mean() {
        let doc = validate("[profile.t]\nmatch.bundleid = \"com.apple.terminal\"\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("did you mean \"match.bundle-id\""), "{msg}");
    }

    #[test]
    fn profile_with_two_matchers_is_rejected_with_advice() {
        let doc = validate("[profile.t]\nmatch.bundle-id = \"a\"\nmatch.process-name = \"b\"\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("one matcher per profile"), "{msg}");
    }

    #[test]
    fn profile_override_is_validated_like_a_top_level_key() {
        let doc = validate("[profile.t]\nmatch.process-name = \"nvim\"\nmodel = \"turbo\"\n");
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("fast, balanced, accurate"), "{msg}");
        // The profile itself still exists, minus the bad override.
        assert_eq!(doc.profiles.len(), 1);
        assert!(doc.profiles[0].overrides.is_empty());
    }

    #[test]
    fn overlapping_profiles_report_winner_and_why() {
        let doc = validate(
            "[profile.jb]\nmatch.bundle-id = \"com.jetbrains.*\"\n\
             [profile.clion]\nmatch.bundle-id = \"com.jetbrains.clion\"\n",
        );
        assert_eq!(doc.errors.len(), 1);
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("\"clion\" wins"), "{msg}");
        assert!(msg.contains("exact match beats a prefix"), "{msg}");
    }

    #[test]
    fn identical_matchers_resolve_by_file_order() {
        let doc = validate(
            "[profile.one]\nmatch.process-name = \"nvim\"\n\
             [profile.two]\nmatch.process-name = \"nvim\"\n",
        );
        let msg = doc.errors[0].to_string();
        assert!(msg.contains("\"one\" wins"), "{msg}");
        assert!(msg.contains("first in the file"), "{msg}");
    }

    #[test]
    fn different_kind_matchers_do_not_report_overlap() {
        // Bundle id refining a process name is the designed precedence, not
        // a conflict.
        let doc = validate(
            "[profile.a]\nmatch.process-name = \"electron\"\n\
             [profile.b]\nmatch.bundle-id = \"com.tinyspeck.slackmacgap\"\n",
        );
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
    }

    #[test]
    fn future_schema_version_is_flagged_but_not_fatal() {
        let doc = validate("schema-version = 99\nhotkey = \"fn\"\n");
        assert!(matches!(
            doc.errors[0],
            ConfigError::FutureSchema { found: 99, .. }
        ));
        // Best-effort reading still applied the known key.
        assert_eq!(doc.values.get("hotkey"), Some(&Value::Str("fn".into())));
    }

    #[test]
    fn pattern_overlap_logic() {
        assert!(patterns_overlap("a.b", "a.b"));
        assert!(!patterns_overlap("a.b", "a.c"));
        assert!(patterns_overlap("a.*", "a.b"));
        assert!(patterns_overlap("a.*", "a.b.*"));
        assert!(!patterns_overlap("a.*", "b.*"));
    }
}
