//! Turning a spoken phrase into an application launch or switch.
//!
//! This is the smallest useful slice of the command-mode proposal in
//! `docs/plans/command-mode.md`, and deliberately the least dangerous one.
//! Two verbs, one noun, nothing else:
//!
//! ```text
//! "open Firefox"       -> Launch { app: <registry entry for Firefox> }
//! "switch to Slack"    -> Launch { app: <registry entry for Slack> }
//! ```
//!
//! # Why launching is the safe slice
//!
//! The proposal's own risk section is blunt about where the danger is: a
//! misheard command "does something, and the blast radius is not a field".
//! Launching is the one action in the whole design space that does not have
//! that problem. Opening the wrong application is **trivially reversible**
//! (close the window), it destroys nothing, it sends nothing, and it spends
//! no money. Every other command verb considered (send, delete, move to
//! inbox) fails at least one of those.
//!
//! So this crate can exist before the confirmation machinery, the per-app
//! verb registry, and the irreversibility tracking that the larger feature
//! needs. Those are not skipped, they are *not yet required*, and the
//! enum below is shaped so adding them is additive rather than a rewrite.
//!
//! # Launch and switch are the same operation, which is why this is portable
//!
//! Measured rather than assumed, on 2026-10-30:
//!
//! - **macOS**: `open -a Messages` on an already-running Messages raised it
//!   to the foreground with the **same PID** (17123 before and after). It
//!   did not start a second copy, and it needed no Accessibility grant,
//!   unlike the System Events `set frontmost` route.
//! - **Linux**: the desktop-entry launchers (`gio launch`, `gtk-launch`)
//!   hand activation to the application, and an app that implements
//!   `org.freedesktop.Application` or GApplication single-instance raises
//!   its existing window. 19 of 144 entries on the test box declare
//!   `DBusActivatable=true` explicitly; the rest rely on their own toolkit
//!   logic.
//! - **Windows**: `Start-Process` on a single-instance application has the
//!   same effect for the same reason.
//!
//! That convergence is the finding that makes this feature portable at all,
//! and it is worth stating plainly because the obvious alternative is not.
//! Explicit window raising is **not** uniformly available: Windows refuses
//! `SetForegroundWindow` from a process that does not already own the
//! foreground (`docs/windows-handoff.md`), and Wayland has no equivalent
//! API for a third-party process by design. A `Switch` implemented by
//! finding and raising a window would therefore work on one platform and a
//! half. Delegating to the application's own activation path works on three.
//!
//! The honest cost: for an application that is **not** single-instance, a
//! second "open X" starts a second copy rather than raising the first. That
//! is the application's behaviour, not something this crate can fix, and
//! `switch to` is documented as a synonym of `open` rather than as a
//! promise about windows.
//!
//! # The rule that governs additions
//!
//! Inherited verbatim from `edit-intent`, because the reasoning is the same
//! and the stakes are higher:
//!
//! **A phrasing that could plausibly mean two different things must not
//! parse.** `docs/investigations/edit-intent.md` measured a too-narrow
//! grammar producing 10 silently wrong results out of 55, and the fix was a
//! wider deterministic grammar *plus a refusal path*, not a model. Here,
//! refusing costs the user a rephrase. Guessing opens the wrong application,
//! or worse, interprets dictation as a command.

pub mod launch;
mod parse;
mod registry;

pub use launch::{discover, launch};
pub use parse::{parse, Refusal};
pub use registry::{App, Registry, Resolution};

/// What the user asked for, once their words have been understood.
///
/// One variant today. It is an enum rather than a struct because the
/// proposal's later phases add verbs that are *not* launches (per-app
/// actions, window management), and those must be distinguishable at the
/// type level so that the confirmation rule for irreversible actions cannot
/// be forgotten: a new variant that needs confirmation will not compile
/// against an exhaustive match that does not handle it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// Bring an application to the foreground, starting it if necessary.
    ///
    /// Carries a resolved [`App`] from the registry, never a raw string.
    /// That is the fail-closed property: a command for an application that
    /// is not installed cannot be constructed, so it cannot reach a
    /// launcher, however confident the parser was about the words.
    Launch(App),
}
