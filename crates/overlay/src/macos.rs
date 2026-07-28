//! The macOS overlay: a glowing orb pinned to the bottom of the screen with
//! a rolling window of transcribed words above it, drawn in a borderless,
//! non-activating `NSPanel` (design: `docs/overlay-redesign.md`).
//!
//! The four properties that make this correct, in one place so they can be
//! audited together:
//!
//! 1. **`NSWindowStyleMask::NonactivatingPanel`** plus overridden
//!    `canBecomeKeyWindow`/`canBecomeMainWindow` returning `false`. The
//!    overlay appears while the user is focused on a text field in another
//!    app; if we ever became key, macOS would strip that field's focus and
//!    the dictation would land nowhere. This is the product's core
//!    correctness requirement, not styling.
//! 2. **`orderFrontRegardless`** instead of `makeKeyAndOrderFront`: shows
//!    the panel without any activation path at all.
//! 3. **Collection behavior `CanJoinAllSpaces | FullScreenAuxiliary`** so
//!    the indicator follows the user to every Space and floats over
//!    fullscreen apps — dictation must work wherever the caret is.
//! 4. **`setIgnoresMouseEvents(true)`**: the surface is a pure indicator
//!    with no interactive controls, so every pixel is click-through and a
//!    click "on" the overlay lands in the app beneath, preserving the
//!    user's focus and caret.
//!
//! # How rendering is driven
//!
//! The host pushes [`OverlayFrame`]s at its own cadence (~30 Hz poll of the
//! pipeline's status slot). Those frames carry *inputs* — state, mic level,
//! the current hypothesis — not animation. Animation (word bloom/fade, orb
//! glow, shimmer) needs a faster, steadier clock than the host's poll, so
//! the overlay owns a 60 Hz `NSTimer` that exists **only while the panel is
//! on screen**. Hidden means the timer is invalidated: an idle dictation
//! daemon schedules nothing and costs ~zero CPU, which matters for a tool
//! that runs all day. While visible, a settled frame (a static error line,
//! say) skips `setNeedsDisplay` entirely, so the timer's only cost is the
//! model step.
//!
//! The rolling-window model itself ([`crate::layout::RollingWindow`]) is
//! pure Rust in `layout.rs`, unit-tested headlessly; this file only
//! measures text, feeds the model, and paints it.

use std::cell::RefCell;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    class, define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSPanel, NSScreen, NSStatusWindowLevel, NSStringDrawing,
    NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};

use crate::layout::{self, RollingWindow, Size};
use crate::state::OverlayState;
use crate::theme;
use crate::{Overlay, OverlayFrame};

/// Panel size in points. Wide enough for the lane plus its fade ramp, tall
/// enough for the orb's glow field with the text lane above it. The panel
/// is transparent; these are bounds, not a visible card.
const PANEL_SIZE: Size = Size {
    width: 760.0,
    height: 230.0,
};

/// Left edge of the *newest* word, relative to the panel. Fixed so the
/// user's glance target never moves; older words extend left (the design's
/// "fixed glance anchor").
const ANCHOR_X: f64 = PANEL_SIZE.width / 2.0 + 40.0;
/// Text lane baseline (top-left/flipped coords) and font size.
const LANE_Y: f64 = 52.0;
const WORD_FONT: f64 = 17.0;
/// Orb geometry: core radius and how far the glow field reaches.
const ORB_R: f64 = 21.0;
const GLOW_R: f64 = 78.0;
const ORB_CX: f64 = PANEL_SIZE.width / 2.0;
const ORB_CY: f64 = PANEL_SIZE.height - 88.0;
/// Gap from the panel's bottom edge to the screen's visible-frame bottom.
const BOTTOM_GAP: f64 = 8.0;
/// Animation clock while visible. 0 Hz while hidden (timer invalidated).
const FRAME_HZ: f64 = 60.0;

/// Everything `drawRect:` reads. One snapshot struct, so a frame is
/// coherent or absent, never partially updated.
struct Model {
    state: OverlayState,
    /// Displayed mic level, eased toward `target_level` so the glow
    /// breathes smoothly even though the host only pushes ~30 Hz.
    level: f64,
    target_level: f64,
    /// The rolling window of words (pure model; see layout.rs).
    words: RollingWindow,
    /// State-specific one-liner (an error's situation → action). Rendered
    /// as a single static line in place of the word lane.
    detail: String,
    /// Seconds since the overlay was created; the shimmer/pulse phase.
    now: f64,
    reduce_motion: bool,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            state: OverlayState::Idle,
            level: 0.0,
            target_level: 0.0,
            words: RollingWindow::new(),
            detail: String::new(),
            now: 0.0,
            reduce_motion: false,
        }
    }
}

define_class!(
    /// `NSPanel` subclass whose only job is to refuse focus. The style mask
    /// already requests non-activation; overriding these two methods makes
    /// the guarantee unconditional even if AppKit's heuristics change.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "AquaOverlayPanel"]
    struct OverlayPanel;

    impl OverlayPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

define_class!(
    /// The single content view. Flipped so its coordinates match the
    /// crate's top-left-origin convention.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "AquaOverlayView"]
    #[ivars = RefCell<Model>]
    struct OverlayView;

    impl OverlayView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

fn ns_color(c: theme::Color, alpha_scale: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r, c.g, c.b, c.a * alpha_scale)
}

/// Measure a word's width in points with the lane font. Points, not
/// characters: this is what makes the lane budget CJK-correct for free
/// (design §3 — no character-count constant exists anywhere anymore).
fn measure(text: &str) -> f64 {
    unsafe {
        let font = NSFont::systemFontOfSize(WORD_FONT);
        let font_obj: Retained<AnyObject> = Retained::into_super(Retained::into_super(font));
        let attrs = NSDictionary::from_retained_objects(&[NSFontAttributeName], &[font_obj]);
        NSString::from_str(text)
            .sizeWithAttributes(Some(&attrs))
            .width
    }
}

/// Draw one run of text with the system font. Isolated so the
/// attribute-dictionary unsafety lives in one function.
fn draw_text(text: &str, at: NSPoint, size: f64, color: &NSColor) {
    unsafe {
        let font = NSFont::systemFontOfSize(size);
        let font_obj: Retained<AnyObject> = Retained::into_super(Retained::into_super(font));
        let color_obj: Retained<AnyObject> =
            Retained::into_super(Retained::into_super(color.retain()));
        let attrs = NSDictionary::from_retained_objects(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font_obj, color_obj],
        );
        NSString::from_str(text).drawAtPoint_withAttributes(at, Some(&attrs));
    }
}

/// System Reduce Motion, queried through the runtime rather than the typed
/// binding: `objc2-app-kit`'s `NSWorkspace` feature is not enabled in this
/// crate's manifest. One dynamic call per frame is negligible, and reading
/// it live means flipping the setting mid-session takes effect. This tool
/// *is* assistive technology (docs/ux/06): the reduced variant is a design,
/// not an off-switch.
fn reduce_motion() -> bool {
    unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        msg_send![&*ws, accessibilityDisplayShouldReduceMotion]
    }
}

impl OverlayView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Model::default()));
        unsafe { msg_send![super(this), init] }
    }

    fn draw(&self) {
        let model = self.ivars().borrow();
        self.draw_orb(&model);
        if model.detail.is_empty() {
            self.draw_words(&model);
        } else {
            self.draw_detail(&model);
        }
    }

    /// The orb: a soft radial glow around a bright core, drawn as
    /// concentric circles with a Gaussian alpha falloff — visually a radial
    /// gradient at this size, without adding a quartz-core dependency
    /// (design §6).
    fn draw_orb(&self, model: &Model) {
        let accent = theme::accent(model.state);
        let (glow_gain, core_alpha) = orb_dynamics(model);

        if !model.reduce_motion {
            let reach = GLOW_R * (0.72 + 0.28 * glow_gain);
            let rings = 20usize;
            for i in (0..rings).rev() {
                let f = i as f64 / rings as f64; // 0 center .. 1 edge
                let r = ORB_R + (reach - ORB_R) * f;
                // Gaussian falloff reads as light, not as banded disks.
                let a = (-3.2 * f * f).exp() * 0.16 * (0.55 + 0.45 * glow_gain);
                ns_color(accent, a).setFill();
                let rect = NSRect::new(
                    NSPoint::new(ORB_CX - r, ORB_CY - r),
                    NSSize::new(r * 2.0, r * 2.0),
                );
                NSBezierPath::bezierPathWithOvalInRect(rect).fill();
            }
        } else if model.state == OverlayState::Listening {
            // Reduced motion: the mic level is shown by ring thickness, a
            // per-frame static quantity, not an oscillation. A stroked
            // circle (not a filled disk) so the level reads as a gauge.
            let thick = 2.0 + 6.0 * model.level;
            let r_mid = ORB_R + 6.0 + thick / 2.0;
            let ring_rect = NSRect::new(
                NSPoint::new(ORB_CX - r_mid, ORB_CY - r_mid),
                NSSize::new(r_mid * 2.0, r_mid * 2.0),
            );
            ns_color(accent, 0.8).setStroke();
            let ring = NSBezierPath::bezierPathWithOvalInRect(ring_rect);
            ring.setLineWidth(thick);
            ring.stroke();
        }

        // The core: a filled ball with a bright top highlight so a flat
        // circle reads as a sphere.
        ns_color(accent, core_alpha).setFill();
        let core = NSRect::new(
            NSPoint::new(ORB_CX - ORB_R, ORB_CY - ORB_R),
            NSSize::new(ORB_R * 2.0, ORB_R * 2.0),
        );
        NSBezierPath::bezierPathWithOvalInRect(core).fill();
        let hi_r = ORB_R * 0.62;
        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.34).setFill();
        let hi = NSRect::new(
            NSPoint::new(ORB_CX - hi_r - 3.0, ORB_CY - hi_r - 5.0),
            NSSize::new(hi_r * 2.0, hi_r * 1.6),
        );
        NSBezierPath::bezierPathWithOvalInRect(hi).fill();
    }

    /// The rolling window. The newest word's left edge is pinned at
    /// ANCHOR_X (the glance target never moves); older words extend left
    /// into the fade ramp.
    fn draw_words(&self, model: &Model) {
        let xs = model.words.positions();
        for (w, &x) in model.words.words().iter().zip(&xs) {
            if w.opacity <= 0.01 {
                continue;
            }
            // Committed = settled near-white type. In-flight = accent tint:
            // the tinted zone is the only text allowed to change, which
            // makes the commit horizon visible (the redesign's point).
            let color = if w.committed {
                ns_color(theme::palette::PAPER.alpha(0.95), w.opacity)
            } else {
                ns_color(theme::palette::AQUA.alpha(0.62), w.opacity)
            };
            draw_text(
                &w.text,
                NSPoint::new(ANCHOR_X + x, LANE_Y),
                WORD_FONT,
                &color,
            );
        }
    }

    /// A state detail line ("situation → action" for errors, the model-load
    /// note) replaces the word lane: those states have no hypothesis, and
    /// the line must be static and legible, not animated (design §4).
    fn draw_detail(&self, model: &Model) {
        let width = measure(&model.detail);
        let x = (PANEL_SIZE.width - width) / 2.0;
        let color = ns_color(theme::palette::PAPER.alpha(0.95), 1.0);
        draw_text(&model.detail, NSPoint::new(x, LANE_Y), WORD_FONT, &color);
    }
}

/// Per-state orb motion: (glow gain, core alpha), from the 8-state table in
/// design §4 — which is `docs/ux/05-settings-and-states.md`'s table, not a
/// new state machine.
fn orb_dynamics(model: &Model) -> (f64, f64) {
    if model.reduce_motion {
        // No oscillation of any kind under Reduce Motion.
        let core = match model.state {
            OverlayState::ModelLoading => 0.5,
            OverlayState::Transcribing => 1.0,
            _ => 0.9,
        };
        return (0.0, core);
    }
    match model.state {
        // Glow breathes with the shaped mic level: the orb replaces the bar
        // meter as the "is it hearing me?" surface.
        OverlayState::Listening => (model.level, 0.95),
        // No level input exists; a slow constant shimmer says "machine
        // working, not hung" (UX principle 2).
        OverlayState::Transcribing => {
            let s = (model.now * std::f64::consts::TAU / 1.2).sin() * 0.5 + 0.5;
            (0.3 + 0.4 * s, 1.0)
        }
        // The state table's "pulsing" glyph.
        OverlayState::ModelLoading => {
            let s = (model.now * std::f64::consts::TAU * theme::PULSE_HZ).sin() * 0.5 + 0.5;
            (s, 0.5 + 0.4 * s)
        }
        // Errors get stillness: motion soothes or celebrates, and an error
        // line should do neither. NoPermission likewise.
        _ => (0.15, 0.9),
    }
}

/// Whether a state's orb oscillates on its own (needs repaints even when
/// the word model is settled and the level is steady).
fn state_self_animates(state: OverlayState, reduce_motion: bool) -> bool {
    !reduce_motion
        && matches!(
            state,
            OverlayState::Transcribing | OverlayState::ModelLoading
        )
}

/// The macOS implementation of [`Overlay`].
pub struct MacOverlay {
    panel: Retained<OverlayPanel>,
    view: Retained<OverlayView>,
    mtm: MainThreadMarker,
    visible: bool,
    /// The 60 Hz animation clock. `Some` exactly while visible; hidden
    /// invalidates it so an idle daemon schedules nothing (battery).
    timer: Option<Retained<NSTimer>>,
    /// Zero point for `Model::now` (shimmer phase, stale-decay clocks).
    epoch: Instant,
    /// State of the previous frame, to detect utterance boundaries.
    last_state: OverlayState,
}

impl MacOverlay {
    pub fn new(mtm: MainThreadMarker) -> anyhow::Result<Self> {
        let content = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(PANEL_SIZE.width, PANEL_SIZE.height),
        );
        // Borderless + NonactivatingPanel is requirement #1 (see module doc).
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel: Retained<OverlayPanel> = unsafe {
            msg_send![
                OverlayPanel::alloc(mtm),
                initWithContentRect: content,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };
        {
            // Status level floats above normal and floating windows; the
            // collection behavior keeps it on every Space and over
            // fullscreen apps (requirement #3).
            panel.setLevel(NSStatusWindowLevel);
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            // The panel is a fully transparent container; the orb and words
            // are the only pixels. No shadow: a shadow would draw a
            // rectangle around what should read as a free-floating orb and
            // loose glowing words.
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setHasShadow(false);
            // Click-through (requirement #4).
            panel.setIgnoresMouseEvents(true);
            // Never disappear because some other app took focus; this is an
            // indicator, not a window the user manages.
            panel.setHidesOnDeactivate(false);
        }
        let view = OverlayView::new(mtm);
        panel.setContentView(Some(&view));
        Ok(MacOverlay {
            panel,
            view,
            mtm,
            visible: false,
            timer: None,
            epoch: Instant::now(),
            last_state: OverlayState::Idle,
        })
    }

    /// Bottom-center of the main screen's visible frame (clears the Dock),
    /// the placement the design pins the orb to. `mainScreen` is the screen
    /// with the key window, i.e. wherever the user is working.
    fn place_panel(&self) {
        if let Some(screen) = NSScreen::mainScreen(self.mtm) {
            let vf = screen.visibleFrame();
            let x = vf.origin.x + (vf.size.width - PANEL_SIZE.width) / 2.0;
            let y = vf.origin.y + BOTTOM_GAP;
            self.panel.setFrameOrigin(NSPoint::new(x, y));
        }
    }

    /// Start the animation clock. Idempotent; only ever called while
    /// visible. The timer holds a retained view pointer, an epoch, and its
    /// own dt bookkeeping — no reference back to `MacOverlay`, so there is
    /// no cycle to break beyond invalidating the timer.
    fn start_timer(&mut self) {
        if self.timer.is_some() {
            return;
        }
        let view = self.view.retain();
        let epoch = self.epoch;
        let last = RefCell::new(epoch.elapsed().as_secs_f64());
        let tick = RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            let now = epoch.elapsed().as_secs_f64();
            let dt = (now - *last.borrow()).max(0.0);
            *last.borrow_mut() = now;
            let mut repaint = false;
            {
                let mut model = view.ivars().borrow_mut();
                model.now = now;
                model.reduce_motion = reduce_motion();
                // Ease the displayed level toward the host's target so the
                // glow breathes smoothly between 30 Hz host pushes. Under
                // Reduce Motion the level snaps: no oscillation, and the
                // ring gauge is a static per-frame quantity.
                let level_target = layout::shape_level(model.target_level as f32) as f64;
                if model.reduce_motion {
                    repaint |= (level_target - model.level).abs() > 0.01;
                    model.level = level_target;
                } else {
                    let ease = 1.0 - (-dt / layout::EASE_TAU).exp();
                    let delta = (level_target - model.level) * ease;
                    if delta.abs() > 0.002 {
                        model.level += delta;
                        repaint = true;
                    }
                }
                let rm = model.reduce_motion;
                repaint |= model.words.step(now, dt, rm);
                repaint |= state_self_animates(model.state, rm);
            }
            if repaint {
                view.setNeedsDisplay(true);
            }
        });
        let timer = unsafe {
            let timer = NSTimer::timerWithTimeInterval_repeats_block(1.0 / FRAME_HZ, true, &tick);
            // Common modes so animation continues during menu tracking.
            NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
            timer
        };
        self.timer = Some(timer);
    }

    fn stop_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
    }
}

impl Drop for MacOverlay {
    fn drop(&mut self) {
        // An invalidated timer is removed from the run loop; without this a
        // dropped overlay would leave a 60 Hz callback running forever.
        self.stop_timer();
    }
}

impl Overlay for MacOverlay {
    fn render(&mut self, frame: &OverlayFrame) -> anyhow::Result<()> {
        if !frame.state.overlay_visible() {
            self.last_state = frame.state;
            return self.hide();
        }

        let now = self.epoch.elapsed().as_secs_f64();
        {
            let mut model = self.view.ivars().borrow_mut();
            // A fresh utterance begins when we enter Listening from any
            // state that was not mid-utterance: the lane must start empty,
            // not show the previous utterance's stale window.
            if frame.state == OverlayState::Listening
                && !matches!(
                    self.last_state,
                    OverlayState::Listening | OverlayState::Transcribing
                )
            {
                model.words.reset();
            }
            model.state = frame.state;
            model.target_level = frame.audio_level as f64;
            model.detail = frame.detail.clone().unwrap_or_default();
            model.now = now;
            model.reduce_motion = reduce_motion();
            // Feed the whole current hypothesis; the model diffs it
            // unit-wise, applies the display-side stability policy, and
            // ignores unchanged repeats (host polls faster than speech).
            if !frame.partial_text.is_empty() {
                model.words.ingest(&frame.partial_text, now, &mut measure);
            }
            // Key released: the recognizer is finalizing and the hypothesis
            // will be committed wholesale, so stability no longer applies —
            // the same rule as `stream::CommitHorizon::finish`. Turning the
            // lane white here is what shows "this is what will be written"
            // while the finalize pass runs.
            if frame.state == OverlayState::Transcribing
                && self.last_state == OverlayState::Listening
            {
                model.words.finalize(now);
            }
        }
        self.last_state = frame.state;

        // A host push always repaints once: state/detail/level changed, and
        // the 60 Hz timer only repaints what it knows moved.
        self.view.setNeedsDisplay(true);

        if !self.visible {
            self.place_panel();
            // orderFrontRegardless shows the panel with no activation path
            // at all (requirement #2 in the module doc).
            self.panel.orderFrontRegardless();
            self.visible = true;
        }
        self.start_timer();
        Ok(())
    }

    fn hide(&mut self) -> anyhow::Result<()> {
        // Hidden = zero cost: no timer, no scheduled work of any kind.
        self.stop_timer();
        if self.visible {
            self.panel.orderOut(None);
            self.visible = false;
        }
        // Next appearance starts a fresh window rather than replaying the
        // stale one.
        self.view.ivars().borrow_mut().words.reset();
        Ok(())
    }

    fn is_visible(&self) -> bool {
        self.visible
    }
}
