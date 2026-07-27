//! The macOS overlay: a borderless, non-activating `NSPanel` drawn with a
//! single custom `NSView`.
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
//!    with no interactive controls yet, so every pixel is click-through and
//!    a click "on" the overlay lands in the app beneath, preserving the
//!    user's focus and caret. When an interactive error-action button is
//!    added, this becomes a per-region hit-test rather than a blanket
//!    ignore.
//!
//! Rendering is immediate-mode `drawRect:` with `NSBezierPath` — a state
//! dot, a level meter, and two text runs. No Core Animation, no layout
//! engine: the overlay redraws at most at meter rate (~30 Hz) while
//! listening and is otherwise static, so the simplest possible pipeline is
//! also the cheapest.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSPanel, NSScreen, NSStatusWindowLevel, NSStringDrawing,
    NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

use crate::layout::{place, Anchor, Point, Rect, Size};
use crate::state::OverlayState;
use crate::{Overlay, OverlayFrame};

/// Fixed overlay size in points. Small enough to be glanceable, wide enough
/// for a ~40-character partial tail. A future pass can size-to-fit; a fixed
/// size keeps positioning deterministic and testable for now.
const OVERLAY_SIZE: Size = Size {
    width: 340.0,
    height: 72.0,
};

/// How many characters of the partial transcription tail to show. The
/// committed text lives in the target field; the overlay only proves that
/// recognition is keeping up, so the tail is all that matters.
const TAIL_CHARS: usize = 44;

/// What the view draws. A plain snapshot struct so `drawRect:` reads one
/// coherent frame with no partially-updated state.
#[derive(Default)]
struct RenderData {
    state: Option<OverlayState>,
    level: f32,
    tail: String,
    detail: String,
    /// Monotonic redraw counter; drives the idle "breathing" of the meter
    /// bars so the listening state visibly means "live", not "frozen".
    tick: u64,
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
    #[ivars = RefCell<RenderData>]
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

impl OverlayView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(RenderData::default()));
        unsafe { msg_send![super(this), init] }
    }

    /// Per-state accent color. Matches the glyph column of the state table:
    /// red while the mic is hot, amber for machine-working states, red for
    /// error, gray for no-permission.
    fn accent(state: OverlayState, tick: u64) -> Retained<NSColor> {
        {
            match state {
                OverlayState::Listening => {
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.96, 0.26, 0.21, 1.0)
                }
                OverlayState::Transcribing => {
                    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.72, 0.0, 1.0)
                }
                OverlayState::Error => {
                    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.23, 0.19, 1.0)
                }
                OverlayState::NoPermission => {
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.62, 0.62, 0.66, 1.0)
                }
                // "Pulsing" per the state table: alpha breathes with the tick.
                OverlayState::ModelLoading => {
                    let phase = (tick as f64 * 0.15).sin() * 0.35 + 0.65;
                    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.72, 0.0, phase)
                }
                _ => NSColor::colorWithSRGBRed_green_blue_alpha(0.62, 0.62, 0.66, 1.0),
            }
        }
    }

    fn draw(&self) {
        let data = self.ivars().borrow();
        let Some(state) = data.state else { return };
        let bounds = self.bounds();

        {
            // Backdrop: a dark rounded card. Dark regardless of system
            // appearance so it reads over any content, like a caption.
            NSColor::colorWithSRGBRed_green_blue_alpha(0.11, 0.11, 0.13, 0.92).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, 14.0, 14.0).fill();

            let accent = Self::accent(state, data.tick);

            // State dot, vertically centered in the label row.
            accent.setFill();
            let dot = NSRect::new(NSPoint::new(16.0, 15.0), NSSize::new(10.0, 10.0));
            NSBezierPath::bezierPathWithOvalInRect(dot).fill();

            // Label row: the state's short name or the host's detail line
            // ("situation → action" for errors, per UX principle 4).
            let label = if data.detail.is_empty() {
                state.label().to_string()
            } else {
                data.detail.clone()
            };
            let white = NSColor::colorWithSRGBRed_green_blue_alpha(0.94, 0.94, 0.96, 1.0);
            draw_text(&label, NSPoint::new(34.0, 12.0), 13.0, &white, false);

            match state {
                OverlayState::Listening => {
                    self.draw_meter(&data, &accent, bounds);
                    if !data.tail.is_empty() {
                        // The partial tail, dimmed: provisional text is
                        // visually distinct from committed text.
                        let dim = NSColor::colorWithSRGBRed_green_blue_alpha(0.75, 0.75, 0.78, 0.9);
                        draw_text(&data.tail, NSPoint::new(16.0, 32.0), 12.0, &dim, true);
                    }
                }
                _ => {
                    // Non-listening states get a thin static accent strip
                    // where the meter would be, so the card's shape does not
                    // jump between states.
                    accent.colorWithAlphaComponent(0.35).setFill();
                    let strip = NSRect::new(
                        NSPoint::new(16.0, 58.0),
                        NSSize::new(bounds.size.width - 32.0, 3.0),
                    );
                    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(strip, 1.5, 1.5).fill();
                }
            }
        }
    }

    /// The live level meter: a row of bars whose height tracks the shaped
    /// mic level, with a per-bar phase wobble so the meter reads as alive
    /// even at constant level. This is the "microphone state is never
    /// ambiguous" surface from UX principle 3.
    fn draw_meter(&self, data: &RenderData, accent: &NSColor, bounds: NSRect) {
        let level = crate::layout::shape_level(data.level) as f64;
        let bars = 24usize;
        let gap = 3.0;
        let region_w = bounds.size.width - 32.0;
        let bar_w = (region_w - gap * (bars as f64 - 1.0)) / bars as f64;
        let max_h = 12.0;
        let base_y = 64.0; // bottom edge of the bars (flipped coords)
        {
            accent.setFill();
            for i in 0..bars {
                // Deterministic wobble: cheap, loopable, no RNG state.
                let phase = (data.tick as f64 * 0.35 + i as f64 * 0.9).sin() * 0.5 + 0.5;
                let h = (2.0 + max_h * level * (0.45 + 0.55 * phase)).min(max_h);
                let x = 16.0 + i as f64 * (bar_w + gap);
                let r = NSRect::new(NSPoint::new(x, base_y - h), NSSize::new(bar_w, h));
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 1.0, 1.0).fill();
            }
        }
    }
}

/// Draw one run of text with the system (or monospaced) font. Isolated so
/// the attribute-dictionary unsafety lives in one function.
fn draw_text(text: &str, at: NSPoint, size: f64, color: &NSColor, mono: bool) {
    unsafe {
        let font = if mono {
            NSFont::monospacedSystemFontOfSize_weight(size, 0.0)
        } else {
            NSFont::systemFontOfSize(size)
        };
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

/// The macOS implementation of [`Overlay`].
pub struct MacOverlay {
    panel: Retained<OverlayPanel>,
    view: Retained<OverlayView>,
    mtm: MainThreadMarker,
    visible: bool,
}

impl MacOverlay {
    pub fn new(mtm: MainThreadMarker) -> anyhow::Result<Self> {
        let content = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(OVERLAY_SIZE.width, OVERLAY_SIZE.height),
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
            // The rounded card is drawn by the view; the window itself must
            // be a transparent, shadowless container.
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setHasShadow(true);
            // Click-through (requirement #4).
            panel.setIgnoresMouseEvents(true);
            // Never appear in the window cycle or the Dock's window list;
            // this is an indicator, not a window the user manages.
            panel.setHidesOnDeactivate(false);
        }
        let view = OverlayView::new(mtm);
        panel.setContentView(Some(&view));
        Ok(MacOverlay {
            panel,
            view,
            mtm,
            visible: false,
        })
    }

    /// Convert a top-left-origin layout rect to AppKit's bottom-left-origin
    /// screen frame. This is the one place the flip happens (see the
    /// `layout` module doc for why).
    fn to_appkit(&self, r: Rect) -> NSRect {
        let primary_h = NSScreen::screens(self.mtm)
            .iter()
            .next()
            .map(|s| s.frame().size.height)
            .unwrap_or(0.0);
        NSRect::new(
            NSPoint::new(r.origin.x, primary_h - r.origin.y - r.size.height),
            NSSize::new(r.size.width, r.size.height),
        )
    }

    /// The screen whose visible frame contains the anchor, in the crate's
    /// top-left convention. Falls back to the main screen.
    fn screen_bounds_for(&self, anchor: Anchor) -> Rect {
        let screens = NSScreen::screens(self.mtm);
        let primary_h = screens
            .iter()
            .next()
            .map(|s| s.frame().size.height)
            .unwrap_or(0.0);
        let to_rect = |f: NSRect| {
            Rect::new(
                f.origin.x,
                primary_h - f.origin.y - f.size.height,
                f.size.width,
                f.size.height,
            )
        };
        let probe = match anchor {
            Anchor::Caret(r) => Some(Point {
                x: r.origin.x,
                y: r.origin.y,
            }),
            Anchor::Cursor(p) => Some(p),
            Anchor::Corner => None,
        };
        let mut fallback = None;
        for s in screens.iter() {
            // visibleFrame excludes the menu bar and Dock, so the overlay
            // never hides beneath either.
            let r = to_rect(s.visibleFrame());
            if fallback.is_none() {
                fallback = Some(r);
            }
            if let Some(p) = probe {
                if r.contains(p) {
                    return r;
                }
            }
        }
        fallback.unwrap_or(Rect::new(0.0, 0.0, 1440.0, 900.0))
    }

    /// Resolve `Anchor::Cursor` requests when the caller has no position:
    /// current mouse location in top-left coordinates.
    pub fn mouse_anchor(&self) -> Anchor {
        let p = NSEvent::mouseLocation();
        let primary_h = NSScreen::screens(self.mtm)
            .iter()
            .next()
            .map(|s| s.frame().size.height)
            .unwrap_or(0.0);
        Anchor::Cursor(Point {
            x: p.x,
            y: primary_h - p.y,
        })
    }
}

impl Overlay for MacOverlay {
    fn render(&mut self, frame: &OverlayFrame) -> anyhow::Result<()> {
        if !frame.state.overlay_visible() {
            return self.hide();
        }

        {
            let mut data = self.view.ivars().borrow_mut();
            data.state = Some(frame.state);
            data.level = frame.audio_level;
            data.detail = frame.detail.clone().unwrap_or_default();
            data.tick = data.tick.wrapping_add(1);
            // Keep only the tail: committed text lives in the target field.
            let chars: Vec<char> = frame.partial_text.chars().collect();
            data.tail = if chars.len() > TAIL_CHARS {
                let cut: String = chars[chars.len() - TAIL_CHARS..].iter().collect();
                format!("…{cut}")
            } else {
                frame.partial_text.clone()
            };
        }

        let screen = self.screen_bounds_for(frame.anchor);
        let placed = place(frame.anchor, OVERLAY_SIZE, screen);
        let appkit_frame = self.to_appkit(placed);
        self.panel.setFrame_display(appkit_frame, true);
        self.view.setNeedsDisplay(true);
        if !self.visible {
            // orderFrontRegardless shows the panel with no activation path
            // at all (requirement #2 in the module doc).
            self.panel.orderFrontRegardless();
            self.visible = true;
        }
        Ok(())
    }

    fn hide(&mut self) -> anyhow::Result<()> {
        if self.visible {
            self.panel.orderOut(None);
            self.visible = false;
        }
        Ok(())
    }

    fn is_visible(&self) -> bool {
        self.visible
    }
}
