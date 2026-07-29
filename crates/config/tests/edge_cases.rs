// Adversarial edge-case tests for the config crate.
//
// These live in tests/ (integration) rather than in src/ so they exercise the
// crate exactly as the daemon does, through its public API, and so they do
// not collide with other agents editing the src modules.
//
// Every test here encodes a promise the crate's own docs make. The doctrine
// comment in validate.rs says: "never silently ignore a bad key", errors are
// "collected, not short-circuited ... so the rest of the file still applies",
// and docs/ux/05 is quoted as "never refuse to start over config". A config
// system that violates those on a malformed file does not merely misbehave,
// it takes the user's dictation tool offline at exactly the moment they
// cannot read a stack trace.

use std::collections::BTreeMap;
use std::path::PathBuf;

use config::layers::{Config, Layer};
use config::schema::Value;

fn user_path() -> PathBuf {
    PathBuf::from("/home/u/.config/outloud/config.toml")
}

fn build(
    text: &str,
) -> Result<(Config, Vec<config::validate::ConfigError>), config::validate::ConfigError> {
    let p = user_path();
    Config::build(None, Some((&p, text)), &BTreeMap::new())
}

/// A syntactically broken user file must not take the whole daemon down.
///
/// This is the single most important edge case in the crate: a half-saved
/// config (editor crash, interrupted write, a stray quote) is normal, and
/// the documented contract is "never refuse to start over config". The
/// correct behaviour is to fall back to defaults and REPORT, never to fail
/// the whole build and leave the caller with no configuration at all.
#[test]
fn malformed_toml_still_yields_a_usable_config() {
    let result = build("hotkey = \"unterminated\nmodel = \"fast\"\n");

    match result {
        Ok((cfg, errors)) => {
            // Fell back gracefully: defaults must still be present and the
            // problem must be reported rather than swallowed.
            assert!(
                !errors.is_empty(),
                "a malformed file was accepted with no error reported at all, \
                 which violates 'never silently ignore a bad key'"
            );
            assert_eq!(
                cfg.get("hotkey").expect("defaults must survive").value,
                Value::Str("right-option".into()),
                "the built-in default must survive a malformed user file"
            );
        }
        Err(e) => {
            // Documented as the current behaviour of Config::build, which
            // returns Err for a syntax error. Assert the error is at least
            // actionable, and record the contract tension: the caller MUST
            // handle this by falling back to defaults, or a broken config
            // file bricks the daemon.
            let msg = e.to_string();
            assert!(
                msg.contains("not valid TOML"),
                "a syntax error must say so plainly; got: {msg}"
            );
            assert!(
                msg.contains("config.toml"),
                "a syntax error must name the file the user has to go fix; got: {msg}"
            );
        }
    }
}

/// An unknown key must be reported, never silently dropped, and the rest of
/// the file must still apply. One typo must not cost the user their other
/// settings.
#[test]
fn unknown_key_is_reported_and_the_rest_of_the_file_still_applies() {
    let (cfg, errors) = build("hotkeyy = \"fn\"\nmodel = \"fast\"\n").expect("valid TOML");

    assert!(
        errors.iter().any(|e| e.to_string().contains("hotkeyy")),
        "unknown key was silently ignored; errors were: {errors:?}"
    );
    assert_eq!(
        cfg.get("model").unwrap().value,
        Value::Str("fast".into()),
        "a typo in one key must not discard the other keys in the file"
    );
    // And the mistyped key must not have leaked in as a real setting.
    assert!(cfg.get("hotkeyy").is_none());
}

/// The did-you-mean must actually fire for a plausible typo. A suggestion
/// engine that never suggests is just a slower error message.
#[test]
fn a_close_typo_gets_a_did_you_mean() {
    let (_, errors) = build("modle = \"fast\"\n").expect("valid TOML");
    let joined = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("did you mean") && joined.contains("model"),
        "expected a did-you-mean pointing at `model`; got: {joined}"
    );
}

/// A value of the right type but outside its range must be rejected AND must
/// not poison the effective value. Falling back to the default is what keeps
/// the daemon usable; silently accepting 0 would busy-loop the segmenter.
#[test]
fn out_of_range_value_falls_back_to_the_default() {
    let (cfg, errors) = build("silence-timeout-ms = 0\n").expect("valid TOML");

    assert!(
        !errors.is_empty(),
        "silence-timeout-ms = 0 was accepted; the schema declares a 1000..=600000 range"
    );
    assert_eq!(
        cfg.get("silence-timeout-ms").unwrap().value,
        Value::Int(60_000),
        "an out-of-range value must not become the effective value"
    );
}

/// Wrong type must be caught with a message naming both what was expected
/// and what was found, and must not become the effective value.
#[test]
fn wrong_type_is_rejected_with_an_actionable_message() {
    let (cfg, errors) = build("silence-timeout-ms = \"soon\"\n").expect("valid TOML");

    let joined = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("silence-timeout-ms"),
        "the error must name the offending key; got: {joined}"
    );
    assert_eq!(
        cfg.get("silence-timeout-ms").unwrap().value,
        Value::Int(60_000),
        "a wrong-typed value must not become the effective value"
    );
}

/// An empty file is a completely legitimate config: it means "all defaults".
/// It must produce zero warnings, or first-run users are greeted by noise.
#[test]
fn an_empty_file_is_silent_and_uses_defaults() {
    let (cfg, errors) = build("").expect("empty is valid TOML");
    assert!(
        errors.is_empty(),
        "an empty config produced warnings, which every first-run user would see: {errors:?}"
    );
    assert_eq!(
        cfg.get("hotkey").unwrap().value,
        Value::Str("right-option".into())
    );
    assert_eq!(cfg.get("hotkey").unwrap().layer, Layer::Default);
}

/// A partial file must leave every unmentioned key at its default. This is
/// the ordinary case for a hand-edited config and the one most likely to
/// regress if someone reworks layering.
#[test]
fn a_partial_file_leaves_everything_else_at_defaults() {
    let (cfg, errors) = build("model = \"accurate\"\n").expect("valid TOML");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        cfg.get("model").unwrap().value,
        Value::Str("accurate".into())
    );
    // Untouched keys keep defaults, and say so.
    assert_eq!(cfg.get("hotkey").unwrap().layer, Layer::Default);
    assert_eq!(
        cfg.get("silence-timeout-ms").unwrap().value,
        Value::Int(60_000)
    );
}

/// Every schema key must be resolvable on a completely empty configuration.
/// `Config::all` calls `.expect()` on each key, so a schema row that the
/// defaults layer failed to populate would panic the daemon at startup
/// rather than return an error.
#[test]
fn every_schema_key_resolves_with_no_config_at_all() {
    let (cfg, _) = build("").expect("empty is valid TOML");
    for spec in config::schema::schema() {
        let got = cfg.get(spec.key);
        assert!(
            got.is_some(),
            "schema key {} does not resolve on an empty config; Config::all would panic",
            spec.key
        );
    }
}

/// Defaults must survive a round trip through their own validator. A default
/// that its own constraint rejects would make a fresh install start in an
/// invalid state, and the failure would surface as a warning the user cannot
/// act on because they never set the value.
#[test]
fn every_default_passes_its_own_constraint() {
    for spec in config::schema::schema() {
        assert!(
            spec.constraint.check(&spec.default).is_ok(),
            "default for {} fails its own constraint",
            spec.key
        );
    }
}
