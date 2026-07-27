//! Overlay positioning math, kept pure so it is unit-testable on every
//! platform (no display needed, no AppKit types).
//!
//! Coordinate convention: **top-left origin, y grows downward**, matching
//! what the Accessibility API returns for `AXBoundsForRange` and what mouse
//! APIs report. The macOS backend converts to AppKit's bottom-left-origin
//! screen coordinates in exactly one place, so the flipping bug every mac
//! overlay ships once lives behind one tested seam instead of being smeared
//! through the layout code.

/// A point in top-left-origin global screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A size in screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// A rectangle in top-left-origin global screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Rect {
            origin: Point { x, y },
            size: Size { width, height },
        }
    }

    pub fn max_x(&self) -> f64 {
        self.origin.x + self.size.width
    }

    pub fn max_y(&self) -> f64 {
        self.origin.y + self.size.height
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.origin.x && p.x < self.max_x() && p.y >= self.origin.y && p.y < self.max_y()
    }
}

/// Where the caller wants the overlay, best knowledge first.
///
/// The fallback ladder mirrors the product requirement: near the caret when
/// ax-edit could resolve `AXBoundsForRange` on the focused field, else near
/// the mouse cursor (the user's likely locus of attention), else a fixed
/// screen corner that at least never covers the middle of anyone's work.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    /// Bounds of the insertion caret (or selection) in the focused field,
    /// from the AX parameterized attribute. The most precise anchor: the
    /// overlay sits just below the text being dictated into.
    Caret(Rect),
    /// The mouse cursor position. Second best: where the user is looking
    /// when we cannot see the caret.
    Cursor(Point),
    /// Bottom-right corner of the screen. The anchor of last resort, and
    /// also what a user who set "overlay position: corner" in advanced
    /// settings gets unconditionally.
    Corner,
}

/// Gap between the anchor and the overlay, so the overlay never touches the
/// text it is annotating.
const ANCHOR_GAP: f64 = 8.0;
/// Minimum distance from any screen edge. Keeps the overlay clear of rounded
/// display corners, the Dock, and the notch shadow.
const SCREEN_MARGIN: f64 = 12.0;

/// Compute the overlay's frame (top-left-origin) for an anchor, an overlay
/// size, and the visible bounds of the screen the anchor falls on.
///
/// Placement policy:
/// * `Caret`: directly **below** the caret, left-aligned to it — below,
///   because covering the line being typed would defeat the overlay's whole
///   purpose. If below does not fit, flip above.
/// * `Cursor`: below-right of the pointer with the same flip rule, offset so
///   the pointer itself never overlaps the overlay.
/// * `Corner`: bottom-right, inset by the margin.
/// * In every case the result is clamped fully inside `screen`, so a caret
///   at the screen edge can never push the overlay off-screen.
pub fn place(anchor: Anchor, overlay: Size, screen: Rect) -> Rect {
    let (mut x, mut y) = match anchor {
        Anchor::Caret(caret) => {
            let below = caret.max_y() + ANCHOR_GAP;
            let y = if below + overlay.height <= screen.max_y() - SCREEN_MARGIN {
                below
            } else {
                // Flip above the caret rather than covering the text line.
                caret.origin.y - ANCHOR_GAP - overlay.height
            };
            (caret.origin.x, y)
        }
        Anchor::Cursor(p) => {
            let below = p.y + ANCHOR_GAP * 2.0;
            let y = if below + overlay.height <= screen.max_y() - SCREEN_MARGIN {
                below
            } else {
                p.y - ANCHOR_GAP * 2.0 - overlay.height
            };
            (p.x + ANCHOR_GAP, y)
        }
        Anchor::Corner => (
            screen.max_x() - SCREEN_MARGIN - overlay.width,
            screen.max_y() - SCREEN_MARGIN - overlay.height,
        ),
    };

    // Clamp inside the screen's visible bounds. Order matters: clamping the
    // max edge first and the min edge second means an overlay wider than the
    // screen pins to the left/top rather than hanging off the right/bottom.
    x = x.min(screen.max_x() - SCREEN_MARGIN - overlay.width);
    x = x.max(screen.origin.x + SCREEN_MARGIN);
    y = y.min(screen.max_y() - SCREEN_MARGIN - overlay.height);
    y = y.max(screen.origin.y + SCREEN_MARGIN);

    Rect {
        origin: Point { x, y },
        size: overlay,
    }
}

/// Shape a raw audio level for display.
///
/// Raw RMS microphone levels sit almost entirely in the bottom of the 0..1
/// range for normal speech, which renders as a meter that barely moves. A
/// square-root curve expands the quiet end so the meter visibly tracks the
/// voice, which is the meter's entire job: proof the mic is hearing you.
pub fn shape_level(raw: f32) -> f32 {
    raw.clamp(0.0, 1.0).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        origin: Point { x: 0.0, y: 0.0 },
        size: Size {
            width: 1440.0,
            height: 900.0,
        },
    };
    const OVERLAY: Size = Size {
        width: 320.0,
        height: 56.0,
    };

    fn assert_on_screen(r: Rect) {
        assert!(r.origin.x >= SCREEN.origin.x + SCREEN_MARGIN, "{r:?}");
        assert!(r.origin.y >= SCREEN.origin.y + SCREEN_MARGIN, "{r:?}");
        assert!(r.max_x() <= SCREEN.max_x() - SCREEN_MARGIN, "{r:?}");
        assert!(r.max_y() <= SCREEN.max_y() - SCREEN_MARGIN, "{r:?}");
    }

    #[test]
    fn caret_anchor_places_below_and_left_aligned() {
        let caret = Rect::new(400.0, 300.0, 2.0, 18.0);
        let r = place(Anchor::Caret(caret), OVERLAY, SCREEN);
        assert_eq!(r.origin.x, 400.0);
        assert_eq!(r.origin.y, caret.max_y() + ANCHOR_GAP);
        assert_on_screen(r);
    }

    #[test]
    fn caret_near_bottom_flips_above_instead_of_covering_the_line() {
        let caret = Rect::new(400.0, 860.0, 2.0, 18.0);
        let r = place(Anchor::Caret(caret), OVERLAY, SCREEN);
        // Must be entirely above the caret's top edge: overlapping the text
        // line being dictated into defeats the overlay's purpose.
        assert!(r.max_y() <= caret.origin.y, "{r:?} covers the caret line");
        assert_on_screen(r);
    }

    #[test]
    fn caret_at_right_edge_clamps_fully_on_screen() {
        let caret = Rect::new(1430.0, 300.0, 2.0, 18.0);
        let r = place(Anchor::Caret(caret), OVERLAY, SCREEN);
        assert_on_screen(r);
    }

    #[test]
    fn cursor_anchor_never_overlaps_the_pointer() {
        let p = Point { x: 700.0, y: 400.0 };
        let r = place(Anchor::Cursor(p), OVERLAY, SCREEN);
        assert!(!r.contains(p), "overlay {r:?} sits under the pointer");
        assert_on_screen(r);
    }

    #[test]
    fn cursor_near_bottom_flips_above() {
        let p = Point { x: 700.0, y: 880.0 };
        let r = place(Anchor::Cursor(p), OVERLAY, SCREEN);
        assert!(r.max_y() <= p.y, "{r:?}");
        assert_on_screen(r);
    }

    #[test]
    fn corner_anchor_is_bottom_right_inset_by_margin() {
        let r = place(Anchor::Corner, OVERLAY, SCREEN);
        assert_eq!(r.max_x(), SCREEN.max_x() - SCREEN_MARGIN);
        assert_eq!(r.max_y(), SCREEN.max_y() - SCREEN_MARGIN);
    }

    #[test]
    fn secondary_screen_with_negative_origin_is_respected() {
        // Multi-monitor: a display left of the primary has negative x in
        // global coordinates. Placement must clamp into *that* screen.
        let screen = Rect::new(-1920.0, 0.0, 1920.0, 1080.0);
        let caret = Rect::new(-1910.0, 500.0, 2.0, 18.0);
        let r = place(Anchor::Caret(caret), OVERLAY, screen);
        assert!(r.origin.x >= screen.origin.x + SCREEN_MARGIN, "{r:?}");
        assert!(r.max_x() <= screen.max_x() - SCREEN_MARGIN, "{r:?}");
    }

    #[test]
    fn oversized_overlay_pins_to_top_left_not_off_screen() {
        let huge = Size {
            width: 5000.0,
            height: 5000.0,
        };
        let r = place(Anchor::Corner, huge, SCREEN);
        // Wider than the screen: the min-edge clamp must win so at least the
        // top-left of the surface is visible.
        assert_eq!(r.origin.x, SCREEN.origin.x + SCREEN_MARGIN);
        assert_eq!(r.origin.y, SCREEN.origin.y + SCREEN_MARGIN);
    }

    #[test]
    fn level_shaping_is_monotone_and_clamped() {
        assert_eq!(shape_level(-1.0), 0.0);
        assert_eq!(shape_level(2.0), 1.0);
        assert!(shape_level(0.04) > 0.04, "quiet end must be expanded");
        let mut prev = 0.0;
        for i in 0..=100 {
            let v = shape_level(i as f32 / 100.0);
            assert!(v >= prev);
            prev = v;
        }
    }
}
