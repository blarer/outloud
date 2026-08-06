//! The dictation overlay: a small floating indicator that appears while the
//! microphone is hot, shows live state and the partial-text tail, and — the
//! one non-negotiable — **never steals keyboard focus**. The whole product
//! edits the text field the user is focused on; an overlay that takes key
//! status destroys that field's focus and with it the edit we are about to
//! perform. Everything in this crate is subordinate to that requirement.
//!
//! # Implementation approach, and why
//!
//! The macOS backend talks to AppKit directly through `objc2` /
//! `objc2-app-kit` rather than going through a windowing crate (winit, tao,
//! egui, tauri). Justification:
//!
//! * **Non-activating panels are the whole point.** The correctness
//!   requirement is `NSPanel` with `.nonactivatingPanel`, `canBecomeKey =
//!   false`, and a floating window level. General windowing crates are built
//!   around windows that *do* want focus; winit only grew partial support
//!   for this and hides the panel-specific knobs (`becomesKeyOnlyIfNeeded`,
//!   collection behavior) behind versions and feature flags we would fight
//!   forever. With AppKit direct, the four critical lines are four visible,
//!   auditable lines.
//! * **The `display` feature gate stays clean.** This workspace builds
//!   headless (`--no-default-features`) as a correctness gate, mirroring
//!   `text-target`. `objc2-app-kit` is an optional dependency behind
//!   `display`; a windowing crate would drag an event-loop abstraction and
//!   its transitive GUI deps into the tree even when unused.
//! * **No event loop of our own.** The overlay is a passive renderer driven
//!   by the host app's main run loop. A framework that insists on owning the
//!   event loop (winit, egui) inverts that relationship.
//!
//! Everything platform-independent — the state machine from
//! `docs/ux/05-settings-and-states.md`, the anchor-selection and positioning
//! math, the meter shaping — is pure Rust in [`state`] and [`layout`],
//! compiled and unit-tested on every platform including headless CI.

/// The animated cat mascot: pure vector geometry plus a pure animator.
/// Platform-neutral for the same reason [`mark`] is: headless CI asserts
/// its motion properties, and every backend renders the same points.
pub mod cat;
pub mod layout;
pub mod mark;
pub mod menu;
pub mod pixel;
/// The animated skull mascot: pure vector geometry plus a pure animator.
/// Platform-neutral for the same reason [`mark`] is: headless CI asserts
/// its motion properties, and every backend renders the same points.
pub mod skull;
pub mod state;
pub mod text_window;
/// The visual language (palette, radii, type scale, motion) as pure data.
/// Platform-neutral on purpose: it must compile in the headless build, and
/// it is what keeps the macOS and Windows backends from drifting apart.
pub mod theme;

#[cfg(all(target_os = "macos", feature = "display"))]
pub mod macos;

/// The macOS menu-bar status item. Gated exactly like [`macos`]: a headless
/// build has no menu bar to put anything in, and must not link AppKit.
#[cfg(all(target_os = "macos", feature = "display"))]
pub mod status_item;

#[cfg(all(target_os = "windows", feature = "display"))]
pub mod windows;

/// The Windows notification-area tray icon. Gated exactly like [`windows`]:
/// a headless build has no tray to put anything in, and must not link the
/// Shell_NotifyIcon / TrackPopupMenu surface.
#[cfg(all(target_os = "windows", feature = "display"))]
pub mod win_tray;

pub use layout::{place, Anchor, Point, Rect, Size};
pub use menu::{MenuId, MenuItem, MenuModel};
pub use state::OverlayState;

/// One complete description of what the overlay should show right now.
///
/// The host pushes frames; the overlay renders them. There is deliberately
/// no incremental API (`set_level`, `set_text`, …) because the state machine
/// contract says several states hide the overlay entirely — a single frame
/// makes "what is visible" a pure function of the last frame, with no
/// stale-field bugs.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayFrame {
    /// Current product state. Decides visibility, glyph, and layout.
    pub state: OverlayState,
    /// Microphone level in `0.0..=1.0`. Only rendered while `Listening`.
    pub audio_level: f32,
    /// The provisional transcription tail. Only the trailing portion is
    /// shown; the committed text lives in the target field, not here.
    pub partial_text: String,
    /// State-specific one-liner: the error's situation→action string, the
    /// "will transcribe in ~3s" note during model load, elapsed ms while
    /// transcribing. `None` renders the state's default label.
    pub detail: Option<String>,
    /// Where to appear. Callers should pass the best anchor they know:
    /// caret bounds from ax-edit's `AXBoundsForRange` when available, else
    /// the mouse cursor, else `Corner`.
    pub anchor: Anchor,
}

impl OverlayFrame {
    /// A frame for a state with no live data, anchored at the fallback
    /// corner. Convenient for tests and simple callers.
    pub fn state_only(state: OverlayState) -> Self {
        Self {
            state,
            audio_level: 0.0,
            partial_text: String::new(),
            detail: None,
            anchor: Anchor::Corner,
        }
    }
}

/// The platform-neutral overlay surface. One implementation per OS; the
/// Windows and Linux backends slot in behind this trait later exactly the
/// way `ax-edit` stubs its non-macOS backends.
///
/// Implementations must guarantee:
/// * rendering never takes keyboard focus from any other application,
/// * the surface is visible on every Space/virtual desktop and over
///   fullscreen apps,
/// * non-interactive regions are click-through.
pub trait Overlay {
    /// Show (or update) the overlay for this frame. States whose UX
    /// contract is "show nothing" (`Idle`, `Injecting`, `DegradedOffline`)
    /// hide the surface; this is not an error.
    fn render(&mut self, frame: &OverlayFrame) -> anyhow::Result<()>;

    /// Hide the overlay immediately regardless of state.
    fn hide(&mut self) -> anyhow::Result<()>;

    /// Whether the surface is currently on screen.
    fn is_visible(&self) -> bool;

    /// Optional richer audio signal for backends that animate with it:
    /// four linear `0.0..=1.0` bands, low to high (~0–300Hz, 300–1k,
    /// 1k–3k, 3k–8k), updated per audio chunk. A default no-op so hosts
    /// without band data need no changes, and so the pipeline crate can
    /// start supplying bands without a lockstep overlay change.
    fn set_audio_bands(&mut self, _bands: [f32; 4]) {}
}

/// Construct the overlay for this platform and build.
///
/// On macOS with the `display` feature this must be called on the main
/// thread (AppKit requirement). Elsewhere it returns `Unsupported`, so
/// callers compile everywhere and branch at runtime — the same shape as
/// `ax-edit` on non-macOS.
pub fn platform_overlay() -> anyhow::Result<Box<dyn Overlay>> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    {
        let mtm = objc2::MainThreadMarker::new().ok_or_else(|| {
            anyhow::anyhow!("the overlay must be created on the main thread (AppKit requirement)")
        })?;
        Ok(Box::new(macos::MacOverlay::new(mtm)?))
    }
    #[cfg(all(target_os = "windows", feature = "display"))]
    {
        Ok(Box::new(windows::WinOverlay::new()?))
    }
    #[cfg(not(any(
        all(target_os = "macos", feature = "display"),
        all(target_os = "windows", feature = "display")
    )))]
    {
        anyhow::bail!(
            "overlay: unsupported here (needs macOS or Windows and the `display` feature); \
             terminal surfaces render the same state machine via OSC instead"
        )
    }
}

#[cfg(test)]
mod tests {
    // Imported inside the cfg arms below: on a Windows display build every
    // arm is compiled out, so a top-level `use super::*` would be unused.
    #[test]
    fn platform_overlay_off_display_is_unsupported_not_a_panic() {
        #[allow(unused_imports)]
        use super::*;

        // On headless builds and unsupported platforms this must be a clean
        // error. On macOS+display it must still not panic off the main
        // thread. On Windows+display construction may genuinely succeed, so
        // no assertion is made there beyond not panicking.
        #[cfg(not(any(
            all(target_os = "macos", feature = "display"),
            all(target_os = "windows", feature = "display")
        )))]
        assert!(platform_overlay().is_err());
        #[cfg(all(target_os = "macos", feature = "display"))]
        {
            let res = std::thread::spawn(|| platform_overlay().err().map(|e| e.to_string()))
                .join()
                .unwrap();
            // Off the main thread creation must refuse, not crash.
            assert!(res.is_some());
        }
    }
}
