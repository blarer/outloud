//! The layered store: defaults <- system file <- user file <- profile <- env.
//!
//! Every resolved value carries the layer it came from, because "why is my
//! hotkey not what I set" is unanswerable without provenance. `hexa status`
//! and validation errors both consume the same [`Provenance`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::profile::{select, AppIdentity, Profile, WinReason};
use crate::schema::{self, KeySpec, Value};
use crate::validate::{validate_document, ConfigError};

/// The environment-variable prefix for overrides: `HEXA_HOTKEY` sets
/// `hotkey`.
pub const ENV_PREFIX: &str = "HEXA_";

/// The previous product's prefix, still honoured. Frozen: this is history,
/// not configuration.
pub const LEGACY_ENV_PREFIX: &str = "AQUA_";

/// Where a value came from. Ordering is precedence: later variants override
/// earlier ones.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// Compiled-in default from the schema table.
    Default,
    /// The machine-wide file (e.g. /etc/hexavoice/config.toml), for managed
    /// deployments. Carries the path actually read.
    SystemFile(PathBuf),
    /// The user's own config.toml.
    UserFile(PathBuf),
    /// A matched per-app profile; carries the profile name so the answer to
    /// "why" is "profile 'slack' in your config".
    Profile(String),
    /// A `HEXA_*` (or legacy `AQUA_*`) environment variable; carries the
    /// variable name. Highest
    /// precedence because env vars are how one-off debugging and CI say
    /// "just this run, do this".
    Env(String),
}

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Layer::Default => write!(f, "built-in default"),
            Layer::SystemFile(p) => write!(f, "system file {}", p.display()),
            Layer::UserFile(p) => write!(f, "user file {}", p.display()),
            Layer::Profile(name) => write!(f, "profile \"{name}\""),
            Layer::Env(var) => write!(f, "environment variable {var}"),
        }
    }
}

/// A resolved value and its full history: the winning (value, layer) plus
/// every shadowed (value, layer) beneath it, so a UI can render "hotkey =
/// f13 (from env AQUA_HOTKEY; user file set right-option; default
/// right-option)".
#[derive(Debug, Clone, PartialEq)]
pub struct Provenance {
    pub value: Value,
    pub layer: Layer,
    /// Shadowed entries, nearest-loser first.
    pub shadowed: Vec<(Value, Layer)>,
}

/// One layer's worth of key -> value settings, tagged with where it came
/// from. Layers are just data; all precedence logic lives in [`Config`].
#[derive(Debug, Clone)]
pub struct LayerValues {
    pub layer: Layer,
    pub values: BTreeMap<String, Value>,
}

/// The merged configuration. Holds the layer stack rather than a flattened
/// map so provenance is a lookup, not a reconstruction.
#[derive(Debug, Clone)]
pub struct Config {
    /// Precedence order, lowest first. Defaults always occupy index 0.
    layers: Vec<LayerValues>,
    /// Profiles from all files (system first, then user, so user profiles
    /// win file-order ties within `select`'s equal-rank rule).
    profiles: Vec<Profile>,
}

impl Config {
    /// Build from optional file contents and an environment snapshot.
    ///
    /// Takes strings, not paths: I/O stays in the caller (and in
    /// [`crate::watch`]) so this function is pure and every error path is
    /// testable. Fails only on *malformed* input; unknown keys and bad
    /// values are collected as [`ConfigError`]s by `validate` beforehand,
    /// and this function expects pre-validated documents.
    pub fn build(
        system: Option<(&PathBuf, &str)>,
        user: Option<(&PathBuf, &str)>,
        env: &BTreeMap<String, String>,
    ) -> Result<(Config, Vec<ConfigError>), ConfigError> {
        let mut layers = vec![LayerValues {
            layer: Layer::Default,
            values: schema::schema()
                .iter()
                .map(|s| (s.key.to_string(), s.default.clone()))
                .collect(),
        }];
        let mut profiles = Vec::new();
        let mut warnings = Vec::new();

        for (path, text, mk) in [
            system.map(|(p, t)| (p, t, Layer::SystemFile as fn(PathBuf) -> Layer)),
            user.map(|(p, t)| (p, t, Layer::UserFile as fn(PathBuf) -> Layer)),
        ]
        .into_iter()
        .flatten()
        {
            let layer = mk(path.clone());
            let parsed = validate_document(text, &layer)?;
            warnings.extend(parsed.errors);
            layers.push(LayerValues {
                layer,
                values: parsed.values,
            });
            profiles.extend(parsed.profiles);
        }

        // Environment overrides: HEXA_INSERTION_MODE -> insertion.mode.
        // Unknown HEXA_ variables are reported, not ignored: a typo'd env
        // override that silently does nothing is the same failure class as a
        // typo'd key.
        //
        // The product's previous `AQUA_` prefix is still accepted, and will
        // be indefinitely: an environment variable lives in a shell profile,
        // a CI config, and someone's muscle memory, so breaking it costs far
        // more than carrying six characters of compatibility. When the same
        // key is set under both prefixes the current one wins, because that
        // is the one the user set most recently on purpose.
        let mut env_values = BTreeMap::new();
        for (var, raw) in env {
            let rest = match var.strip_prefix(ENV_PREFIX) {
                Some(rest) => rest,
                None => match var.strip_prefix(LEGACY_ENV_PREFIX) {
                    // Skip a legacy variable whose current-prefix twin is
                    // also set, so precedence never depends on the iteration
                    // order of the environment map.
                    Some(rest) if env.contains_key(&format!("{ENV_PREFIX}{rest}")) => continue,
                    Some(rest) => rest,
                    None => continue,
                },
            };
            let key = rest.to_lowercase().replace('_', ".");
            // The env spelling cannot distinguish '.' from '-' (both map
            // from '_'), so try schema keys whose normalized form matches.
            let spec = schema::schema()
                .iter()
                .find(|s| s.key.replace('-', ".") == key);
            match spec {
                Some(spec) => match Value::parse_env(spec, raw) {
                    Ok(v) => match spec.constraint.check(&v) {
                        Ok(()) => {
                            env_values.insert(spec.key.to_string(), (v, var.clone()));
                        }
                        Err(msg) => warnings.push(ConfigError::InvalidValue {
                            key: spec.key.to_string(),
                            layer: Layer::Env(var.clone()),
                            message: msg,
                        }),
                    },
                    Err(msg) => warnings.push(ConfigError::InvalidValue {
                        key: spec.key.to_string(),
                        layer: Layer::Env(var.clone()),
                        message: msg,
                    }),
                },
                None => warnings.push(ConfigError::UnknownKey {
                    key: var.clone(),
                    layer: Layer::Env(var.clone()),
                    suggestion: crate::fuzzy::closest(&key, schema::key_names()).map(|k| {
                        format!("{ENV_PREFIX}{}", k.replace(['.', '-'], "_").to_uppercase())
                    }),
                }),
            }
        }
        // One LayerValues per env var so provenance names the exact variable.
        for (key, (value, var)) in env_values {
            layers.push(LayerValues {
                layer: Layer::Env(var),
                values: BTreeMap::from([(key, value)]),
            });
        }

        Ok((Config { layers, profiles }, warnings))
    }

    /// The effective value of `key` with no app context (no profile layer).
    pub fn get(&self, key: &str) -> Option<Provenance> {
        self.get_for(key, None)
    }

    /// The effective value of `key` for the given frontmost app. The profile
    /// layer sits between the user file and env overrides: a profile is the
    /// user's own configuration and should win over their global setting,
    /// but an explicit environment override is a per-run command and must
    /// beat everything.
    pub fn get_for(&self, key: &str, app: Option<&AppIdentity>) -> Option<Provenance> {
        schema::spec_for(key)?;
        let mut hits: Vec<(Value, Layer)> = Vec::new();
        for lv in &self.layers {
            if let Some(v) = lv.values.get(key) {
                // Profile layer is spliced in above the user file: collect
                // file/default hits first, then profile, then env, by
                // partitioning on layer kind below.
                hits.push((v.clone(), lv.layer.clone()));
            }
        }
        if let Some(app) = app {
            if let Some((profile, _)) = select(&self.profiles, app) {
                if let Some(v) = profile.overrides.get(key) {
                    // Insert before any Env entries so env still wins.
                    let env_start = hits
                        .iter()
                        .position(|(_, l)| matches!(l, Layer::Env(_)))
                        .unwrap_or(hits.len());
                    hits.insert(env_start, (v.clone(), Layer::Profile(profile.name.clone())));
                }
            }
        }
        let (value, layer) = hits.pop()?;
        hits.reverse(); // nearest loser first
        Some(Provenance {
            value,
            layer,
            shadowed: hits,
        })
    }

    /// Which profile would apply to `app`, and why it won. Exposed so the
    /// settings UI and `hexa status --json` explain profile selection with
    /// the same logic that performs it.
    pub fn profile_for(&self, app: &AppIdentity) -> Option<(&Profile, WinReason)> {
        select(&self.profiles, app)
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Settings the user has actually SET that no code reads yet.
    ///
    /// The caller warns once at startup. This is the file-level counterpart
    /// to the menu bar's refusal to offer unwired settings: without it, a
    /// user edits `insertion.mode`, sees no error, observes no change, and
    /// reasonably concludes the feature is broken rather than unbuilt.
    ///
    /// Only user-supplied values count. A key sitting at its compiled-in
    /// default is not a broken promise, it is a placeholder, and warning
    /// about all 13 on every start would be noise a user learns to ignore,
    /// which is how a warning stops working.
    pub fn inert_settings(&self) -> Vec<&'static KeySpec> {
        schema::schema()
            .iter()
            .filter(|spec| !spec.wired)
            .filter(|spec| {
                // Set by someone other than the defaults layer?
                self.get(spec.key)
                    .is_some_and(|p| !matches!(p.layer, Layer::Default))
            })
            .collect()
    }

    /// Every known key with its provenance, for `hexa status --json` and the
    /// docs generator.
    pub fn all(&self, app: Option<&AppIdentity>) -> Vec<(&'static KeySpec, Provenance)> {
        schema::schema()
            .iter()
            .map(|spec| {
                let p = self
                    .get_for(spec.key, app)
                    .expect("defaults layer covers every schema key");
                (spec, p)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(user: &str, env: &[(&str, &str)]) -> (Config, Vec<ConfigError>) {
        let path = PathBuf::from("/home/u/.config/hexavoice/config.toml");
        let env: BTreeMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::build(None, Some((&path, user)), &env).unwrap()
    }

    #[test]
    fn defaults_apply_when_nothing_set() {
        let (cfg, warnings) = build("", &[]);
        assert!(warnings.is_empty());
        let p = cfg.get("hotkey").unwrap();
        assert_eq!(p.value, Value::Str("right-option".into()));
        assert_eq!(p.layer, Layer::Default);
        assert!(p.shadowed.is_empty());
    }

    #[test]
    fn user_file_overrides_default_and_provenance_names_the_file() {
        let (cfg, _) = build("hotkey = \"f13\"\n", &[]);
        let p = cfg.get("hotkey").unwrap();
        assert_eq!(p.value, Value::Str("f13".into()));
        assert!(matches!(p.layer, Layer::UserFile(_)));
        // The shadowed default is still reported.
        assert_eq!(
            p.shadowed,
            vec![(Value::Str("right-option".into()), Layer::Default)]
        );
    }

    #[test]
    fn system_file_sits_below_user_file() {
        let sys_path = PathBuf::from("/etc/hexavoice/config.toml");
        let user_path = PathBuf::from("/home/u/.config/hexavoice/config.toml");
        let (cfg, _) = Config::build(
            Some((&sys_path, "model = \"fast\"\nlanguage = \"en\"\n")),
            Some((&user_path, "model = \"accurate\"\n")),
            &BTreeMap::new(),
        )
        .unwrap();
        // User wins where both set a key...
        let p = cfg.get("model").unwrap();
        assert_eq!(p.value, Value::Str("accurate".into()));
        assert!(matches!(p.layer, Layer::UserFile(_)));
        // ...system still applies where the user is silent.
        let p = cfg.get("language").unwrap();
        assert_eq!(p.value, Value::Str("en".into()));
        assert!(matches!(p.layer, Layer::SystemFile(_)));
    }

    #[test]
    fn env_overrides_everything_and_names_the_variable() {
        let (cfg, warnings) = build("hotkey = \"f13\"\n", &[("AQUA_HOTKEY", "fn")]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let p = cfg.get("hotkey").unwrap();
        assert_eq!(p.value, Value::Str("fn".into()));
        assert_eq!(p.layer, Layer::Env("AQUA_HOTKEY".into()));
        // Full chain visible: user file first (nearest loser), then default.
        assert_eq!(p.shadowed.len(), 2);
        assert!(matches!(p.shadowed[0].1, Layer::UserFile(_)));
        assert_eq!(p.shadowed[1].1, Layer::Default);
    }

    #[test]
    fn env_dotted_keys_map_from_underscores() {
        let (cfg, warnings) = build("", &[("AQUA_INSERTION_MODE", "stream")]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let p = cfg.get("insertion.mode").unwrap();
        assert_eq!(p.value, Value::Str("stream".into()));
    }

    #[test]
    fn env_kebab_keys_also_map() {
        let (cfg, warnings) = build("", &[("AQUA_SILENCE_TIMEOUT_MS", "800")]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            cfg.get("silence-timeout-ms").unwrap().value,
            Value::Int(800)
        );
    }

    #[test]
    fn unknown_env_var_is_reported_with_suggestion() {
        let (_, warnings) = build("", &[("HEXA_HOTKYE", "fn")]);
        assert_eq!(warnings.len(), 1);
        match &warnings[0] {
            ConfigError::UnknownKey {
                key, suggestion, ..
            } => {
                assert_eq!(key, "HEXA_HOTKYE");
                // The suggestion names the CURRENT prefix even when the typo
                // came in under the legacy one: telling an upgrader to fix
                // their typo by keeping the deprecated spelling is advice
                // that ages badly.
                assert_eq!(suggestion.as_deref(), Some("HEXA_HOTKEY"));
            }
            other => panic!("expected UnknownKey, got {other}"),
        }
    }

    #[test]
    fn the_legacy_env_prefix_still_works() {
        // An environment variable lives in a shell profile and a CI config.
        // Breaking it on a rename would break other people's automation for
        // no benefit.
        let (cfg, warnings) = build("", &[("AQUA_HOTKEY", "f13")]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            cfg.get("hotkey").unwrap().value,
            Value::Str("f13".into()),
            "the legacy prefix must still be honoured"
        );
    }

    #[test]
    fn the_current_prefix_wins_when_both_are_set() {
        // Deterministically, and not by whichever the environment map
        // happened to yield first: a user who sets the new spelling is
        // saying "this one", and a stale line in a shell profile must not
        // silently beat it.
        let (cfg, warnings) = build(
            "",
            &[("AQUA_HOTKEY", "f13"), ("HEXA_HOTKEY", "right-option")],
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            cfg.get("hotkey").unwrap().value,
            Value::Str("right-option".into())
        );
    }

    #[test]
    fn invalid_env_value_is_reported_not_applied() {
        let (cfg, warnings) = build("", &[("AQUA_SILENCE_TIMEOUT_MS", "soon")]);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0], ConfigError::InvalidValue { .. }));
        // The default survives; a bad override must not poison the value.
        assert_eq!(
            cfg.get("silence-timeout-ms").unwrap().value,
            Value::Int(1500)
        );
    }

    #[test]
    fn profile_layer_beats_user_file_but_not_env() {
        let toml = r#"
formatting.casing = "standard"

[profile.slack]
match.bundle-id = "com.tinyspeck.slackmacgap"
formatting.casing = "casual-lowercase"
"#;
        let (cfg, warnings) = build(toml, &[]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let slack = AppIdentity {
            bundle_id: Some("com.tinyspeck.slackmacgap".into()),
            ..Default::default()
        };
        let p = cfg.get_for("formatting.casing", Some(&slack)).unwrap();
        assert_eq!(p.value, Value::Str("casual-lowercase".into()));
        assert_eq!(p.layer, Layer::Profile("slack".into()));

        // Env still beats the profile.
        let (cfg_env, _) = build(toml, &[("AQUA_FORMATTING_CASING", "standard")]);
        let p = cfg_env.get_for("formatting.casing", Some(&slack)).unwrap();
        assert!(matches!(p.layer, Layer::Env(_)));
        // And the profile shows up as the nearest shadowed layer.
        assert_eq!(p.shadowed[0].1, Layer::Profile("slack".into()));

        // A different app is untouched by the profile (no env override here).
        let other = AppIdentity {
            bundle_id: Some("com.apple.mail".into()),
            ..Default::default()
        };
        let p = cfg.get_for("formatting.casing", Some(&other)).unwrap();
        assert!(matches!(p.layer, Layer::UserFile(_)));
    }

    #[test]
    fn unknown_key_lookup_returns_none() {
        let (cfg, _) = build("", &[]);
        assert!(cfg.get("no-such-key").is_none());
    }

    #[test]
    fn all_reports_every_schema_key() {
        let (cfg, _) = build("hotkey = \"fn\"\n", &[]);
        let all = cfg.all(None);
        assert_eq!(all.len(), schema::schema().len());
        let hotkey = all.iter().find(|(s, _)| s.key == "hotkey").unwrap();
        assert!(matches!(hotkey.1.layer, Layer::UserFile(_)));
    }

    #[test]
    fn inert_settings_names_only_what_the_user_actually_set() {
        // Nothing set: silence. Warning about the 13 unwired defaults on
        // every start is noise, and noisy warnings stop being read.
        let (cfg, _) = build("", &[]);
        assert!(cfg.inert_settings().is_empty());

        // A wired setting the user changed is not inert, so it must not warn.
        let (cfg, _) = build("hotkey = \"f13\"\n", &[]);
        assert!(cfg.inert_settings().is_empty());

        // An unwired setting the user changed IS the case worth a warning:
        // they expect an effect and there is none.
        let (cfg, _) = build("insertion.mode = \"stream\"\n", &[]);
        let inert: Vec<&str> = cfg.inert_settings().iter().map(|s| s.key).collect();
        assert_eq!(inert, vec!["insertion.mode"]);
    }

    #[test]
    fn inert_settings_also_catches_env_overrides() {
        // An AQUA_* override is a per-run command, so a user who reaches for
        // one is even more certain it will take effect than one editing a file.
        let (cfg, _) = build("", &[("AQUA_FORMATTING_CASING", "casual-lowercase")]);
        let inert: Vec<&str> = cfg.inert_settings().iter().map(|s| s.key).collect();
        assert_eq!(inert, vec!["formatting.casing"]);
    }
}
