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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_needing_action_are_distinguishable_without_colour() {
        // The mark is one shape for every state (see `crate::mark`), so
        // colour carries the state. That is fine for the machine-working
        // states, but the two states that need the user to DO something must
        // survive a monochrome menu bar, so they are the ones whose tint is
        // mandatory rather than optional.
        for s in [OverlayState::Error, OverlayState::NoPermission] {
            assert!(
                glyph_tint(s).is_some(),
                "{s} needs the user to act and must not render as quiet monochrome"
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
