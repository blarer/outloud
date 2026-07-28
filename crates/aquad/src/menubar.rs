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
}

/// The settings the menu can change, resolved from the config layers.
///
/// A snapshot struct rather than a borrow of `config::Config` so
/// [`build`] is a pure function of plain data and can be unit-tested
/// without a filesystem.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub hotkey: String,
    pub model: String,
    pub insertion_mode: String,
    pub casing: String,
    pub overlay_position: String,
    pub enabled: bool,
    pub launch_at_login: bool,
    pub config_path: Option<PathBuf>,
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
    // it is broken nothing else in this menu matters.
    if status.state == OverlayState::NoPermission || status.bound_hotkey.is_none() {
        items.push(MenuItem::Separator);
        items.push(MenuItem::Label(
            "Aqua needs Accessibility to see the hotkey and write text.".into(),
        ));
        let id = add(
            &mut actions,
            Action::OpenPrivacyPane {
                pane: PANE_ACCESSIBILITY,
            },
        );
        items.push(MenuItem::action("Open Accessibility settings…", id));
    }

    items.push(MenuItem::Separator);
    match &status.bound_hotkey {
        Some(chord) => items.push(MenuItem::Label(format!("Hold {chord} to dictate"))),
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

    items.push(MenuItem::Separator);
    items.push(MenuItem::Submenu {
        title: "Settings".into(),
        items: settings_menu(&mut actions, settings),
    });

    let id = add(&mut actions, Action::OpenConfigFile);
    items.push(MenuItem::Item {
        title: match &settings.config_path {
            Some(p) => format!("Edit {}…", p.display()),
            None => "Edit config file…".into(),
        },
        id,
        checked: false,
        enabled: settings.config_path.is_some(),
    });
    let id = add(&mut actions, Action::OpenVocabularyFolder);
    items.push(MenuItem::action("Open vocabulary folder…", id));

    items.push(MenuItem::Separator);
    let id = add(&mut actions, Action::RunDiagnostics);
    items.push(MenuItem::action("Run diagnostics…", id));
    let id = add(&mut actions, Action::ReloadConfig);
    items.push(MenuItem::action("Reload configuration", id));

    items.push(MenuItem::Separator);
    let id = add(&mut actions, Action::Quit);
    items.push(MenuItem::action("Quit Aqua", id));

    let model = MenuModel {
        state: status.state,
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
    choice_group(
        actions,
        &mut items,
        "Model",
        "model",
        &[
            ("fast", "Fast"),
            ("balanced", "Balanced"),
            ("accurate", "Accurate"),
        ],
        &s.model,
    );
    choice_group(
        actions,
        &mut items,
        "Insertion",
        "insertion.mode",
        &[
            ("on-release", "Insert when I release"),
            ("stream", "Stream words as I speak"),
        ],
        &s.insertion_mode,
    );
    choice_group(
        actions,
        &mut items,
        "Casing",
        "formatting.casing",
        &[
            ("standard", "Standard"),
            ("casual-lowercase", "casual lowercase"),
        ],
        &s.casing,
    );
    choice_group(
        actions,
        &mut items,
        "Overlay",
        "overlay.position",
        &[
            ("bottom-center", "Bottom center"),
            ("bottom-left", "Bottom left"),
            ("bottom-right", "Bottom right"),
            ("top-center", "Top center"),
            ("hidden", "Hidden"),
        ],
        &s.overlay_position,
    );

    // Toggles write the negation of the current value, so the row is a
    // switch rather than two rows that disagree.
    actions.push(Action::Set {
        key: "enabled".into(),
        value: Value::Bool(!s.enabled),
    });
    items.push(MenuItem::choice(
        "Dictation enabled",
        MenuId(actions.len() as u64 - 1),
        s.enabled,
    ));
    actions.push(Action::Set {
        key: "launch-at-login".into(),
        value: Value::Bool(!s.launch_at_login),
    });
    items.push(MenuItem::choice(
        "Launch at login",
        MenuId(actions.len() as u64 - 1),
        s.launch_at_login,
    ));

    items
}

/// The one-line answer to "what is it doing?", used for both the top menu
/// row and the status item's tooltip.
fn status_line(status: &Status) -> String {
    match status.state {
        OverlayState::Idle => "Aqua: ready".into(),
        OverlayState::Listening => "Aqua: listening".into(),
        OverlayState::Transcribing => "Aqua: transcribing…".into(),
        OverlayState::Injecting => "Aqua: inserting text".into(),
        OverlayState::Error => "Aqua: error".into(),
        OverlayState::NoPermission => "Aqua: permission needed".into(),
        OverlayState::ModelLoading => "Aqua: loading model…".into(),
        OverlayState::DegradedOffline => "Aqua: ready (offline)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            hotkey: "right-option".into(),
            model: "balanced".into(),
            insertion_mode: "on-release".into(),
            casing: "standard".into(),
            overlay_position: "bottom-center".into(),
            enabled: true,
            launch_at_login: false,
            config_path: Some(PathBuf::from("/home/u/.config/aqua/config.toml")),
        }
    }

    fn status(state: OverlayState) -> Status {
        Status {
            state,
            detail: None,
            bound_hotkey: Some("right-option".into()),
            config_problems: Vec::new(),
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
        let mut s = settings();
        s.model = "accurate".into();
        s.enabled = false;
        let (model, actions) = build(&status(OverlayState::Idle), &s);
        let checked: Vec<&Action> = model
            .ids()
            .into_iter()
            .filter(|id| checked_in(&model, *id))
            .map(|id| &actions[id.0 as usize])
            .collect();
        assert!(checked.contains(&&Action::Set {
            key: "model".into(),
            value: Value::Str("accurate".into())
        }));
        // A toggle's action is the *negation*: clicking "enabled" while off
        // must turn it on.
        assert!(actions.contains(&Action::Set {
            key: "enabled".into(),
            value: Value::Bool(true)
        }));
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
    fn config_problems_reach_the_user() {
        let mut st = status(OverlayState::Idle);
        st.config_problems
            .push("unknown setting \"hotkye\"; did you mean \"hotkey\"?".into());
        let (model, _) = build(&st, &settings());
        assert!(format!("{:?}", model.items).contains("hotkye"));
    }

    fn checked_in(model: &MenuModel, want: MenuId) -> bool {
        fn walk(items: &[MenuItem], want: MenuId) -> bool {
            items.iter().any(|item| match item {
                MenuItem::Item { id, checked, .. } => *id == want && *checked,
                MenuItem::Submenu { items, .. } => walk(items, want),
                _ => false,
            })
        }
        walk(&model.items, want)
    }
}
