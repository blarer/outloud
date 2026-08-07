//! The menu-bar mark: the OutLoud skull, as pure geometry.
//!
//! # Why a skull and not the megaphone
//!
//! This was a megaphone horn with two sound arcs, and the module doc argued
//! at length that a horn-with-arcs silhouette could not be confused with the
//! waveform icons around it. The user reported the opposite: "it just looks
//! like a speaker icon so it's kinda confusing with the other audio apps".
//!
//! That argument lost to evidence. A menu bar is full of audio apps, and
//! every one of them is entitled to a horn: Music, Volume, AirPlay, Zoom,
//! Discord. Being a *nicer* horn does not help, because the user is not
//! comparing shapes, they are scanning for something that is not a horn.
//!
//! The skull is already this product's mascot: it is the on-screen overlay
//! ([`crate::skull`]), the thing that opens its jaw while you dictate. So
//! this is not a new idea to learn, it is the same identity in the one place
//! it was missing, and nothing else in a menu bar is a skull.
//!
//! # What survives 15 points
//!
//! Not the overlay skull. That one has articulated jaw, teeth, nasal
//! aperture, and eye glows, all of which turn to mud at menu-bar size for
//! the same reason the megaphone dropped the logo's third arc.
//!
//! What survives is the *silhouette*: a cranium that flares wide at the
//! temples and narrows to a jaw, with two eye sockets punched out of it. Two
//! dark holes in a pale rounded shape is the most recognisable part of a
//! skull at any size, which is why emoji and road signs both keep exactly
//! that and drop the rest.
//!
//! Drawn as one filled path with the sockets as reversed sub-paths, so the
//! holes are cut by the even-odd fill rule rather than painted over. Painting
//! over needs a background colour, and this glyph is a *template image*: the
//! system renders it from alpha alone and paints it white on a dark menu bar.
//! A hole must be a real hole or it fills in the moment the menu bar goes
//! dark.
//!
//! Same purity rule as before: no AppKit here, so the headless build compiles
//! it, CI asserts its properties without a display, and the Windows tray
//! renders the identical points instead of a hand-ported cousin that drifts.

use crate::layout::Point;

/// The mark's geometry: one filled outline with holes punched in it, all in
/// the same top-left-origin, y-down coordinate space.
///
/// Field names are deliberately shape-neutral rather than `horn`/`waves`:
/// the backends draw "fill this, then cut these", and naming them after the
/// current drawing means the next redesign is a rename across three
/// platforms instead of an edit here.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    /// The skull silhouette: cranium flaring at the temples down to a jaw,
    /// as a closed polygon (first point repeated last). Fill it.
    pub outline: Vec<Point>,
    /// Eye sockets, left then right, as closed polygons. Cut these OUT of
    /// the outline (even-odd fill), never paint over them: this is a
    /// template image, so a socket painted in a background colour fills
    /// solid the moment the menu bar is dark.
    pub holes: Vec<Vec<Point>>,
}

// The design, as fractions of a unit square (0.0..=1.0, y down). Fractions
// rather than absolute points so one set of numbers serves every glyph size
// a menu bar height or DPI setting asks for.

/// Half-width of the cranium at its widest (the temples), as a fraction.
const TEMPLE_HALF_W: f64 = 0.40;
/// Half-width at the jaw. Narrower than the temples: that taper is what
/// makes the silhouette read as a skull rather than a circle or a bear.
const JAW_HALF_W: f64 = 0.24;
/// Top of the cranium and bottom of the jaw.
const CROWN_Y: f64 = 0.04;
const CHIN_Y: f64 = 0.96;
/// Where the cranium stops being round and starts tapering to the jaw.
const CHEEK_Y: f64 = 0.58;
/// Horizontal centre.
const CX: f64 = 0.50;

/// Eye socket centres and radii. Large relative to the head, and set wide:
/// small eyes vanish at 15pt, and close-set eyes read as a bear or an owl.
/// Cartoon-skull proportions rather than anatomical ones, for the same
/// reason the overlay skull is a cartoon.
const EYE_DX: f64 = 0.175;
const EYE_CY: f64 = 0.44;
const EYE_RX: f64 = 0.115;
const EYE_RY: f64 = 0.135;

/// Segments per sampled curve. Enough that the cranium reads as round at
/// 2x scale without generating points a Win32 polygon call would choke on.
const SEGMENTS: usize = 24;

/// The mark in a unit square.
///
/// Curves are pre-sampled into line segments here rather than returned as
/// Bézier control points, so a backend with no curve support (Win32 tray
/// drawing) renders exactly the same shape as AppKit.
pub fn unit_mark() -> Mark {
    let mut outline = Vec::with_capacity(SEGMENTS + 8);

    // The cranium: a half-ellipse from the left cheek, over the crown, to
    // the right cheek. Walked left-to-right over the top.
    let rx = TEMPLE_HALF_W;
    let ry = CHEEK_Y - CROWN_Y;
    for i in 0..=SEGMENTS {
        // pi (left) -> 0 (right), i.e. over the top in a y-down space.
        let a = std::f64::consts::PI * (1.0 - i as f64 / SEGMENTS as f64);
        outline.push(Point {
            x: CX + rx * a.cos(),
            y: CHEEK_Y - ry * a.sin(),
        });
    }

    // Down the right cheek to the jaw, across the chin, and back up the
    // left. The chin corners are eased with one intermediate point each so
    // the jaw reads as a rounded box rather than a wedge.
    let jaw_top = CHEEK_Y;
    outline.push(Point {
        x: CX + JAW_HALF_W,
        y: jaw_top + (CHIN_Y - jaw_top) * 0.55,
    });
    outline.push(Point {
        x: CX + JAW_HALF_W * 0.80,
        y: CHIN_Y,
    });
    outline.push(Point {
        x: CX - JAW_HALF_W * 0.80,
        y: CHIN_Y,
    });
    outline.push(Point {
        x: CX - JAW_HALF_W,
        y: jaw_top + (CHIN_Y - jaw_top) * 0.55,
    });

    // Close the polygon explicitly. Backends differ on whether an unclosed
    // fill path is closed for you, and an open one leaves a visible notch
    // at the left cheek on the ones that do not.
    let first = outline[0];
    outline.push(first);

    let socket = |cx: f64| -> Vec<Point> {
        let mut pts: Vec<Point> = (0..SEGMENTS)
            .map(|i| {
                let a = i as f64 / SEGMENTS as f64 * std::f64::consts::TAU;
                Point {
                    x: cx + EYE_RX * a.cos(),
                    y: EYE_CY + EYE_RY * a.sin(),
                }
            })
            .collect();
        let first = pts[0];
        pts.push(first);
        pts
    };

    Mark {
        outline,
        holes: vec![socket(CX - EYE_DX), socket(CX + EYE_DX)],
    }
}

/// The mark scaled to fit a square of `size` points.
pub fn mark_in(size: f64) -> Mark {
    let scale = |p: &Point| Point {
        x: p.x * size,
        y: p.y * size,
    };
    let unit = unit_mark();
    Mark {
        outline: unit.outline.iter().map(scale).collect(),
        holes: unit
            .holes
            .iter()
            .map(|h| h.iter().map(scale).collect())
            .collect(),
    }
}

/// Glyph box size in points. Matches the SF Symbol point size the status
/// item used before, so the mark sits at the same visual weight as the
/// system icons beside it.
pub const GLYPH_SIZE: f64 = 15.0;

/// Stroke width, kept for backends that outline rather than fill.
///
/// The skull is a filled silhouette with cut holes, so nothing strokes it
/// today. Retained because the Windows tray path still references it and
/// removing it is a separate change from redrawing the mark.
pub const GLYPH_LINE_WIDTH: f64 = 1.6;

#[cfg(test)]
mod tests {
    use super::*;

    /// Both the outline and every socket must be explicitly closed.
    ///
    /// Backends disagree about whether an unclosed fill path is closed for
    /// you. On the ones that do not, the outline shows a notch at the left
    /// cheek and a socket leaks into the face.
    #[test]
    fn every_polygon_is_closed() {
        let m = unit_mark();
        for (name, poly) in std::iter::once(("outline", &m.outline)).chain(
            m.holes.iter().enumerate().map(|(i, h)| {
                if i == 0 {
                    ("left socket", h)
                } else {
                    ("right socket", h)
                }
            }),
        ) {
            let first = poly[0];
            let last = *poly.last().unwrap();
            assert!(
                (first.x - last.x).abs() < 1e-9 && (first.y - last.y).abs() < 1e-9,
                "{name} does not close"
            );
        }
    }

    /// Two sockets, and they must be HOLES rather than decoration.
    ///
    /// The count is the assertion with teeth: a skull with one eye is a
    /// different character, and a skull with none is a potato. Both must
    /// also sit inside the outline's bounding box, or they cut nothing.
    #[test]
    fn there_are_two_sockets_inside_the_head() {
        let m = unit_mark();
        assert_eq!(m.holes.len(), 2, "a skull has two eye sockets");

        let (mut lo_x, mut hi_x) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut lo_y, mut hi_y) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in &m.outline {
            lo_x = lo_x.min(p.x);
            hi_x = hi_x.max(p.x);
            lo_y = lo_y.min(p.y);
            hi_y = hi_y.max(p.y);
        }
        for (i, hole) in m.holes.iter().enumerate() {
            for p in hole {
                assert!(
                    p.x > lo_x && p.x < hi_x && p.y > lo_y && p.y < hi_y,
                    "socket {i} escapes the head at ({}, {})",
                    p.x,
                    p.y
                );
            }
        }
    }

    /// The sockets are set wide and left/right of centre.
    ///
    /// Close-set eyes read as an owl or a bear at 15pt, which is exactly the
    /// "looks like some other app's icon" problem this mark was redrawn to
    /// fix. Pinning the separation keeps a future tweak from drifting back.
    #[test]
    fn the_sockets_are_set_wide() {
        let m = unit_mark();
        let centre = |h: &Vec<Point>| h.iter().map(|p| p.x).sum::<f64>() / h.len() as f64;
        let left = centre(&m.holes[0]);
        let right = centre(&m.holes[1]);
        assert!(left < 0.5 && right > 0.5, "sockets must straddle centre");
        assert!(
            right - left > 0.25,
            "sockets too close together ({:.3}); reads as an animal, not a skull",
            right - left
        );
    }

    /// The silhouette must TAPER: wide at the temples, narrow at the jaw.
    ///
    /// This is the single property that makes the shape read as a skull
    /// rather than a circle, and it is the one a well-meaning simplification
    /// would remove first.
    #[test]
    fn the_head_tapers_to_a_jaw() {
        let m = unit_mark();
        let width_near = |y: f64| -> f64 {
            let band: Vec<f64> = m
                .outline
                .iter()
                .filter(|p| (p.y - y).abs() < 0.06)
                .map(|p| p.x)
                .collect();
            assert!(!band.is_empty(), "no outline points near y={y}");
            band.iter().fold(f64::NEG_INFINITY, |a, &x| a.max(x))
                - band.iter().fold(f64::INFINITY, |a, &x| a.min(x))
        };
        let temples = width_near(CHEEK_Y - 0.10);
        let jaw = width_near(CHIN_Y - 0.02);
        assert!(
            temples > jaw * 1.3,
            "skull must taper: temples {temples:.3} vs jaw {jaw:.3}"
        );
    }

    /// The whole mark stays inside its box at the size actually drawn.
    ///
    /// Nothing strokes this mark, so the old margin-for-a-stroke argument is
    /// gone; what remains is that a filled shape which touches the edge gets
    /// visually clipped against the menu bar's own padding.
    #[test]
    fn scaling_stays_inside_the_glyph_box() {
        let m = mark_in(GLYPH_SIZE);
        for p in m.outline.iter().chain(m.holes.iter().flatten()) {
            assert!(
                p.x >= 0.0 && p.x <= GLYPH_SIZE && p.y >= 0.0 && p.y <= GLYPH_SIZE,
                "point ({}, {}) escapes the {GLYPH_SIZE}pt box",
                p.x,
                p.y
            );
        }
    }

    /// Scaling is uniform: the unit design times the size, nothing else.
    #[test]
    fn scaling_is_proportional() {
        let unit = unit_mark();
        let scaled = mark_in(GLYPH_SIZE);
        assert_eq!(unit.outline.len(), scaled.outline.len());
        for (u, sc) in unit.outline.iter().zip(scaled.outline.iter()) {
            assert!((u.x * GLYPH_SIZE - sc.x).abs() < 1e-9);
            assert!((u.y * GLYPH_SIZE - sc.y).abs() < 1e-9);
        }
    }

    /// The mark is no longer a megaphone.
    ///
    /// The previous mark was a horn with sound arcs, and its module doc
    /// argued it could not be confused with the audio icons around it. The
    /// user reported exactly that confusion, which is how this became a
    /// skull. This test exists so the reasoning is not quietly re-litigated:
    /// the old shape had a rightward-pointing widest point and two open
    /// polylines, and a skull has neither.
    #[test]
    fn nothing_here_describes_a_megaphone() {
        let m = unit_mark();
        // A horn's widest span is at its mouth, off to one side. A skull's
        // is at the temples, straddling the centre.
        let widest_y = {
            let mut best = (0.0f64, 0.0f64);
            for p in &m.outline {
                let span = m
                    .outline
                    .iter()
                    .filter(|q| (q.y - p.y).abs() < 0.04)
                    .fold(f64::NEG_INFINITY, |a, q| a.max(q.x))
                    - m.outline
                        .iter()
                        .filter(|q| (q.y - p.y).abs() < 0.04)
                        .fold(f64::INFINITY, |a, q| a.min(q.x));
                if span > best.1 {
                    best = (p.y, span);
                }
            }
            best.0
        };
        assert!(
            widest_y < CHEEK_Y,
            "the widest part must be the cranium, not a horn mouth"
        );
        // Every sub-path is a closed polygon; the megaphone's arcs were open.
        for hole in &m.holes {
            assert!(hole.len() > 3, "a socket must be a polygon, not a stroke");
        }
    }
}
