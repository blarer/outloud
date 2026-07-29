//! The macOS overlay: an animated skull pinned to the bottom of the screen
//! with a rolling window of transcribed words above it, drawn in a
//! borderless, non-activating `NSPanel` (design: `docs/overlay-redesign.md`;
//! skull geometry/motion: [`crate::skull`]).
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
//! the current hypothesis — not animation. Animation (jaw, breath, word
//! bloom/fade, shimmer) needs a faster, steadier clock than the host's
//! poll, so the overlay owns a **`CADisplayLink`** (via
//! `NSView.displayLinkWithTarget:selector:`, macOS 14+) that exists **only
//! while the panel is on screen**. A display link fires on the display's
//! actual refresh (vsync), so motion is sampled exactly once per shown
//! frame — a sleep-based `NSTimer` at nominally 60 Hz drifts against vsync
//! and beats visibly. On the off chance the selector is unavailable the
//! code falls back to the old 60 Hz `NSTimer`, which is worse but not
//! wrong. Hidden means the clock is invalidated: an idle dictation daemon
//! schedules nothing and costs ~zero CPU, which matters for a tool that
//! runs all day. While visible, a settled frame (a static error line under
//! Reduce Motion, say) skips `setNeedsDisplay` entirely.
//!
//! Set `OVERLAY_FRAMESTATS=1` to get measured tick-interval and draw-time
//! percentiles on stderr every ~4 s — measured, because this project does
//! not estimate (`docs/latency.md`).
//!
//! The rolling-window model itself ([`crate::layout::RollingWindow`]) is
//! pure Rust in `layout.rs`, unit-tested headlessly, and the skull's
//! geometry and animator are pure Rust in `skull.rs`; this file only
//! measures text, feeds the models, and paints them.

use std::cell::RefCell;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    class, define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSGradient, NSGraphicsContext, NSPanel, NSScreen, NSShadow,
    NSStatusWindowLevel, NSStringDrawing, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};

use crate::layout::{self, RollingWindow, Size};
use crate::skull::{self, SkullAnimator, SkullPose};
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
/// The skull's bounding box in panel points: EXACTLY the orb's box. The
/// orb was a circle of radius [`ORB_R`] centred at (panel centre,
/// height-88); the skull occupies that same 42pt square, because this is
/// a glanceable status indicator over someone's work, not a focal point.
/// Legibility at 42pt comes from simplified geometry in [`crate::skull`]
/// (three wide teeth, bold sockets), not from being bigger.
const ORB_R: f64 = 21.0;
const SKULL_SIZE: f64 = ORB_R * 2.0;
const SKULL_X: f64 = (PANEL_SIZE.width - SKULL_SIZE) / 2.0;
const SKULL_Y: f64 = PANEL_SIZE.height - 88.0 - ORB_R;
/// The glow field behind the skull: the orb's Gaussian-ring aura,
/// unchanged — same centre, same reach — so the overall footprint is the
/// one the design already accepted.
const GLOW_CX: f64 = PANEL_SIZE.width / 2.0;
const GLOW_CY: f64 = PANEL_SIZE.height - 88.0;
const GLOW_R: f64 = 78.0;
/// Gap from the panel's bottom edge to the screen's visible-frame bottom.
const BOTTOM_GAP: f64 = 8.0;
/// Fallback animation clock when `CADisplayLink` is unavailable. 0 Hz
/// while hidden (clock invalidated).
const FALLBACK_HZ: f64 = 60.0;

/// Everything `drawRect:` reads. One snapshot struct, so a frame is
/// coherent or absent, never partially updated.
struct Model {
    state: OverlayState,
    /// Displayed mic level, eased toward `target_level` so the eye glow
    /// breathes smoothly even though the host only pushes ~30 Hz.
    level: f64,
    target_level: f64,
    /// Optional per-band levels from the host (see
    /// [`crate::Overlay::set_audio_bands`]). All-zero means "no band data",
    /// and the jaw falls back to the broadband level.
    bands: [f32; 4],
    /// The rolling window of words (pure model; see layout.rs).
    words: RollingWindow,
    /// The skull's motion state and its latest pose (pure; see skull.rs).
    animator: SkullAnimator,
    pose: SkullPose,
    /// State-specific one-liner (an error's situation → action). Rendered
    /// as a single static line in place of the word lane.
    detail: String,
    /// Seconds since the overlay was created; the shimmer/pulse phase.
    now: f64,
    /// `now` at the previous animation tick, for dt.
    last_tick: f64,
    /// Zero point for `now`. Lives in the model so the view's own tick
    /// callback and the host's render path read the same clock.
    epoch: Instant,
    reduce_motion: bool,
    stats: FrameStats,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            state: OverlayState::Idle,
            level: 0.0,
            target_level: 0.0,
            bands: [0.0; 4],
            words: RollingWindow::new(),
            animator: SkullAnimator::new(),
            pose: SkullPose::at_rest(),
            detail: String::new(),
            now: 0.0,
            last_tick: 0.0,
            epoch: Instant::now(),
            reduce_motion: false,
            stats: FrameStats::default(),
        }
    }
}

/// Measured frame health: tick intervals (the vsync cadence we actually
/// got) and draw durations (what a frame costs). This project measures
/// instead of estimating (`docs/latency.md`); enabling
/// `OVERLAY_FRAMESTATS=1` prints percentiles on stderr every ~4 s.
#[derive(Default)]
struct FrameStats {
    tick_intervals_ms: Vec<f64>,
    draw_ms: Vec<f64>,
    last_report: f64,
}

fn stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("OVERLAY_FRAMESTATS").is_some_and(|v| v != "0"))
}

impl FrameStats {
    fn record_tick(&mut self, interval_ms: f64) {
        if stats_enabled() {
            self.tick_intervals_ms.push(interval_ms);
        }
    }

    fn record_draw(&mut self, ms: f64) {
        if stats_enabled() {
            self.draw_ms.push(ms);
        }
    }

    fn maybe_report(&mut self, now: f64) {
        if !stats_enabled() || now - self.last_report < 4.0 || self.tick_intervals_ms.len() < 30 {
            return;
        }
        let pct = |v: &mut Vec<f64>, p: f64| -> f64 {
            v.sort_by(|a, b| a.total_cmp(b));
            v[((v.len() - 1) as f64 * p) as usize]
        };
        let n = self.tick_intervals_ms.len();
        let (t50, t95, tmax) = (
            pct(&mut self.tick_intervals_ms, 0.5),
            pct(&mut self.tick_intervals_ms, 0.95),
            pct(&mut self.tick_intervals_ms, 1.0),
        );
        let (d50, d95) = if self.draw_ms.is_empty() {
            (0.0, 0.0)
        } else {
            (pct(&mut self.draw_ms, 0.5), pct(&mut self.draw_ms, 0.95))
        };
        eprintln!(
            "overlay-framestats: {n} ticks | interval p50 {t50:.2}ms p95 {t95:.2}ms max {tmax:.2}ms | draw p50 {d50:.2}ms p95 {d95:.2}ms"
        );
        self.tick_intervals_ms.clear();
        self.draw_ms.clear();
        self.last_report = now;
    }
}

define_class!(
    /// `NSPanel` subclass whose only job is to refuse focus. The style mask
    /// already requests non-activation; overriding these two methods makes
    /// the guarantee unconditional even if AppKit's heuristics change.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "OutLoudOverlayPanel"]
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
    /// crate's top-left-origin convention. Also the display link's target:
    /// `aquaTick:` is the per-refresh animation step.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OutLoudOverlayView"]
    #[ivars = RefCell<Model>]
    struct OverlayView;

    impl OverlayView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let t0 = Instant::now();
            self.draw();
            let ms = t0.elapsed().as_secs_f64() * 1e3;
            self.ivars().borrow_mut().stats.record_draw(ms);
        }

        /// One animation frame, fired by the `CADisplayLink` at the
        /// display's real refresh (or by the fallback `NSTimer`).
        #[unsafe(method(aquaTick:))]
        fn aqua_tick(&self, _sender: &AnyObject) {
            self.step_animation();
        }
    }
);

fn ns_color(c: theme::Color, alpha_scale: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r, c.g, c.b, c.a * alpha_scale)
}

/// Where the light comes from.
///
/// Depth is not a pile of effects; it is every highlight and shadow
/// agreeing about one light. Inconsistent lighting is the specific thing
/// that makes an interface look assembled rather than designed, so this
/// constant exists to be the single answer, and everything below derives
/// from it instead of choosing its own direction.
///
/// Top-front, very slightly left, which is the convention macOS itself
/// uses: lit surfaces face up, shadows fall down and a little right.
const LIGHT_ELEVATION: f64 = 0.92;

/// Build the mapped path for one unit-square polygon.
///
/// Split out of [`fill_poly`] because the depth passes need the same path
/// several times over (shade, fill, rim) and rebuilding it per pass would
/// be both slower and a chance for the passes to disagree by a rounding
/// error, which is exactly how a rim light ends up half a point off its
/// own shape.
fn poly_path(poly: &[crate::layout::Point]) -> Option<Retained<NSBezierPath>> {
    if poly.len() < 3 {
        return None;
    }
    let map = |p: &crate::layout::Point| {
        NSPoint::new(SKULL_X + p.x * SKULL_SIZE, SKULL_Y + p.y * SKULL_SIZE)
    };
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(map(&poly[0]));
    for p in &poly[1..] {
        path.lineToPoint(map(p));
    }
    path.closePath();
    Some(path)
}

/// Fill one skull polygon, mapping the unit-square geometry into the
/// panel's SKULL box. The mapping lives here — not in `skull.rs` — so the
/// pure geometry stays resolution-free and the same points serve any panel
/// size or Retina factor (NSBezierPath is drawn in points; AppKit handles
/// the backing scale).
fn fill_poly(poly: &[crate::layout::Point], color: &NSColor) {
    let Some(path) = poly_path(poly) else {
        return;
    };
    color.setFill();
    path.fill();
}

/// Fill a polygon with a vertical gradient instead of one flat tone.
///
/// A single flat fill is what makes a shape read as a sticker. Giving the
/// bone a lit top and a shaded underside is the cheapest possible cue that
/// it is a solid object, and it costs one extra draw rather than a blur.
fn fill_poly_lit(poly: &[crate::layout::Point], lit: &NSColor, shade: &NSColor) {
    let Some(path) = poly_path(poly) else {
        return;
    };
    // A gradient can fail to construct only if AppKit rejects the colours;
    // falling back to the flat lit tone keeps the skull drawn rather than
    // leaving a hole in it.
    let Some(gradient) = NSGradient::initWithStartingColor_endingColor(
        <NSGradient as objc2::AllocAnyThread>::alloc(),
        shade,
        lit,
    ) else {
        lit.setFill();
        path.fill();
        return;
    };
    // 90 degrees is bottom-to-top in AppKit's default space, so `lit` lands
    // on the upper surface, agreeing with LIGHT_ELEVATION.
    gradient.drawInBezierPath_angle(&path, 90.0 * LIGHT_ELEVATION);
}

/// Draw `body` with a soft drop shadow beneath it.
///
/// Uses an explicit shadow rather than compositing a blurred copy: AppKit
/// renders it once into the same context, and confining it to a saved
/// graphics state keeps it from leaking onto the passes that follow, which
/// is the usual way a stray shadow ends up smeared under the text lane.
fn with_drop_shadow(offset_y: f64, blur: f64, alpha: f64, body: impl FnOnce()) {
    NSGraphicsContext::saveGraphicsState_class();
    let shadow = NSShadow::new();
    // Negative y: AppKit's default coordinate space puts positive y up,
    // so the shadow must fall DOWN to agree with a light from above.
    shadow.setShadowOffset(NSSize::new(0.0, -offset_y));
    shadow.setShadowBlurRadius(blur);
    let ink = theme::palette::INK;
    shadow.setShadowColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        ink.r, ink.g, ink.b, alpha,
    )));
    shadow.set();
    body();
    NSGraphicsContext::restoreGraphicsState_class();
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
        self.draw_skull(&model);
        if model.detail.is_empty() {
            self.draw_words(&model);
        } else {
            self.draw_detail(&model);
        }
    }

    /// One animation step: advance the pure models with a real dt, then
    /// schedule a repaint only if something visibly moved. Called by the
    /// display link (vsync) or the fallback timer.
    fn step_animation(&self) {
        let mut repaint = false;
        {
            let mut model = self.ivars().borrow_mut();
            let now = model.epoch.elapsed().as_secs_f64();
            let dt = (now - model.last_tick).max(0.0);
            if model.last_tick > 0.0 {
                model.stats.record_tick(dt * 1e3);
            }
            model.last_tick = now;
            model.now = now;
            model.reduce_motion = reduce_motion();
            let rm = model.reduce_motion;

            // Ease the displayed broadband level toward the host's target
            // so glow breathes smoothly between 30 Hz host pushes. Under
            // Reduce Motion the level snaps: no oscillation.
            let level_target = layout::shape_level(model.target_level as f32) as f64;
            if rm {
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

            // The jaw prefers the low/mid band envelope when the host
            // supplies bands (speech energy lives there); an all-zero array
            // means no band data and falls back to the broadband level.
            let jaw_drive = if model.bands.iter().any(|&b| b > 0.0) {
                layout::shape_level(model.bands[0].max(model.bands[1])) as f64
            } else {
                model.level
            };
            let state = model.state;
            let pose = model.animator.step(now, dt, state, jaw_drive, rm);
            if pose.visibly_differs(&model.pose) {
                model.pose = pose;
                repaint = true;
            }
            repaint |= model.words.step(now, dt, rm);
            repaint |= state_self_animates(model.state, rm);
            model.stats.maybe_report(now);
        }
        if repaint {
            self.setNeedsDisplay(true);
        }
    }

    /// The skull: pure posed geometry from [`crate::skull`], mapped into
    /// the panel's SKULL box and filled with NSBezierPath polygons. Behind
    /// it, the orb's old Gaussian-ring glow survives as the skull's aura —
    /// still a radial gradient without a quartz-core dependency.
    fn draw_skull(&self, model: &Model) {
        let accent = theme::accent(model.state);
        let geo = skull::posed_geometry(&model.pose);
        // Aura first, so the skull draws over it. Reduced motion drops the
        // aura entirely: it exists to flicker with the voice.
        if !model.reduce_motion {
            let gain = model.pose.eye_glow;
            let reach = GLOW_R * (0.72 + 0.28 * gain);
            let rings = 20usize;
            for i in (0..rings).rev() {
                let f = i as f64 / rings as f64; // 0 center .. 1 edge
                let r = ORB_R + (reach - ORB_R) * f;
                // Gaussian falloff reads as light, not as banded disks.
                let a = (-3.2 * f * f).exp() * 0.10 * (0.55 + 0.45 * gain);
                ns_color(accent, a).setFill();
                let rect = NSRect::new(
                    NSPoint::new(GLOW_CX - r, GLOW_CY - r),
                    NSSize::new(r * 2.0, r * 2.0),
                );
                NSBezierPath::bezierPathWithOvalInRect(rect).fill();
            }
        }

        // Bone: near-paper white with a hint of the accent, dark features.
        // The skull reads on light and dark desktops because bone is light
        // and every feature is a dark cutout — self-contrast, no theme
        // branch needed.
        //
        // Every alpha is scaled by the pose's opacity, which is below 1.0
        // only during the entry animation. Fading the whole skull as one
        // object keeps the features from appearing to float in separately.
        let fade = model.pose.opacity;
        // Three bone tones instead of one, so a lit top can meet a shaded
        // underside. A single flat fill is what made this read as a sticker
        // rather than an object.
        let bone_lit = ns_color(theme::palette::PAPER, 0.99 * fade);
        let bone = ns_color(theme::palette::PAPER.alpha(0.86), 0.96 * fade);
        let bone_shade = ns_color(theme::palette::PAPER.alpha(0.66), 0.96 * fade);
        let dark = ns_color(theme::palette::INK, 0.94 * fade);

        // Cast the whole skull onto the desktop behind it. One shadow for
        // the silhouette, not one per polygon: the parts are a single solid
        // object, and shadowing them individually would advertise that they
        // are separate shapes, which is precisely the flatness being fixed.
        //
        // Reduced motion keeps the shadow. It is depth, not animation, and
        // removing it would flatten the object for the users who most need
        // to find it quickly.
        let shadow_alpha = 0.42 * fade;
        with_drop_shadow(2.0, 5.0, shadow_alpha, || {
            fill_poly(&geo.cranium, &bone);
            fill_poly(&geo.jaw, &bone_shade);
        });

        // Now the lit passes, drawn over the shadowed silhouette so the
        // shadow reads as cast by the whole head.
        fill_poly(&geo.mouth, &dark);
        fill_poly_lit(&geo.cranium, &bone_lit, &bone_shade);
        // The jaw sits under the cranium, so it never catches the top light:
        // its own gradient runs darker at both ends. This is what stops the
        // two pieces looking like one flat outline.
        fill_poly_lit(&geo.jaw, &bone, &bone_shade);

        // Sockets last among the dark features, and with an inner shadow, so
        // they read as holes in a solid rather than black paint on a
        // surface. Cheapest possible cue that the bone has thickness.
        with_drop_shadow(-1.0, 2.0, 0.5 * fade, || {
            for socket in &geo.sockets {
                fill_poly(socket, &dark);
            }
        });
        // Eye glow: the state's accent inside the sockets, alpha from the
        // pose (listening brightens with the voice, transcribing shimmers,
        // loading pulses, errors stare).
        let glow = ns_color(accent, (0.25 + 0.75 * model.pose.eye_glow) * fade);
        for eye in &geo.eyes {
            fill_poly(eye, &glow);
        }
        fill_poly(&geo.nose, &dark);
        for tooth in &geo.teeth {
            fill_poly(tooth, &bone);
        }
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

/// Whether a state's skull oscillates on its own (needs repaints even when
/// the word model is settled and the level is steady). With motion on, the
/// answer is every state: breath, sway and blink never stop — the mascot
/// must never look frozen. Under Reduce Motion nothing self-animates.
fn state_self_animates(_state: OverlayState, reduce_motion: bool) -> bool {
    !reduce_motion
}

/// The animation clock. Vsync when the OS provides it, a plain timer when
/// not; both are invalidated the moment the panel hides.
enum Clock {
    /// `CADisplayLink` from `NSView.displayLinkWithTarget:selector:`
    /// (macOS 14+), held type-erased so this crate needs no quartz-core
    /// dependency for one object it only ever sends `invalidate` to.
    DisplayLink(Retained<AnyObject>),
    Timer(Retained<NSTimer>),
}

/// The macOS implementation of [`Overlay`].
pub struct MacOverlay {
    panel: Retained<OverlayPanel>,
    view: Retained<OverlayView>,
    mtm: MainThreadMarker,
    visible: bool,
    /// The animation clock. `Some` exactly while visible; hidden
    /// invalidates it so an idle daemon schedules nothing (battery).
    clock: Option<Clock>,
    /// Zero point for `Model::now`, copied from the model so the host
    /// render path and the view's tick share one clock.
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
            // The panel is a fully transparent container; the skull and
            // words are the only pixels. No shadow: a shadow would draw a
            // rectangle around what should read as a free-floating mascot
            // and loose glowing words.
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
        // One clock for the host path and the tick: the model's epoch.
        let epoch = view.ivars().borrow().epoch;
        Ok(MacOverlay {
            panel,
            view,
            mtm,
            visible: false,
            clock: None,
            epoch,
            last_state: OverlayState::Idle,
        })
    }

    /// Bottom-center of the main screen's visible frame (clears the Dock),
    /// the placement the design pins the skull to. `mainScreen` is the
    /// screen with the key window, i.e. wherever the user is working.
    fn place_panel(&self) {
        if let Some(screen) = NSScreen::mainScreen(self.mtm) {
            let vf = screen.visibleFrame();
            let x = vf.origin.x + (vf.size.width - PANEL_SIZE.width) / 2.0;
            let y = vf.origin.y + BOTTOM_GAP;
            self.panel.setFrameOrigin(NSPoint::new(x, y));
        }
    }

    /// Start the animation clock. Idempotent; only ever called while
    /// visible.
    ///
    /// Preferred clock: a `CADisplayLink` targeting the view's `aquaTick:`,
    /// created by `NSView.displayLinkWithTarget:selector:` so it is bound
    /// to the screen the view is actually on — ticks arrive on the real
    /// vsync (120 Hz on ProMotion, 60 Hz elsewhere) instead of a sleep
    /// loop's approximation of one. Fallback for pre-14 systems: the old
    /// 60 Hz repeating `NSTimer` driving the same selector path.
    fn start_clock(&mut self) {
        if self.clock.is_some() {
            return;
        }
        // Reset dt bookkeeping so the first tick after a re-show does not
        // integrate the hidden gap as one giant step.
        {
            let mut model = self.view.ivars().borrow_mut();
            model.last_tick = model.epoch.elapsed().as_secs_f64();
        }
        let view_obj: &AnyObject = self.view.as_ref();
        let supported: bool = unsafe {
            msg_send![view_obj, respondsToSelector: sel!(displayLinkWithTarget:selector:)]
        };
        if supported {
            let link: Retained<AnyObject> = unsafe {
                msg_send![
                    view_obj,
                    displayLinkWithTarget: view_obj,
                    selector: sel!(aquaTick:),
                ]
            };
            unsafe {
                // Common modes so animation continues during menu tracking,
                // same rationale as the old timer.
                let run_loop = NSRunLoop::currentRunLoop();
                let _: () = msg_send![
                    &*link,
                    addToRunLoop: &*run_loop,
                    forMode: NSRunLoopCommonModes,
                ];
            }
            self.clock = Some(Clock::DisplayLink(link));
            return;
        }
        let view = self.view.retain();
        let tick = RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            view.step_animation();
        });
        let timer = unsafe {
            let timer =
                NSTimer::timerWithTimeInterval_repeats_block(1.0 / FALLBACK_HZ, true, &tick);
            NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
            timer
        };
        self.clock = Some(Clock::Timer(timer));
    }

    fn stop_clock(&mut self) {
        match self.clock.take() {
            Some(Clock::DisplayLink(link)) => unsafe {
                let _: () = msg_send![&*link, invalidate];
            },
            Some(Clock::Timer(timer)) => timer.invalidate(),
            None => {}
        }
    }
}

impl Drop for MacOverlay {
    fn drop(&mut self) {
        // An invalidated clock is removed from the run loop; without this a
        // dropped overlay would leave a per-vsync callback running forever.
        self.stop_clock();
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
            // The entry gesture: the skull scales and fades in rather than
            // cutting into view. This fires on every keypress, so it is the
            // most-seen animation in the product, and a hard cut was the one
            // moment of the interaction that read as abrupt.
            //
            // Keyed off `visible` before the show path below sets it, so it
            // runs exactly once per appearance.
            if !self.visible {
                model.animator.trigger_entry(now);
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
                // The skull's commit gesture: jaw shuts through its release
                // envelope, plus one damped-spring pop (the settle).
                model.animator.trigger_settle(now);
            }
        }
        self.last_state = frame.state;

        // A host push always repaints once: state/detail/level changed, and
        // the animation clock only repaints what it knows moved.
        self.view.setNeedsDisplay(true);

        if !self.visible {
            self.place_panel();
            // orderFrontRegardless shows the panel with no activation path
            // at all (requirement #2 in the module doc).
            self.panel.orderFrontRegardless();
            self.visible = true;
        }
        self.start_clock();
        Ok(())
    }

    fn hide(&mut self) -> anyhow::Result<()> {
        // Hidden = zero cost: no clock, no scheduled work of any kind.
        self.stop_clock();
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

    fn set_audio_bands(&mut self, bands: [f32; 4]) {
        self.view.ivars().borrow_mut().bands = bands;
    }
}
