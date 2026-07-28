//! The menu-bar mark: the OutLoud megaphone, as pure geometry.
//!
//! This is the product's brand mark (`docs/assets/logo.svg`, also the app
//! icon and the README image) reduced to what survives at menu-bar size: a
//! filled horn pointing right and two sound arcs. The logo's handle and
//! third arc are deliberately dropped — at ~15 points they turn to mud, and
//! SF Symbols' own speaker glyphs make the same cut (`speaker.wave.2.fill`
//! keeps two waves, not three, for exactly this reason). Simplified, not
//! scaled.
//!
//! Why a hand-drawn path instead of an SF Symbol like `megaphone.fill`:
//! an SF Symbol exists only on Apple platforms, and the point of keeping
//! the geometry here, apart from AppKit, is that it is testable on every
//! platform including headless CI, and the Windows tray backend can consume
//! the identical points rather than reimplementing a horn that drifts
//! subtly from the macOS one. An SF Symbol would break that shared-geometry
//! property, so it stays a documented fallback, not the implementation.
//!
//! Recognisability across the room: the old mark argued that a menu bar is
//! full of waveform icons and the glyph must not be confusable with them.
//! That argument survives the redesign: the silhouette here is a *horn with
//! arcs*, not a bar-graph waveform, and the filled horn gives it a solid
//! visual anchor no waveform icon has. It answers "is it on?" by tint
//! (see [`crate::menu::glyph_tint`]) exactly as before — the shape is
//! constant across states, the colour carries the state, so the geometry
//! needs no per-state variants.
//!
//! Two kinds of path, because they are drawn differently:
//!
//! * [`Mark::horn`] is a closed polygon meant to be **filled**. A stroked
//!   outline horn collapses into scribble at 15pt; a filled one reads.
//! * [`Mark::waves`] are open polylines meant to be **stroked** with round
//!   caps. Arcs are pre-sampled into line segments here rather than
//!   returned as curve control points, so a backend with no Bézier support
//!   (Win32 tray drawing) renders the same shape as AppKit.

use crate::layout::Point;

/// The mark's geometry: one filled polygon plus stroked arcs, all in the
/// same top-left-origin, y-down coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    /// The megaphone horn, pointing right, as a closed polygon (first point
    /// repeated last). Fill it.
    pub horn: Vec<Point>,
    /// Sound arcs radiating from the horn mouth, inner first. Stroke them
    /// with round caps.
    pub waves: Vec<Vec<Point>>,
}

// The design, as fractions of a unit square (0.0..=1.0, y down). Fractions
// rather than absolute points so one set of numbers serves every glyph size
// a menu bar height or DPI setting asks for.
//
// The shape is the logo's horn path (`M34 52 L34 76 L48 76 L70 92 L70 36
// L48 52 Z` in a 128 box) re-proportioned for a square glyph: throat at the
// left, bell flaring to the right, mouth just past centre so the waves get
// the right half of the box.

/// Left edge of the horn's throat.
const THROAT_LEFT: f64 = 0.08;
/// Vertical extent of the throat (the narrow back of the horn).
const THROAT_TOP: f64 = 0.34;
const THROAT_BOTTOM: f64 = 0.66;
/// Where the throat ends and the bell starts flaring.
const BELL_START: f64 = 0.30;
/// The horn's mouth: its x position and vertical extent.
const MOUTH_X: f64 = 0.52;
const MOUTH_TOP: f64 = 0.16;
const MOUTH_BOTTOM: f64 = 0.84;

/// The arcs' shared centre. On the horn's mouth line, at the glyph's
/// vertical centre, so the waves visibly *emanate from the horn* rather
/// than floating beside it.
const WAVE_CENTRE: Point = Point { x: 0.50, y: 0.50 };
/// Arc radii, inner then outer. Two arcs, not the logo's three: the third
/// does not survive 15pt (see module doc).
const WAVE_RADII: [f64; 2] = [0.22, 0.40];
/// Half-angle of each arc, radians. 45 degrees either side of horizontal
/// matches the bell's flare angle, which is what makes the waves read as
/// coming *out of* the horn.
const WAVE_HALF_ANGLE: f64 = std::f64::consts::FRAC_PI_4;
/// Line segments per arc. Eight is visually indistinguishable from a true
/// arc at menu-bar size and keeps the polyline cheap for the Win32 backend.
const WAVE_SEGMENTS: usize = 8;

/// The mark in the unit square, y down.
pub fn unit_mark() -> Mark {
    let horn = vec![
        Point {
            x: THROAT_LEFT,
            y: THROAT_TOP,
        },
        Point {
            x: THROAT_LEFT,
            y: THROAT_BOTTOM,
        },
        Point {
            x: BELL_START,
            y: THROAT_BOTTOM,
        },
        Point {
            x: MOUTH_X,
            y: MOUTH_BOTTOM,
        },
        Point {
            x: MOUTH_X,
            y: MOUTH_TOP,
        },
        Point {
            x: BELL_START,
            y: THROAT_TOP,
        },
        // Closed explicitly, so a backend that draws polylines verbatim
        // (rather than calling a close-path primitive) still closes it.
        Point {
            x: THROAT_LEFT,
            y: THROAT_TOP,
        },
    ];

    let waves = WAVE_RADII
        .iter()
        .map(|&r| {
            (0..=WAVE_SEGMENTS)
                .map(|i| {
                    // Sweep from -half to +half angle around horizontal.
                    // cos/sin in y-down space: positive angles bow the arc
                    // downward, so the sweep is symmetric about the centre
                    // line, which the tests pin.
                    let t = -WAVE_HALF_ANGLE
                        + (i as f64 / WAVE_SEGMENTS as f64) * 2.0 * WAVE_HALF_ANGLE;
                    Point {
                        x: WAVE_CENTRE.x + r * t.cos(),
                        y: WAVE_CENTRE.y + r * t.sin(),
                    }
                })
                .collect()
        })
        .collect();

    Mark { horn, waves }
}

/// The mark scaled to fit a square of `size` points. The design already
/// leaves enough margin that a stroke of [`GLYPH_LINE_WIDTH`] does not clip
/// at [`GLYPH_SIZE`]; `scaling_leaves_room_for_the_stroke` asserts it.
pub fn mark_in(size: f64) -> Mark {
    let scale = |p: &Point| Point {
        x: p.x * size,
        y: p.y * size,
    };
    let unit = unit_mark();
    Mark {
        horn: unit.horn.iter().map(scale).collect(),
        waves: unit
            .waves
            .iter()
            .map(|w| w.iter().map(scale).collect())
            .collect(),
    }
}

/// Glyph box size in points. Matches the SF Symbol point size the status
/// item used before, so the mark sits at the same visual weight as the
/// system icons beside it.
pub const GLYPH_SIZE: f64 = 15.0;

/// Stroke width for the wave arcs. Heavier than the old star's stroke
/// because two short arcs carry less ink than a five-line star and would
/// otherwise disappear next to the filled horn.
pub const GLYPH_LINE_WIDTH: f64 = 1.6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_horn_is_a_closed_polygon() {
        let m = unit_mark();
        let first = m.horn[0];
        let last = *m.horn.last().unwrap();
        assert!((first.x - last.x).abs() < 1e-9, "horn does not close");
        assert!((first.y - last.y).abs() < 1e-9, "horn does not close");
        // Six distinct vertices plus the closing repeat: the simplified
        // horn. More would mean detail crept back in that 15pt cannot
        // carry; fewer would mean the bell/throat distinction was lost.
        assert_eq!(m.horn.len(), 7);
    }

    #[test]
    fn the_horn_points_right() {
        // Orientation is a product decision (the logo's horn points right,
        // "speech travelling toward the text"), and it would survive an
        // accidental x-flip in every other test, so it is pinned: the
        // widest part of the horn (the mouth) must be on the RIGHT.
        let m = unit_mark();
        let mouth_x = m.horn.iter().fold(f64::NEG_INFINITY, |acc, p| acc.max(p.x));
        let throat_x = m.horn.iter().fold(f64::INFINITY, |acc, p| acc.min(p.x));
        assert!(mouth_x > 0.5, "mouth is not on the right half: {m:?}");
        assert!(throat_x < 0.2, "throat is not on the left: {m:?}");
        // The mouth is taller than the throat: that flare is what makes it
        // a megaphone rather than a rectangle with a nub. Measured from the
        // geometry, not the constants, so a bad edit cannot pass by
        // agreeing with itself.
        let height_at = |x: f64| {
            let ys: Vec<f64> = m
                .horn
                .iter()
                .filter(|p| (p.x - x).abs() < 1e-9)
                .map(|p| p.y)
                .collect();
            ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                - ys.iter().cloned().fold(f64::INFINITY, f64::min)
        };
        assert!(height_at(mouth_x) > 1.5 * height_at(throat_x));
    }

    #[test]
    fn the_horn_has_area() {
        // Shoelace: a degenerate (self-intersecting or zero-area) polygon
        // fills as nothing or as garbage, and no compile-time check would
        // notice.
        let m = unit_mark();
        let mut twice_area = 0.0;
        for pair in m.horn.windows(2) {
            twice_area += pair[0].x * pair[1].y - pair[1].x * pair[0].y;
        }
        assert!(
            twice_area.abs() > 0.2,
            "horn area is implausibly small: {twice_area}"
        );
    }

    #[test]
    fn the_waves_emanate_from_the_horn_mouth() {
        let m = unit_mark();
        assert_eq!(m.waves.len(), 2, "two arcs, by design (see module doc)");
        for (i, wave) in m.waves.iter().enumerate() {
            assert_eq!(wave.len(), WAVE_SEGMENTS + 1);
            // Every sample sits on its circle around the wave centre.
            for p in wave {
                let r = ((p.x - WAVE_CENTRE.x).powi(2) + (p.y - WAVE_CENTRE.y).powi(2)).sqrt();
                assert!((r - WAVE_RADII[i]).abs() < 1e-9, "point off its arc: {p:?}");
            }
            // Arcs open to the RIGHT of the horn mouth: every point at or
            // past the mouth's x. Waves behind the horn would read as the
            // megaphone sucking sound in.
            for p in wave {
                assert!(p.x >= MOUTH_X - 1e-9, "wave point behind the mouth: {p:?}");
            }
            // Symmetric about the centre line: first and last points mirror
            // in y. Asymmetric arcs read as a rendering bug at small sizes.
            let (first, last) = (wave[0], *wave.last().unwrap());
            assert!((first.y + last.y - 1.0).abs() < 1e-9, "arc not symmetric");
        }
        // Inner arc before outer, measured from the samples: the order is
        // part of the contract (a Windows backend may thin the outer arc).
        let r_of = |w: &[Point]| {
            let p = w[0];
            ((p.x - WAVE_CENTRE.x).powi(2) + (p.y - WAVE_CENTRE.y).powi(2)).sqrt()
        };
        assert!(r_of(&m.waves[0]) < r_of(&m.waves[1]));
    }

    #[test]
    fn scaling_leaves_room_for_the_stroke() {
        // A stroke that clips at the edge reads as a broken icon, and the
        // menu bar gives no room to fix it visually. The horn is filled
        // (no stroke overhang) but is held to the same bound for slack.
        let m = mark_in(GLYPH_SIZE);
        let lo = GLYPH_LINE_WIDTH / 2.0;
        let hi = GLYPH_SIZE - GLYPH_LINE_WIDTH / 2.0;
        let all = m.horn.iter().chain(m.waves.iter().flatten());
        for p in all {
            assert!(p.x >= lo - 1e-9 && p.x <= hi + 1e-9, "{p:?}");
            assert!(p.y >= lo - 1e-9 && p.y <= hi + 1e-9, "{p:?}");
        }
    }

    #[test]
    fn nothing_here_describes_a_star() {
        // Regression pin for the rename: the pentagram was Hexavoice's mark
        // and its geometry must not resurface. A five-pointed star's
        // outline visits five vertices; the horn has six plus the close,
        // and no path in the mark self-intersects the way a unicursal star
        // must. Cheap proxy: the horn polygon is convex-ish enough that
        // consecutive edge cross-products never alternate sign more than
        // the flare requires. Simpler and sufficient: just assert the
        // vertex count is not five.
        assert_ne!(unit_mark().horn.len() - 1, 5, "the star is back");
    }
}
