//! The menu-bar surface's *policy*: what the menu says, and what clicking a
//! row does.
//!
//! `overlay::menu` owns the shape of a menu and `overlay::status_item` owns
//! the AppKit plumbing; neither knows what a hotkey or a permission is. This
//! module is the other half: it turns the live engine state plus the loaded
//! configuration into a [`MenuModel`], and turns a clicked [`MenuId`] back
//! into an [`Action`] it then performs.
//!
//! Two decisions worth stating:
//!
//! * **Settings are edits to `config.toml`, not a parallel store.** Every
//!   settings row writes through `config::update_file`, which preserves the
//!   user's comments and formatting, and the daemon re-reads the file. The
//!   menu is therefore exactly what docs/ux/05 promises: "the GUI is a
//!   convenience view over the files". Nothing the menu can do is
//!   unreachable from an editor, and nothing an editor does is invisible to
//!   the menu.
//! * **Ids are positions in the action table built alongside the model.**
//!   The two are produced together by [`build`], so a row and its action
//!   cannot drift apart; the platform layer only ever round-trips an opaque
//!   integer.

use std::path::PathBuf;

use config::schema::Value;
use overlay::menu::{MenuId, MenuItem, MenuModel};
use overlay::OverlayState;

/// What a menu row does when clicked.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Write `key = value` into the user's config file, then reload.
    Set { key: String, value: Value },
    /// Open the config file in the user's editor.
    OpenConfigFile,
    /// Reveal the vocabulary folder in Finder, creating it if needed.
    OpenVocabularyFolder,
    /// Deep-link into a System Settings privacy pane, because "grant
    /// Accessibility" is useless advice without the two clicks that get you
    /// there. `pane` is the `x-apple.systempreferences` anchor.
    OpenPrivacyPane { pane: &'static str },
    /// Run the diagnostics and show the report.
    RunDiagnostics,
    /// Re-read config from disk (also what a settings write does after
    /// writing, and what the user reaches for after editing by hand).
    ReloadConfig,
    /// Quit the daemon.
    Quit,
}

/// A privacy pane anchor. Named constants because a typo here yields a
/// silently-does-nothing menu item.
pub const PANE_ACCESSIBILITY: &str = "Privacy_Accessibility";
pub const PANE_MICROPHONE: &str = "Privacy_Microphone";
/// Input Monitoring. A DIFFERENT grant from Accessibility, and the one the
/// event tap actually needs: without it the hotkey never fires.
pub const PANE_INPUT_MONITORING: &str = "Privacy_ListenEvent";

/// Everything the menu needs to know about the running daemon that is not
/// in the config file.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: OverlayState,
    /// The engine's current detail line, if any ("mic stream died → …").
    pub detail: Option<String>,
    /// The chord actually bound, as the hotkey crate displays it. `None`
    /// when the bind failed, which is a headline fact, not a footnote.
    pub bound_hotkey: Option<String>,
    /// Config problems worth telling the user about (bad key, unparsable
    /// file). docs/ux/05: a broken config "says so in the tray".
    pub config_problems: Vec<String>,
    /// The input device capture is actually using. "Granted, but listening
    /// to the wrong microphone" presents as "it hears nothing", and no other
    /// surface tells the user which device won.
    pub microphone: Option<String>,
    /// Capture could not open a device at all, which is a different fix from
    /// a missing Accessibility grant and needs its own row.
    pub microphone_blocked: bool,
    /// Accessibility trust is missing right now, from a live poll rather
    /// than from whatever was true at launch.
    pub accessibility_blocked: bool,
    /// Input Monitoring is missing, so the hotkey cannot fire at all.
    pub input_monitoring_blocked: bool,
}

/// The settings the menu can change, resolved from the config layers.
///
/// A snapshot struct rather than a borrow of `config::Config` so
/// [`build`] is a pure function of plain data and can be unit-tested
/// without a filesystem.
///
/// `Default` is the schema's defaults, so callers (and tests) can name only
/// the fields they care about. Adding a setting then cannot break every
/// construction site, which is how the last three fields got added without
/// noticing the test fixtures had gone stale.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub hotkey: String,
    pub model: String,
    pub insertion_mode: String,
    pub casing: String,
    pub overlay_position: String,
    /// `microphone.sensitivity`, 1-100. How quiet a voice still registers as
    /// speech.
    pub sensitivity: u8,
    /// `silence-timeout-ms`: the hot-mic safety net.
    pub silence_timeout_ms: i64,
    /// `microphone.warm-hold-ms`: post-commit hold for slow devices.
    pub warm_hold_ms: i64,
    pub enabled: bool,
    pub launch_at_login: bool,
    pub config_path: Option<PathBuf>,
}

impl Default for Status {
    /// Idle, bound, nothing wrong. The shape of a healthy daemon, so a
    /// caller (or a test) only has to name what is unusual about its case.
    fn default() -> Status {
        Status {
            state: OverlayState::Idle,
            detail: None,
            bound_hotkey: Some("right-option".into()),
            config_problems: Vec::new(),
            microphone: None,
            microphone_blocked: false,
            accessibility_blocked: false,
            input_monitoring_blocked: false,
        }
    }
}

impl Default for Settings {
    /// The schema's own defaults, so this cannot drift from a fresh install.
    fn default() -> Settings {
        let s = |key: &str| match config::schema::spec_for(key).map(|k| k.default.clone()) {
            Some(config::schema::Value::Str(v)) => v,
            _ => String::new(),
        };
        let i = |key: &str, fallback: i64| match config::schema::spec_for(key)
            .map(|k| k.default.clone())
        {
            Some(config::schema::Value::Int(v)) => v,
            _ => fallback,
        };
        let b = |key: &str| {
            matches!(
                config::schema::spec_for(key).map(|k| k.default.clone()),
                Some(config::schema::Value::Bool(true))
            )
        };
        Settings {
            hotkey: s("hotkey"),
            model: s("model"),
            insertion_mode: s("insertion.mode"),
            casing: s("formatting.casing"),
            overlay_position: s("overlay.position"),
            sensitivity: i("microphone.sensitivity", 50).clamp(1, 100) as u8,
            silence_timeout_ms: i("silence-timeout-ms", 60_000),
            warm_hold_ms: i("microphone.warm-hold-ms", 0),
            enabled: b("enabled"),
            launch_at_login: b("launch-at-login"),
            config_path: None,
        }
    }
}

impl Settings {
    /// Read the menu-relevant keys out of a loaded config. Keys the schema
    /// guarantees exist, so a missing one is a schema bug, not user input;
    /// we still fall back rather than panic, because a daemon must not die
    /// of a settings read.
    pub fn from_config(cfg: &config::Config, config_path: Option<PathBuf>) -> Settings {
        let s = |key: &str, fallback: &str| match cfg.get(key).map(|p| p.value) {
            Some(Value::Str(v)) => v,
            _ => fallback.to_string(),
        };
        let i = |key: &str, fallback: i64| match cfg.get(key).map(|p| p.value) {
            Some(Value::Int(v)) => v,
            _ => fallback,
        };
        let b = |key: &str, fallback: bool| match cfg.get(key).map(|p| p.value) {
            Some(Value::Bool(v)) => v,
            _ => fallback,
        };
        Settings {
            hotkey: s("hotkey", "right-option"),
            model: s("model", "balanced"),
            insertion_mode: s("insertion.mode", "on-release"),
            casing: s("formatting.casing", "standard"),
            overlay_position: s("overlay.position", "bottom-center"),
            // Clamped, not trusted: this is a hand-edited text file, and an
            // out-of-range value must degrade to the default rather than
            // reach the VAD as a nonsense threshold.
            sensitivity: i("microphone.sensitivity", 50).clamp(1, 100) as u8,
            // Clamped for the same reason as sensitivity: hand-edited file.
            silence_timeout_ms: i("silence-timeout-ms", 60_000).clamp(1_000, 600_000),
            warm_hold_ms: i("microphone.warm-hold-ms", 0).clamp(0, 10_000),
            enabled: b("enabled", true),
            launch_at_login: b("launch-at-login", false),
            config_path,
        }
    }
}

/// Build the menu and the action table for it. The `MenuId` of every row is
/// its index in the returned table, which is what keeps the two in step.
pub fn build(status: &Status, settings: &Settings) -> (MenuModel, Vec<Action>) {
    let mut actions: Vec<Action> = Vec::new();
    let mut items: Vec<MenuItem> = Vec::new();
    // Small closure so every clickable row registers its action in the same
    // breath as it is created; an id can never point at the wrong action.
    let add = |actions: &mut Vec<Action>, action: Action| -> MenuId {
        actions.push(action);
        MenuId(actions.len() as u64 - 1)
    };

    items.push(MenuItem::Label(status_line(status)));
    if let Some(detail) = &status.detail {
        items.push(MenuItem::Label(format!("   {detail}")));
    }

    // The permission story goes directly under the status line, because when
    // it is broken nothing else in this menu matters. Two grants, two rows,
    // each shown only when that one is actually failing: a permanent list of
    // things to go fix is nagging, and principle 1 is invisible-by-default.
    if status.state == OverlayState::NoPermission
        || status.bound_hotkey.is_none()
        || status.accessibility_blocked
    {
        items.push(MenuItem::Separator);
        items.push(MenuItem::Label(
            "OutLoud needs Accessibility to see the hotkey and write text.".into(),
        ));
        let id = add(
            &mut actions,
            Action::OpenPrivacyPane {
                pane: PANE_ACCESSIBILITY,
            },
        );
        items.push(MenuItem::action("Open Accessibility Settings…", id));
    }
    // Input Monitoring first among the permission rows, because it is the
    // one that stops the product working outright: without it the hotkey
    // produces nothing at all, whereas a missing Accessibility grant still
    // dictates via clipboard paste.
    if status.input_monitoring_blocked {
        items.push(MenuItem::Separator);
        items.push(MenuItem::Label(
            "OutLoud needs Input Monitoring to see the hotkey.".into(),
        ));
        let id = add(
            &mut actions,
            Action::OpenPrivacyPane {
                pane: PANE_INPUT_MONITORING,
            },
        );
        items.push(MenuItem::action("Open Input Monitoring Settings…", id));
    }
    if status.microphone_blocked {
        items.push(MenuItem::Label(
            "OutLoud cannot open the microphone.".into(),
        ));
        let id = add(
            &mut actions,
            Action::OpenPrivacyPane {
                pane: PANE_MICROPHONE,
            },
        );
        items.push(MenuItem::action("Open Microphone Settings…", id));
    }

    items.push(MenuItem::Separator);
    match &status.bound_hotkey {
        Some(chord) => {
            items.push(MenuItem::Label(format!("Hold {chord} to dictate")));
            // The event tap binds once, at launch. Changing the hotkey
            // rewrites the file but not the live binding, so a settings row
            // that appears to have done nothing until some future restart
            // is worse than one that admits it. Comparing the live bind to
            // the configured value catches this however it happened: the
            // menu, a hand edit, or an OUTLOUD_HOTKEY (or legacy AQUA_HOTKEY) override.
            if !same_chord(chord, &settings.hotkey) {
                items.push(MenuItem::Label(format!(
                    "   \"{}\" takes effect after Quit and reopen",
                    settings.hotkey
                )));
            }
        }
        // A dead hotkey is a dead product, so say so here rather than only
        // in stderr nobody is reading.
        None => items.push(MenuItem::Label(format!(
            "Hotkey {} is NOT bound",
            settings.hotkey
        ))),
    }

    for problem in &status.config_problems {
        items.push(MenuItem::Label(format!("Config: {problem}")));
    }

    // The input device, not just the hotkey: "permission granted but the
    // wrong microphone is selected" is a real state a user cannot otherwise
    // diagnose, and it presents as "it hears nothing" (docs/ux/01).
    if let Some(device) = &status.microphone {
        items.push(MenuItem::Label(format!("Microphone: {device}")));
    }

    items.push(MenuItem::Separator);
    // Pause is the highest-frequency action in the whole menu ("get out of
    // the way for a minute"), so it is one click at the top level rather
    // than two inside Settings. It is also the honest alternative to a
    // second Quit item: there is exactly one Quit, and it stops everything.
    // The negation: the row is a switch, so clicking it must change the
    // value. Writing the CURRENT value here made the row a silent no-op,
    // which is the exact failure this menu exists to avoid -- and it got
    // past a unit test that asserted the same wrong thing, so the assertion
    // below now derives from the checkmark rather than restating the code.
    actions.push(Action::Set {
        key: "enabled".into(),
        value: Value::Bool(!settings.enabled),
    });
    items.push(MenuItem::choice(
        "Pause Dictation",
        MenuId(actions.len() as u64 - 1),
        !settings.enabled,
    ));

    items.push(MenuItem::Separator);
    items.push(MenuItem::Submenu {
        title: "Settings".into(),
        items: settings_menu(&mut actions, settings),
    });

    let id = add(&mut actions, Action::OpenConfigFile);
    items.push(MenuItem::Item {
        // Not the full path: a long home directory blows the menu out to
        // half the screen. The path is stated in the docs and one row below
        // in the file-problem lines when it matters.
        title: "Edit Config File…".into(),
        id,
        checked: false,
        enabled: settings.config_path.is_some(),
    });
    let id = add(&mut actions, Action::OpenVocabularyFolder);
    items.push(MenuItem::action("Open Vocabulary Folder…", id));

    items.push(MenuItem::Separator);
    let id = add(&mut actions, Action::RunDiagnostics);
    items.push(MenuItem::action("Run Diagnostics…", id));
    let id = add(&mut actions, Action::ReloadConfig);
    items.push(MenuItem::action("Reload Config", id));

    items.push(MenuItem::Separator);
    let id = add(&mut actions, Action::Quit);
    items.push(MenuItem::action("Quit OutLoud", id));

    // The glyph must answer "is it on?" without a click, which is the whole
    // reason the tray surface exists. A daemon whose Accessibility grant was
    // revoked is NOT ready, however healthy the pipeline's own state machine
    // believes itself to be: the engine has no idea the permission moved, so
    // the live poll overrides it here rather than faking a state transition
    // the engine's table would reject.
    let glyph_state = if (status.accessibility_blocked || status.input_monitoring_blocked)
        && status.state == OverlayState::Idle
    {
        OverlayState::NoPermission
    } else {
        status.state
    };
    let model = MenuModel {
        state: glyph_state,
        tooltip: status_line(status),
        items,
    };
    (model, actions)
}

/// The Settings submenu: the handful of keys worth a click, each shown with
/// its current value checked. Everything else stays in the file, per
/// docs/ux/05 ("the first screen must fit on one screen").
fn settings_menu(actions: &mut Vec<Action>, s: &Settings) -> Vec<MenuItem> {
    let mut items = Vec::new();
    let choice_group = |actions: &mut Vec<Action>,
                        items: &mut Vec<MenuItem>,
                        title: &str,
                        key: &str,
                        options: &[(&str, &str)],
                        current: &str| {
        items.push(MenuItem::Label(title.to_string()));
        for (value, label) in options {
            actions.push(Action::Set {
                key: key.to_string(),
                value: Value::Str((*value).to_string()),
            });
            let id = MenuId(actions.len() as u64 - 1);
            items.push(MenuItem::choice(
                format!("   {label}"),
                id,
                *value == current,
            ));
        }
        items.push(MenuItem::Separator);
    };

    // Hotkey presets only: a chord *recorder* needs a key-capture window and
    // is a separate piece of work. These four cover the documented
    // recommendations, and anything else is one line in the config file.
    choice_group(
        actions,
        &mut items,
        "Hotkey",
        "hotkey",
        &[
            ("right-option", "Right Option"),
            ("right-command", "Right Command"),
            ("fn", "Fn / Globe"),
            ("f13", "F13"),
        ],
        &s.hotkey,
    );
    // Microphone sensitivity. Named steps, not a 1-100 spinner: the number
    // is meaningless to a user, but "I'm sitting back from the mic" is
    // exactly how the problem gets described. Values are the dial positions
    // those descriptions map to.
    //
    // This is the one setting users reach for when dictation "just doesn't
    // hear me", so it is in the menu rather than config-file-only.
    items.push(MenuItem::Label("Microphone Sensitivity".to_string()));
    // Steps come from the audio crate, which owns the knee mapping they
    // index into. Restating them here drifted once already: the diagnostic
    // kept recommending a setting the menu no longer offered.
    for (value, label) in audio::vad::SENSITIVITY_STEPS {
        actions.push(Action::Set {
            key: "microphone.sensitivity".into(),
            value: Value::Int(value as i64),
        });
        let id = MenuId(actions.len() as u64 - 1);
        // Nearest step wins the checkmark, so a hand-edited 63 still shows
        // a checked row instead of an all-unchecked group that looks broken.
        let nearest = audio::vad::SENSITIVITY_STEPS
            .into_iter()
            .min_by_key(|(v, _)| v.abs_diff(s.sensitivity))
            .map(|(v, _)| v)
            .unwrap();
        items.push(MenuItem::choice(
            format!("   {label}"),
            id,
            value == nearest,
        ));
    }
    items.push(MenuItem::Separator);

    // Overlay: a switch, not the five-way position picker the schema
    // allows, because "hidden" is the only position value the renderer can
    // honor today. Offering the other four would be four rows that write a
    // key and change nothing.
    let hidden = s.overlay_position == "hidden";
    actions.push(Action::Set {
        key: "overlay.position".into(),
        value: Value::Str(if hidden { "bottom-center" } else { "hidden" }.into()),
    });
    items.push(MenuItem::choice(
        "Show Floating Overlay",
        MenuId(actions.len() as u64 - 1),
        !hidden,
    ));
    items.push(MenuItem::Separator);

    // Deliberately absent: Model, Language, Insertion mode, Casing, Smart
    // Quotes, Trailing Punctuation, History, Vocabulary Sets, and Launch at
    // Login. Every one of those keys exists in the schema and NOTHING in the
    // pipeline reads it yet. A settings row that writes a key no code
    // consumes is a lie told by the UI, and it is a worse lie than an absent
    // row because the user believes they changed something. Each comes back
    // the day it is wired; until then config.toml and docs/configuration.md
    // are the honest place for them, where their status is at least visible
    // alongside the rest of the file.

    // `enabled` is deliberately NOT here: it is the top-level Pause row, and
    // one setting reachable from two rows is how a menu starts disagreeing
    // with itself. Toggles write the negation of the current value, so the
    // row behaves as a switch.
    items
}

/// Whether a bound chord and a configured chord are the same binding.
///
/// String comparison is not enough: the hotkey crate normalizes on display
/// (`Chord: Display` reorders modifiers and canonicalizes spellings), so
/// `"right-alt"` in the file and `"right-option"` from the tap are the same
/// key. Parsing both is what makes "takes effect after restart" appear only
/// when the binding genuinely differs, instead of on every launch.
fn same_chord(bound: &str, configured: &str) -> bool {
    match (
        bound.parse::<hotkey::Chord>(),
        configured.parse::<hotkey::Chord>(),
    ) {
        (Ok(a), Ok(b)) => a == b,
        // An unparsable configured chord is already reported as a config
        // error; do not also claim a restart would help.
        _ => true,
    }
}
/// The one-line answer to "what is it doing?", used for both the top menu
/// row and the status item's tooltip.
fn status_line(status: &Status) -> String {
    // Same override as the glyph: "ready" is a lie when the permission that
    // makes dictation work has been taken away.
    if status.state == OverlayState::Idle {
        // Name the specific grant. "Permission needed" sends a user to the
        // wrong pane, which is precisely how this bug wasted an afternoon.
        if status.input_monitoring_blocked {
            return "OutLoud: Input Monitoring permission needed".into();
        }
        if status.accessibility_blocked {
            return "OutLoud: Accessibility permission needed".into();
        }
    }
    match status.state {
        OverlayState::Idle => "OutLoud: ready".into(),
        OverlayState::Listening => "OutLoud: listening".into(),
        OverlayState::Transcribing => "OutLoud: transcribing…".into(),
        OverlayState::Injecting => "OutLoud: inserting text".into(),
        OverlayState::Error => "OutLoud: error".into(),
        OverlayState::NoPermission => "OutLoud: permission needed".into(),
        OverlayState::ModelLoading => "OutLoud: loading model…".into(),
        OverlayState::DegradedOffline => "OutLoud: ready (offline)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            config_path: Some(PathBuf::from("/home/u/.config/outloud/config.toml")),
            ..Settings::default()
        }
    }

    fn status(state: OverlayState) -> Status {
        Status {
            state,
            ..Status::default()
        }
    }

    #[test]
    fn every_id_indexes_its_own_action() {
        // The whole safety argument for opaque ids: id == index, densely,
        // with no gaps. A gap would silently fire the wrong action.
        let (model, actions) = build(&status(OverlayState::Idle), &settings());
        let ids = model.ids();
        assert_eq!(ids.len(), actions.len());
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(*id, MenuId(i as u64), "ids must be dense and in order");
        }
    }

    #[test]
    fn the_menu_always_offers_a_way_out() {
        // Quit is the difference between an app and a process you have to
        // `killall`. It must exist in every state.
        for state in OverlayState::ALL {
            let (_, actions) = build(&status(state), &settings());
            assert!(actions.contains(&Action::Quit), "{state} menu has no Quit");
        }
    }

    #[test]
    fn missing_permission_surfaces_the_fix_not_just_the_problem() {
        let (model, actions) = build(&status(OverlayState::NoPermission), &settings());
        assert!(actions.contains(&Action::OpenPrivacyPane {
            pane: PANE_ACCESSIBILITY
        }));
        let text = format!("{:?}", model.items);
        assert!(text.contains("Accessibility"), "{text}");
    }

    #[test]
    fn a_failed_hotkey_bind_is_stated_in_the_menu() {
        // stderr is invisible in a bundled launch, so the menu is the only
        // place a user can learn their hotkey never bound.
        let mut st = status(OverlayState::Idle);
        st.bound_hotkey = None;
        let (model, actions) = build(&st, &settings());
        let text = format!("{:?}", model.items);
        assert!(text.contains("NOT bound"), "{text}");
        assert!(actions.contains(&Action::OpenPrivacyPane {
            pane: PANE_ACCESSIBILITY
        }));
    }

    #[test]
    fn the_bound_chord_comes_from_the_daemon_not_the_file() {
        // The file can say one thing while the running bind says another
        // (--chord, a failed rebind). The menu must report reality.
        let mut st = status(OverlayState::Idle);
        st.bound_hotkey = Some("f13".into());
        let (model, _) = build(&st, &settings());
        let text = format!("{:?}", model.items);
        assert!(text.contains("Hold f13"), "{text}");
    }

    #[test]
    fn current_settings_are_the_checked_ones() {
        // The overlay switch reads as "shown", so a hidden overlay is an
        // UNchecked row whose click restores it.
        for hidden in [true, false] {
            let s = Settings {
                overlay_position: if hidden { "hidden" } else { "bottom-center" }.into(),
                ..settings()
            };
            let (model, actions) = build(&status(OverlayState::Idle), &s);
            let (id, checked) =
                find_row(&model, "Show Floating Overlay").expect("the overlay switch must exist");
            assert_eq!(checked, !hidden);
            assert_eq!(
                actions[id.0 as usize],
                Action::Set {
                    key: "overlay.position".into(),
                    value: Value::Str(if hidden { "bottom-center" } else { "hidden" }.into())
                }
            );
        }
    }

    #[test]
    fn pause_is_one_click_and_writes_the_negation() {
        // Pause is the most-used row in the menu; two clicks (into a
        // submenu) is the wrong cost. It must also behave as a switch, so
        // its action is always the opposite of the current value.
        for enabled in [true, false] {
            let s = Settings {
                enabled,
                ..settings()
            };
            let (model, actions) = build(&status(OverlayState::Idle), &s);
            let (id, checked) = model
                .items
                .iter()
                .find_map(|i| match i {
                    MenuItem::Item {
                        title, id, checked, ..
                    } if title == "Pause Dictation" => Some((*id, *checked)),
                    _ => None,
                })
                .expect("Pause must be a TOP-LEVEL row, not buried in Settings");
            assert_eq!(checked, !enabled, "the checkmark means paused");
            // Derived from the row's own displayed state, not restated from
            // the implementation: a switch must write the value it is not
            // currently showing, or clicking it does nothing.
            let Action::Set { key, value } = &actions[id.0 as usize] else {
                panic!("the pause row must write a setting");
            };
            assert_eq!(key, "enabled");
            assert_eq!(
                *value,
                Value::Bool(checked),
                "clicking a switch must flip it, not rewrite its current value"
            );
        }
    }

    #[test]
    fn only_implemented_settings_are_offered() {
        // A row that writes a key nothing in the pipeline reads is a lie:
        // the user believes they changed something and nothing happens.
        // The source of truth is the schema's own `wired` flag rather than a
        // list here, so wiring a setting and offering it are the same edit
        // and this gate cannot go stale.
        let (_, actions) = build(&status(OverlayState::Idle), &settings());
        for action in &actions {
            if let Action::Set { key, .. } = action {
                let spec = config::schema::spec_for(key)
                    .unwrap_or_else(|| panic!("the menu offers unknown key \"{key}\""));
                assert!(
                    spec.wired,
                    "the menu offers \"{key}\", which no code reads yet; \
                     wire it or drop the row"
                );
            }
        }
    }

    #[test]
    fn missing_input_monitoring_names_its_own_pane() {
        // The bug this test exists for: Input Monitoring is what the event
        // tap actually needs, and a daemon missing it binds nothing while
        // reporting itself ready. Sending the user to the Accessibility
        // pane, which they may already have granted, is worse than silence.
        let mut st = status(OverlayState::Idle);
        st.input_monitoring_blocked = true;
        let (model, actions) = build(&st, &settings());
        assert!(actions.contains(&Action::OpenPrivacyPane {
            pane: PANE_INPUT_MONITORING
        }));
        assert!(
            model.tooltip.contains("Input Monitoring"),
            "the tooltip must name the grant that is actually missing: {}",
            model.tooltip
        );
        assert_eq!(
            model.state,
            OverlayState::NoPermission,
            "the glyph must not read healthy while the hotkey cannot fire"
        );
    }

    #[test]
    fn the_two_permissions_are_never_conflated() {
        // They have different panes and different consequences: without
        // Input Monitoring nothing happens at all, while without
        // Accessibility dictation still works via clipboard paste.
        assert_ne!(PANE_INPUT_MONITORING, PANE_ACCESSIBILITY);

        let mut st = status(OverlayState::Idle);
        st.input_monitoring_blocked = true;
        let (_, actions) = build(&st, &settings());
        assert!(!actions.contains(&Action::OpenPrivacyPane {
            pane: PANE_ACCESSIBILITY
        }));
    }

    #[test]
    fn revoked_accessibility_surfaces_while_running() {
        // macOS revokes silently, and nothing else in the product would tell
        // the user: the daemon keeps running, the glyph stays healthy, and
        // dictation quietly degrades. The live poll writes this flag, and it
        // must produce both the explanation and the way to fix it.
        let mut st = status(OverlayState::Idle);
        st.accessibility_blocked = true;
        let (model, actions) = build(&st, &settings());
        assert!(actions.contains(&Action::OpenPrivacyPane {
            pane: PANE_ACCESSIBILITY
        }));
        assert!(format!("{:?}", model.items).contains("Accessibility"));
    }

    #[test]
    fn a_revoked_grant_changes_the_glyph_not_just_the_menu() {
        // The glyph is the surface a user reads without clicking, so a
        // revoked permission that only shows up inside the menu is a daemon
        // that still LOOKS fine while dictation is broken.
        let mut st = status(OverlayState::Idle);
        st.accessibility_blocked = true;
        let (model, _) = build(&st, &settings());
        assert_eq!(model.state, OverlayState::NoPermission);
        assert!(model.tooltip.contains("Accessibility"), "{}", model.tooltip);
    }

    #[test]
    fn a_live_utterance_is_never_interrupted_by_the_permission_glyph() {
        // The override applies only to Idle. Overriding Listening would
        // replace the mic-hot glyph mid-utterance, which is the one state
        // the user must be able to trust absolutely.
        let mut st = status(OverlayState::Listening);
        st.accessibility_blocked = true;
        let (model, _) = build(&st, &settings());
        assert_eq!(model.state, OverlayState::Listening);
    }

    #[test]
    fn regained_accessibility_stops_nagging() {
        // The opposite direction, and the more common one: the quickstart
        // tells users to grant the permission with OutLoud already running. The
        // row must disappear on its own, or they learn the menu lies.
        let st = status(OverlayState::Idle); // trusted, hotkey bound
        let (model, _) = build(&st, &settings());
        assert!(
            !format!("{:?}", model.items).contains("Accessibility"),
            "a healthy daemon must not show a permission row"
        );
    }

    #[test]
    fn a_wrong_microphone_is_visible_and_a_blocked_one_is_actionable() {
        // "Permission granted, wrong input device" presents exactly like a
        // broken microphone, and nothing else in the product names the
        // device that actually won.
        let mut st = status(OverlayState::Idle);
        st.microphone = Some("Jessie's AirPods".into());
        let (model, _) = build(&st, &settings());
        assert!(format!("{:?}", model.items).contains("Jessie's AirPods"));

        // A blocked microphone is a different fix from Accessibility, so it
        // gets its own row and its own pane.
        let mut st = status(OverlayState::Idle);
        st.microphone_blocked = true;
        let (_, actions) = build(&st, &settings());
        assert!(actions.contains(&Action::OpenPrivacyPane {
            pane: PANE_MICROPHONE
        }));
    }

    #[test]
    fn a_hotkey_change_admits_it_needs_a_restart() {
        // The event tap binds once, at launch. Saving a new chord rewrites
        // the file and changes nothing until restart; saying so is the
        // difference between a setting and a lie.
        let mut s = settings();
        s.hotkey = "f13".into();
        let st = status(OverlayState::Idle); // still bound to right-option
        let (model, _) = build(&st, &s);
        let text = format!("{:?}", model.items);
        assert!(text.contains("takes effect after Quit"), "{text}");
    }

    #[test]
    fn an_unchanged_hotkey_says_nothing_about_restarting() {
        // Display normalization must not make every launch nag: the tap
        // reports "right-option" for a file that says "right-alt".
        let mut s = settings();
        s.hotkey = "right-alt".into();
        let (model, _) = build(&status(OverlayState::Idle), &s);
        let text = format!("{:?}", model.items);
        assert!(!text.contains("takes effect after Quit"), "{text}");
    }

    #[test]
    fn every_settings_write_is_a_real_schema_key_with_a_valid_value() {
        // A menu row that writes an invalid value would be rejected by
        // update_file at click time — i.e. a dead button discovered by the
        // user. Prove the whole table is writable instead.
        let (_, actions) = build(&status(OverlayState::Idle), &settings());
        for action in &actions {
            if let Action::Set { key, value } = action {
                config::update_file("", key, value)
                    .unwrap_or_else(|e| panic!("menu row writes an invalid setting: {e}"));
            }
        }
    }

    #[test]
    fn inert_settings_reach_the_user_through_the_menu() {
        // A bundled app has no terminal, so stderr is invisible: the menu is
        // the only place a user can learn that a setting they wrote does
        // nothing. The host puts these in `config_problems`; this pins that
        // they are actually rendered.
        let mut st = status(OverlayState::Idle);
        st.config_problems
            .push("\"microphone\" is set but not implemented yet; it has no effect".into());
        let (model, _) = build(&st, &settings());
        let text = format!("{:?}", model.items);
        assert!(text.contains("no effect"), "{text}");
        assert!(text.contains("microphone"), "{text}");
    }

    #[test]
    fn config_problems_reach_the_user() {
        let mut st = status(OverlayState::Idle);
        st.config_problems
            .push("unknown setting \"hotkye\"; did you mean \"hotkey\"?".into());
        let (model, _) = build(&st, &settings());
        assert!(format!("{:?}", model.items).contains("hotkye"));
    }

    /// The (id, checked) of a row by title, searching submenus too.
    fn find_row(model: &MenuModel, title: &str) -> Option<(MenuId, bool)> {
        fn walk(items: &[MenuItem], title: &str) -> Option<(MenuId, bool)> {
            for item in items {
                match item {
                    MenuItem::Item {
                        title: t,
                        id,
                        checked,
                        ..
                    } if t == title => return Some((*id, *checked)),
                    MenuItem::Submenu { items, .. } => {
                        if let Some(hit) = walk(items, title) {
                            return Some(hit);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&model.items, title)
    }
}

#[cfg(test)]
mod sensitivity_menu_tests {
    use super::*;

    fn actions_for(sensitivity: u8) -> Vec<Action> {
        let settings = Settings {
            sensitivity,
            ..Settings::default()
        };
        build(&Status::default(), &settings).1
    }

    /// The menu's ceiling is a measurement, not a taste: swept against a
    /// synthetic quiet room (~0.0002 RMS), sensitivity 75 and above starts
    /// scoring pure noise as speech. Offering a step that invents text
    /// would poison the feature it is meant to fix, so the menu stops one
    /// step below at 70.
    ///
    /// This bound moves whenever the knee anchor moves, which is exactly
    /// why it is asserted rather than remembered.
    ///
    /// See scripts/sweep-sensitivity.sh to re-derive this.
    #[test]
    fn the_menu_does_not_offer_a_hallucinating_sensitivity() {
        for action in actions_for(50) {
            if let Action::Set { key, value } = action {
                if key == "microphone.sensitivity" {
                    let Value::Int(n) = value else {
                        panic!("want an int")
                    };
                    assert!(
                        n <= 70,
                        "sensitivity {n} transcribes room noise; the menu must not offer it"
                    );
                }
            }
        }
    }

    #[test]
    fn every_offered_value_passes_schema_validation() {
        // A menu row that writes a value the config layer then rejects is
        // worse than a missing row: it looks like it worked and silently
        // does not survive a reload.
        let spec = config::schema::spec_for("microphone.sensitivity")
            .expect("sensitivity must be in the schema");
        for action in actions_for(50) {
            if let Action::Set { key, value } = action {
                if key == "microphone.sensitivity" {
                    assert!(
                        spec.constraint.check(&value).is_ok(),
                        "menu offers {value:?}, which the schema rejects"
                    );
                }
            }
        }
    }

    #[test]
    fn exactly_one_row_is_checked_for_an_off_step_value() {
        // A hand-edited 63 is not one of the four presets. The group must
        // still show one checkmark, or the menu reads as broken.
        let settings = Settings {
            sensitivity: 63,
            ..Settings::default()
        };
        let (model, actions) = build(&Status::default(), &settings);
        let checked = checked_sensitivity_values(&model, &actions);
        assert_eq!(
            checked.len(),
            1,
            "expected exactly one checked row, got {checked:?}"
        );
    }

    #[test]
    fn the_checked_row_is_the_one_that_matches_the_setting() {
        let settings = Settings {
            sensitivity: 70,
            ..Settings::default()
        };
        let (model, actions) = build(&Status::default(), &settings);
        assert_eq!(
            checked_sensitivity_values(&model, &actions),
            vec![Value::Int(70)]
        );
    }

    /// Walk the menu (including submenus) collecting the values of checked
    /// rows whose action writes the sensitivity key.
    fn checked_sensitivity_values(model: &MenuModel, actions: &[Action]) -> Vec<Value> {
        fn walk(items: &[MenuItem], actions: &[Action], out: &mut Vec<Value>) {
            for item in items {
                match item {
                    MenuItem::Submenu { items, .. } => walk(items, actions, out),
                    MenuItem::Item {
                        id, checked: true, ..
                    } => {
                        if let Some(Action::Set { key, value }) = actions.get(id.0 as usize) {
                            if key == "microphone.sensitivity" {
                                out.push(value.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&model.items, actions, &mut out);
        out
    }
}
