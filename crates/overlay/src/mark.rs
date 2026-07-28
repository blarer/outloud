//! The menu-bar mark: a pentagram, as pure geometry.
//!
//! Why a hand-drawn path instead of an SF Symbol: there is no pentagram in
//! SF Symbols, and the product wants a mark nobody confuses with the dozen
//! other waveform-shaped audio icons already in a typical menu bar. That
//! recognisability is the whole job of this surface -- it answers "is it
//! on?" from across the screen, before any click.
//!
//! The geometry lives here, apart from AppKit, for the same reason the
//! layout math does: it is testable on every platform including headless CI,
//! and the Windows tray backend can consume the identical points rather than
//! reimplementing a star that drifts subtly from the macOS one.
//!
//! The path is the classic unicursal five-pointed star: five vertices on a
//! circle, connected in step-two order (0-2-4-1-3-0), which draws the whole
//! figure without lifting the pen and leaves the pentagon hole in the middle.
//! Stroked rather than filled, because at menu-bar size a filled star loses
//! the interior lines that make it read as a pentagram rather than as a
//! generic star.

use crate::layout::Point;

/// How many points the star has. Five, or it is not a pentagram.
const POINTS: usize = 5;

/// The order vertices are connected in. Step two around the circle is what
/// makes the stroke unicursal and produces the interior pentagon.
const STEP: usize = 2;

/// The pentagram's outline, in order, as a closed polyline on a unit circle
/// centred at the origin.
///
/// Y grows downward, matching this crate's top-left-origin convention, and
/// the first vertex points straight DOWN, so the star is inverted: two
/// points up, one point down. That orientation is the product's chosen mark
/// (it is what makes the glyph unmistakable next to a menu bar full of
/// waveforms), so it is asserted by a test rather than left to the sign of a
/// sine, which is exactly the kind of thing a later refactor flips by
/// accident.
pub fn unit_path() -> Vec<Point> {
    let mut out = Vec::with_capacity(POINTS + 1);
    for i in 0..=POINTS {
        // Start at +90 degrees and walk two vertices at a time. In this
        // crate's y-down space that puts the first vertex at the BOTTOM.
        let vertex = (i * STEP) % POINTS;
        let angle =
            std::f64::consts::FRAC_PI_2 + (vertex as f64) * std::f64::consts::TAU / POINTS as f64;
        out.push(Point {
            x: angle.cos(),
            // NOT negated: this crate's y grows downward, so a positive
            // sin() is lower on screen. Negating here silently flips the
            // star back the other way up.
            y: angle.sin(),
        });
    }
    out
}

/// The pentagram scaled to fit a square of `size` points, inset by
/// `line_width` so the stroke never clips at the edges.
pub fn path_in(size: f64, line_width: f64) -> Vec<Point> {
    let radius = (size - line_width) / 2.0;
    let centre = size / 2.0;
    unit_path()
        .into_iter()
        .map(|p| Point {
            x: centre + p.x * radius,
            y: centre + p.y * radius,
        })
        .collect()
}

/// Glyph box size in points. Matches the SF Symbol point size the status
/// item used before, so the mark sits at the same visual weight as the
/// system icons beside it.
pub const GLYPH_SIZE: f64 = 15.0;

/// Stroke width. Thin enough that the interior pentagon stays open at
/// menu-bar size, heavy enough to survive a non-Retina display.
pub const GLYPH_LINE_WIDTH: f64 = 1.3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_closes_and_visits_every_point_once() {
        let path = unit_path();
        // Six entries: five vertices plus the return to the start.
        assert_eq!(path.len(), POINTS + 1);
        let first = path[0];
        let last = path[POINTS];
        assert!((first.x - last.x).abs() < 1e-9, "path does not close");
        assert!((first.y - last.y).abs() < 1e-9, "path does not close");

        // Step-two ordering must be a single cycle through all five points,
        // not two shorter loops. If it ever visited a vertex twice the shape
        // would silently become a pentagon or a bowtie.
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..POINTS {
            assert!(seen.insert((i * STEP) % POINTS), "vertex visited twice");
        }
        assert_eq!(seen.len(), POINTS);
    }

    #[test]
    fn it_is_inverted_point_down() {
        // The mark is deliberately point-DOWN: one point at the bottom, two
        // at the top. Orientation is a product decision, and it hangs on the
        // sign of a sine, so it is pinned here rather than trusted.
        let path = unit_path();
        let lowest = path[..POINTS]
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, |acc, p| acc.max(p.y));
        assert!(
            (path[0].y - lowest).abs() < 1e-9,
            "the star is not point-down: {path:?}"
        );
        assert!(path[0].x.abs() < 1e-9, "the apex is not centred");

        // Two vertices above the centre line, confirming the "two points up"
        // silhouette rather than merely a low first vertex.
        let above = path[..POINTS].iter().filter(|p| p.y < -1e-9).count();
        assert_eq!(above, 2, "expected two points up: {path:?}");
    }

    #[test]
    fn scaling_stays_inside_the_glyph_box() {
        // A stroke that clips at the edge reads as a broken icon, and the
        // menu bar gives no room to fix it visually.
        let size = GLYPH_SIZE;
        let lw = GLYPH_LINE_WIDTH;
        for p in path_in(size, lw) {
            assert!(
                p.x >= lw / 2.0 - 1e-9 && p.x <= size - lw / 2.0 + 1e-9,
                "{p:?}"
            );
            assert!(
                p.y >= lw / 2.0 - 1e-9 && p.y <= size - lw / 2.0 + 1e-9,
                "{p:?}"
            );
        }
    }

    #[test]
    fn the_five_vertices_are_evenly_spaced_on_the_circle() {
        // All five on the unit circle, no duplicates: this is what keeps the
        // star regular rather than lopsided.
        let path = unit_path();
        for p in &path[..POINTS] {
            let r = (p.x * p.x + p.y * p.y).sqrt();
            assert!((r - 1.0).abs() < 1e-9, "vertex off the circle: {p:?}");
        }
    }
}
