//! The menu-bar surface: what the status glyph shows and what the click
//! menu contains, as data.
//!
//! `docs/ux/05-settings-and-states.md` gives the tray glyph a column in the
//! state table, next to the overlay and the terminal cursor color: it is the
//! surface that answers "is it on?" without a click (principle: "a single
//! always-working entry point"). Until now the daemon had no such surface at
//! all, so a running `Aqua.app` was indistinguishable from a crashed one.
//!
//! This module is deliberately platform-neutral and pure:
//!
//! * The **glyph** is a function of [`OverlayState`], so every surface reads
//!   the same state machine rather than inventing a parallel one.
//! * The **menu** is a plain tree of [`MenuItem`]s carrying opaque
//!   [`MenuId`]s. The overlay crate therefore knows nothing about
//!   configuration, permissions, or diagnostics: the host (aquad) builds the
//!   model, maps ids back to its own actions, and stays the only place where
//!   policy lives. That is what keeps this crate compiling headless, where
//!   there is no menu bar to put anything in.

use crate::state::OverlayState;

/// An opaque handle to whatever the host wants to happen when an item is
/// clicked. The platform backend round-trips it untouched (on macOS it
/// travels as the `NSMenuItem` tag), so hosts can use a plain index into
/// their own action table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MenuId(pub u64);

/// One row of the click menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuItem {
    /// Non-clickable text: the status line, the bound hotkey, a config
    /// error. Rendered disabled so it reads as information, not as a
    /// control that does nothing.
    Label(String),
    Separator,
    /// A clickable row. `checked` renders the platform's checkmark, which is
    /// how a settings choice shows its current value.
    Item {
        title: String,
        id: MenuId,
        checked: bool,
        enabled: bool,
    },
    /// A nested menu. Settings live here so the top level stays glanceable
    /// (docs/ux/05: "the first screen must fit on one screen").
    Submenu {
        title: String,
        items: Vec<MenuItem>,
    },
}

impl MenuItem {
    /// A plain enabled, unchecked action.
    pub fn action(title: impl Into<String>, id: MenuId) -> MenuItem {
        MenuItem::Item {
            title: title.into(),
            id,
            checked: false,
            enabled: true,
        }
    }

    /// A settings choice that shows whether it is the current value.
    pub fn choice(title: impl Into<String>, id: MenuId, checked: bool) -> MenuItem {
        MenuItem::Item {
            title: title.into(),
            id,
            checked,
            enabled: true,
        }
    }
}

/// Everything the status item should display right now.
///
/// A whole model, not a set of mutations, for the same reason
/// [`crate::OverlayFrame`] is a whole frame: "what is on screen" stays a
/// pure function of the last model, so there are no stale-row bugs. The
/// backend diffs models and only touches AppKit when something actually
/// changed.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuModel {
    /// Drives the glyph. Kept as the state itself rather than a resolved
    /// icon name so non-macOS backends can pick their own artwork.
    pub state: OverlayState,
    /// Hover text: the one-line answer to "what is it doing?".
    pub tooltip: String,
    pub items: Vec<MenuItem>,
}

impl MenuModel {
    /// Walk every item, including submenu children. Used by hosts to check
    /// their id table is complete, and by tests.
    pub fn ids(&self) -> Vec<MenuId> {
        fn walk(items: &[MenuItem], out: &mut Vec<MenuId>) {
            for item in items {
                match item {
                    MenuItem::Item { id, .. } => out.push(*id),
                    MenuItem::Submenu { items, .. } => walk(items, out),
                    MenuItem::Label(_) | MenuItem::Separator => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, &mut out);
        out
    }
}

/// The SF Symbol name for a state's glyph.
///
/// SF Symbols rather than a bundled image set because they are template
/// images: macOS recolors them for light/dark menu bars and for the
/// highlighted (menu-open) state for free, which a shipped PNG does not get.
/// Names are all present since macOS 13, our LSMinimumSystemVersion; the
/// backend still falls back to text if a lookup ever returns nil, because an
/// invisible status item is exactly the bug this whole module fixes.
///
/// Most states share the waveform silhouette deliberately: the menu bar is a
/// 16pt canvas where a badge is unreadable, so the *colour* carries the
/// state (see [`crate::theme::accent`]) while the mark stays recognizable.
/// Only the two states that want a human get their own shape, because those
/// are the two a colour-blind user must still be able to tell apart.
pub fn sf_symbol(state: OverlayState) -> &'static str {
    match state {
        OverlayState::Error => "waveform.slash",
        // Not a waveform at all: "we are not listening, and you must act".
        OverlayState::NoPermission => "exclamationmark.triangle.fill",
        _ => "waveform",
    }
}

/// The glyph's tint, or `None` to render it as a template (system-colored)
/// image.
///
/// Idle is deliberately untinted: principle 1 is invisible-by-default, and a
/// permanently coloured glyph is an advertisement. Colour appears only when
/// the state is worth noticing, which is what makes the listening blue mean
/// something when it does appear.
pub fn glyph_tint(state: OverlayState) -> Option<crate::theme::Color> {
    match state {
        OverlayState::Listening
        | OverlayState::Transcribing
        | OverlayState::ModelLoading
        | OverlayState::Error
        | OverlayState::NoPermission => Some(crate::theme::accent(state)),
        // Idle, Injecting, DegradedOffline: quiet monochrome. Offline in
        // particular is a supported condition, not an incident.
        _ => None,
    }
}

/// Point size for the status item's symbol configuration.
///
/// A point size, not a bitmap: menu bar height varies with the notch, with
/// HiDPI, and with the user's menu bar size setting, and a fixed bitmap is
/// wrong on most combinations.
pub const GLYPH_POINT_SIZE: f64 = 15.0;

/// A one-character fallback used when SF Symbol lookup fails (a future OS
/// renaming a symbol, or a stripped install). Text in the menu bar is ugly;
/// nothing in the menu bar is a bug.
pub fn fallback_glyph(state: OverlayState) -> &'static str {
    match state {
        OverlayState::Listening => "\u{25cf}",
        OverlayState::Error | OverlayState::NoPermission => "!",
        _ => "\u{25cc}",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_a_glyph_and_a_fallback() {
        for s in OverlayState::ALL {
            assert!(!sf_symbol(s).is_empty(), "{s} has no glyph");
            assert!(!fallback_glyph(s).is_empty(), "{s} has no fallback");
        }
    }

    #[test]
    fn states_needing_action_are_distinguishable_without_colour() {
        // Colour carries most of the state, so the two states that require
        // the user to DO something must also differ in shape: a colour-blind
        // user, or a monochrome menu bar, has nothing else to go on.
        for s in [OverlayState::Error, OverlayState::NoPermission] {
            assert_ne!(
                sf_symbol(s),
                sf_symbol(OverlayState::Idle),
                "{s} is distinguishable from idle only by colour"
            );
        }
    }

    #[test]
    fn idle_is_never_tinted() {
        // Principle 1, invisible-by-default: a coloured idle glyph is a
        // permanent advertisement, and it devalues the listening colour.
        assert!(glyph_tint(OverlayState::Idle).is_none());
        assert!(glyph_tint(OverlayState::DegradedOffline).is_none());
        assert!(glyph_tint(OverlayState::Listening).is_some());
    }

    #[test]
    fn ids_walks_submenus() {
        let model = MenuModel {
            state: OverlayState::Idle,
            tooltip: "idle".into(),
            items: vec![
                MenuItem::Label("Aqua: idle".into()),
                MenuItem::Separator,
                MenuItem::action("Quit", MenuId(1)),
                MenuItem::Submenu {
                    title: "Settings".into(),
                    items: vec![
                        MenuItem::choice("fast", MenuId(2), false),
                        MenuItem::choice("balanced", MenuId(3), true),
                    ],
                },
            ],
        };
        assert_eq!(model.ids(), vec![MenuId(1), MenuId(2), MenuId(3)]);
    }

    #[test]
    fn model_equality_drives_redraw_skipping() {
        // The backend rebuilds the NSMenu only when the model changes, so
        // equality must be structural, not identity.
        let mk = |title: &str| MenuModel {
            state: OverlayState::Idle,
            tooltip: "idle".into(),
            items: vec![MenuItem::action(title, MenuId(1))],
        };
        assert_eq!(mk("Quit"), mk("Quit"));
        assert_ne!(mk("Quit"), mk("Quit Aqua"));
    }
}
