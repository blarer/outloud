//! Per-app profiles must actually reach the daemon.
//!
//! `Config::get_for(key, app)` and the whole matcher in
//! `crates/config/src/profile.rs` were complete, tested, and never called
//! with an app by anything in the daemon. So every `[profile.slack]` block
//! a user wrote was silently ignored, including the five worked examples in
//! `docs/configuration.md`.
//!
//! A feature the docs teach and the code ignores is worse than a missing
//! one: the user changes a setting, sees no effect, and has no way to tell
//! whether they wrote it wrong or it does nothing. These tests exercise the
//! path the daemon actually takes, so that cannot silently regress.

use ax_edit::TextSnapshot;
use config::AppIdentity;
use outloud::menubar::Settings;

/// A config with a global value and one app-specific override.
const CONFIG: &str = r#"
"insertion.mode" = "on-release"
enabled = true

[profile.terminal]
match.bundle-id = "com.apple.Terminal"
"insertion.mode" = "stream"

[profile.games]
match.bundle-id = "com.valvesoftware.steam"
enabled = false
"#;

fn built() -> config::Config {
    let path = std::path::PathBuf::from("/test/config.toml");
    let (cfg, _warnings) = config::Config::build(None, Some((&path, CONFIG)), &Default::default())
        .expect("fixture config must parse");
    cfg
}

fn identity(bundle: &str) -> AppIdentity {
    AppIdentity {
        bundle_id: Some(bundle.into()),
        process_name: None,
        window_class: None,
    }
}

#[test]
fn a_matching_profile_overrides_the_global_value() {
    let cfg = built();
    let global = Settings::from_config(&cfg, None);
    assert_eq!(
        global.insertion_mode, "on-release",
        "precondition: the global value is what the file says"
    );

    let in_terminal = Settings::from_config_for(&cfg, None, Some(&identity("com.apple.Terminal")));
    assert_eq!(
        in_terminal.insertion_mode, "stream",
        "the profile override must win for the app it matches"
    );
}

#[test]
fn a_non_matching_app_keeps_the_global_value() {
    // The other half of the property, and the one a naive implementation
    // gets wrong: a profile must not leak onto every app.
    let cfg = built();
    let elsewhere = Settings::from_config_for(&cfg, None, Some(&identity("com.apple.TextEdit")));
    assert_eq!(elsewhere.insertion_mode, "on-release");
}

#[test]
fn a_profile_can_mute_dictation_for_one_app() {
    // `enabled = false` in a profile is the documented way to keep the
    // hotkey from firing inside a game. It is also the setting most likely
    // to look like a crash, which is why the pipeline logs when it applies.
    let cfg = built();
    let in_game = Settings::from_config_for(&cfg, None, Some(&identity("com.valvesoftware.steam")));
    assert!(!in_game.enabled, "a profile must be able to mute an app");

    let elsewhere = Settings::from_config_for(&cfg, None, Some(&identity("com.apple.TextEdit")));
    assert!(elsewhere.enabled, "muting one app must not mute the rest");
}

#[test]
fn no_app_means_no_profile() {
    // The menu bar has no app context: it shows global settings. Resolving
    // a profile there would display another app's overrides as if they were
    // the user's defaults.
    let cfg = built();
    assert_eq!(
        Settings::from_config_for(&cfg, None, None).insertion_mode,
        "on-release"
    );
}

#[test]
fn identity_comes_from_the_snapshot_not_a_second_lookup() {
    // Focus can move between two accessibility calls. Resolving profiles
    // against a separately-fetched app would apply one app's rules to
    // another app's text, which is the failure the snapshot's own doc
    // comment already warns about for `app`.
    let snap = TextSnapshot {
        app: Some("Terminal".into()),
        bundle_id: Some("com.apple.Terminal".into()),
        ..Default::default()
    };
    let id =
        outloud::inject::app_identity(Some(&snap)).expect("a snapshot with an app has an identity");
    assert_eq!(id.bundle_id.as_deref(), Some("com.apple.Terminal"));
}

#[test]
fn a_bundleless_process_still_matches_by_process_name() {
    // A bare executable run from a shell has no bundle id. That is a real
    // state, not a failure, and `match.process-name` exists for it.
    let snap = TextSnapshot {
        app: Some("nvim".into()),
        bundle_id: None,
        ..Default::default()
    };
    let id = outloud::inject::app_identity(Some(&snap)).expect("a process name is enough");
    assert_eq!(id.bundle_id, None);
    assert_eq!(id.process_name.as_deref(), Some("nvim"));
}

#[test]
fn window_class_is_never_invented_on_macos() {
    // `match.window-class` is an X11/Wayland concept. Filling it with a
    // macOS app name would make that matcher fire on a platform where it
    // means nothing, silently matching profiles the user wrote for Linux.
    let snap = TextSnapshot {
        app: Some("Terminal".into()),
        bundle_id: Some("com.apple.Terminal".into()),
        ..Default::default()
    };
    let id = outloud::inject::app_identity(Some(&snap)).unwrap();
    assert_eq!(id.window_class, None);
}
