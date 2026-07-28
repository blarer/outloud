//! The skull: the overlay's animated mascot, as pure geometry and a pure
//! animator. No AppKit anywhere in this module, for the same reason
//! [`crate::mark`] is pure: the headless build must compile it, CI must be
//! able to assert its properties without a display, and a future Windows
//! backend must render the *same* skull rather than a hand-ported cousin.
//!
//! # Shape
//!
//! A front-facing cartoon skull in a unit box (`0.0..=1.0`, y down, same
//! convention as [`crate::layout`]): cranium + maxilla as one filled
//! polygon, a separate mandible polygon so the jaw can articulate, elliptical
//! eye sockets, a nasal aperture, and teeth as small rectangles that part
//! when the jaw drops. Curves are pre-sampled into polylines exactly like
//! `mark.rs` does, so any backend that can fill a polygon renders it.
//!
//! # Motion
//!
//! [`SkullAnimator`] turns `(state, mic level, time)` into a [`SkullPose`]
//! per frame. The design constraints, from the product requirements:
//!
//! * **The jaw is driven by the real level, not a loop.** An asymmetric
//!   envelope (fast attack, slow release) follows the shaped mic level, so
//!   the mouth snaps open on a syllable and eases shut in the gaps instead
//!   of jittering at the VAD frame rate.
//! * **Never frozen while visible.** A slow breathing scale and a subtle
//!   sway keep the skull alive between words, and a deterministic blink
//!   schedule keeps the eyes from reading as painted-on.
//! * **A settle on commit.** When the utterance finalizes the jaw closes
//!   and the whole skull does one small damped-spring pop, which is the
//!   "got it" gesture without a toast (UX principle 1: success is silent).
//! * **Reduce Motion is a design, not an off switch.** Under
//!   `accessibilityDisplayShouldReduceMotion` every oscillation (breathe,
//!   sway, blink, shimmer, spring) is removed; the jaw still tracks the
//!   level directly because it is *communication* (is the mic hearing me?),
//!   the same rationale as the reduced-motion level ring it replaces.

use crate::layout::Point;
use crate::state::OverlayState;

// ---------------------------------------------------------------------------
// Geometry: the skull at rest, in the unit box, y down.
// ---------------------------------------------------------------------------

/// Cranium dome: centre and radii of the sampled ellipse arc.
const DOME_CX: f64 = 0.5;
const DOME_CY: f64 = 0.42;
const DOME_RX: f64 = 0.335;
const DOME_RY: f64 = 0.34;
/// Segments for the dome arc. 24 is indistinguishable from a true ellipse
/// at overlay size and keeps the polygon cheap for non-Bézier backends.
const DOME_SEGMENTS: usize = 24;

/// Vertical drop of the mandible at `jaw_open == 1.0`, in unit space.
/// ~10% of the head: cartoonishly readable without dislocating.
const JAW_DROP: f64 = 0.10;

/// Eye sockets: centres and radii.
const EYE_Y: f64 = 0.52;
const EYE_DX: f64 = 0.125;
const EYE_RX: f64 = 0.085;
const EYE_RY: f64 = 0.075;
const EYE_SEGMENTS: usize = 16;

/// Top edge of the mandible when closed. The maxilla bottom sits at 0.80;
/// the 0.015 gap is the closed mouth's seam.
const JAW_TOP: f64 = 0.815;

/// The pivot everything scales and rotates about: between the eyes, so
/// breathing reads as the head swelling, not sliding.
const PIVOT: Point = Point { x: 0.5, y: 0.55 };

/// Everything a backend needs to draw one posed skull frame. All polygons
/// are closed (first point NOT repeated; backends close the path), in the
/// unit box, already transformed by the pose.
#[derive(Debug, Clone, PartialEq)]
pub struct SkullGeometry {
    /// Cranium + maxilla, one filled polygon.
    pub cranium: Vec<Point>,
    /// The mandible, filled separately so it can drop.
    pub jaw: Vec<Point>,
    /// Eye sockets, left then right, filled dark. Height already scaled by
    /// the blink openness.
    pub sockets: [Vec<Point>; 2],
    /// Eye glows, concentric with the sockets, filled with the state accent
    /// at the pose's `eye_glow` alpha.
    pub eyes: [Vec<Point>; 2],
    /// Nasal aperture, filled dark.
    pub nose: Vec<Point>,
    /// The mouth cavity: fills the gap the dropping jaw opens, so the
    /// desktop never shows through the skull's mouth (the panel behind is
    /// fully transparent).
    pub mouth: Vec<Point>,
    /// Teeth rectangles: uppers hang from the maxilla, lowers ride the jaw.
    pub teeth: Vec<Vec<Point>>,
}

fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64, segments: usize) -> Vec<Point> {
    (0..segments)
        .map(|i| {
            let a = i as f64 / segments as f64 * std::f64::consts::TAU;
            Point {
                x: cx + rx * a.cos(),
                y: cy + ry * a.sin(),
            }
        })
        .collect()
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<Point> {
    vec![
        Point { x, y },
        Point { x: x + w, y },
        Point { x: x + w, y: y + h },
        Point { x, y: y + h },
    ]
}

/// The cranium+maxilla outline at rest. Left side listed bottom-up, dome
/// arc over the top, right side mirrored down, then the maxilla's bottom
/// edge closes it.
fn cranium_outline() -> Vec<Point> {
    let mut pts = vec![
        Point { x: 0.34, y: 0.80 }, // maxilla bottom-left
        Point { x: 0.32, y: 0.68 }, // maxilla left flare
        Point { x: 0.30, y: 0.66 }, // cheek notch
        Point { x: 0.20, y: 0.60 }, // left cheekbone
    ];
    // Dome: θ sweeps π..0 so the arc runs left temple → crown → right
    // temple (y-down: sin θ > 0 is above the ellipse centre).
    for i in 0..=DOME_SEGMENTS {
        let theta = std::f64::consts::PI * (1.0 - i as f64 / DOME_SEGMENTS as f64);
        pts.push(Point {
            x: DOME_CX + DOME_RX * theta.cos(),
            y: DOME_CY - DOME_RY * theta.sin(),
        });
    }
    pts.extend([
        Point { x: 0.80, y: 0.60 }, // right cheekbone
        Point { x: 0.70, y: 0.66 },
        Point { x: 0.68, y: 0.68 },
        Point { x: 0.66, y: 0.80 }, // maxilla bottom-right
    ]);
    pts
}

/// The mandible at rest (closed). Translated down by the pose's jaw drop.
fn jaw_outline() -> Vec<Point> {
    vec![
        Point {
            x: 0.36,
            y: JAW_TOP,
        },
        Point {
            x: 0.64,
            y: JAW_TOP,
        },
        Point { x: 0.66, y: 0.86 },
        Point { x: 0.60, y: 0.925 },
        Point { x: 0.40, y: 0.925 },
        Point { x: 0.34, y: 0.86 },
    ]
}

fn nose_outline() -> Vec<Point> {
    vec![
        Point { x: 0.50, y: 0.595 },
        Point { x: 0.535, y: 0.70 },
        Point { x: 0.465, y: 0.70 },
    ]
}

// ---------------------------------------------------------------------------
// Pose: the animator's per-frame output.
// ---------------------------------------------------------------------------

/// One frame of skull motion, all values already smoothed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkullPose {
    /// Mandible openness, `0.0..=1.0`.
    pub jaw_open: f64,
    /// Eyelid openness, `0.0` (mid-blink) to `1.0`.
    pub eye_open: f64,
    /// Eye glow strength, `0.0..=1.0`; the backend maps it to accent alpha.
    pub eye_glow: f64,
    /// Uniform scale about [`PIVOT`]: breathing plus the settle pop.
    pub scale: f64,
    /// Rotation about [`PIVOT`] in radians: the idle sway.
    pub tilt: f64,
}

impl SkullPose {
    /// The dead-still pose reduced-motion error states render.
    pub fn at_rest() -> Self {
        SkullPose {
            jaw_open: 0.0,
            eye_open: 1.0,
            eye_glow: 0.8,
            scale: 1.0,
            tilt: 0.0,
        }
    }

    /// Whether two consecutive poses differ enough that a repaint is worth
    /// scheduling. Thresholds are below one device pixel of effect at
    /// overlay size, so skipping equal-within-epsilon frames is invisible.
    pub fn visibly_differs(&self, other: &SkullPose) -> bool {
        (self.jaw_open - other.jaw_open).abs() > 0.002
            || (self.eye_open - other.eye_open).abs() > 0.01
            || (self.eye_glow - other.eye_glow).abs() > 0.005
            || (self.scale - other.scale).abs() > 0.0005
            || (self.tilt - other.tilt).abs() > 0.0005
    }
}

/// Build the posed geometry for one frame: articulate the jaw and blink,
/// then apply the pose's whole-head scale and tilt about [`PIVOT`].
pub fn posed_geometry(pose: &SkullPose) -> SkullGeometry {
    let drop = pose.jaw_open.clamp(0.0, 1.0) * JAW_DROP;
    let translate = |pts: Vec<Point>, dy: f64| -> Vec<Point> {
        pts.into_iter()
            .map(|p| Point {
                x: p.x,
                y: p.y + dy,
            })
            .collect()
    };

    let cranium = cranium_outline();
    let jaw = translate(jaw_outline(), drop);

    let eye_ry = EYE_RY * (0.15 + 0.85 * pose.eye_open.clamp(0.0, 1.0));
    let socket = |dx: f64| ellipse(0.5 + dx, EYE_Y, EYE_RX, eye_ry, EYE_SEGMENTS);
    let glow = |dx: f64| ellipse(0.5 + dx, EYE_Y, EYE_RX * 0.55, eye_ry * 0.55, EYE_SEGMENTS);
    let sockets = [socket(-EYE_DX), socket(EYE_DX)];
    let eyes = [glow(-EYE_DX), glow(EYE_DX)];

    // Teeth: three wide uppers fixed to the maxilla, three lowers riding
    // the jaw. Three, not a dental chart: at the orb-sized 42pt box a
    // tooth narrower than ~4pt collapses into noise, and the visible gap
    // between the two rows IS the open mouth, which is the part that must
    // read. Legible-at-size beats detailed-and-muddy.
    let mut teeth = Vec::with_capacity(6);
    let centers = [0.41, 0.50, 0.59];
    for &cx in &centers {
        teeth.push(rect(cx - 0.038, 0.80, 0.076, 0.05));
        teeth.push(rect(cx - 0.038, JAW_TOP + drop - 0.05, 0.076, 0.05));
    }

    let mut geo = SkullGeometry {
        cranium,
        jaw,
        sockets,
        eyes,
        nose: nose_outline(),
        mouth: rect(0.355, 0.79, 0.29, JAW_TOP + drop - 0.775),
        teeth,
    };

    // Whole-head transform: scale then rotate about the pivot, baked into
    // the points so backends stay transform-free.
    let (s, c) = (pose.tilt.sin(), pose.tilt.cos());
    let xform = |p: &mut Point| {
        let dx = (p.x - PIVOT.x) * pose.scale;
        let dy = (p.y - PIVOT.y) * pose.scale;
        p.x = PIVOT.x + dx * c - dy * s;
        p.y = PIVOT.y + dx * s + dy * c;
    };
    let apply = |poly: &mut Vec<Point>| poly.iter_mut().for_each(xform);
    apply(&mut geo.cranium);
    apply(&mut geo.jaw);
    geo.sockets.iter_mut().for_each(&apply);
    geo.eyes.iter_mut().for_each(&apply);
    apply(&mut geo.nose);
    apply(&mut geo.mouth);
    geo.teeth.iter_mut().for_each(apply);
    geo
}

// ---------------------------------------------------------------------------
// Animator.
// ---------------------------------------------------------------------------

/// Jaw attack time constant, seconds. Fast: a syllable's onset must read
/// within two 60 Hz frames or the mouth looks dubbed.
const JAW_ATTACK_TAU: f64 = 0.045;
/// Jaw release time constant. Slow relative to attack so inter-word gaps
/// ease shut instead of flapping at the 30 ms VAD frame rate.
const JAW_RELEASE_TAU: f64 = 0.16;

/// Breathing: rate and depth. ~0.22 Hz is resting human breath; ±1.2%
/// scale is visible in the periphery without being distracting.
const BREATHE_HZ: f64 = 0.22;
const BREATHE_DEPTH: f64 = 0.012;
/// Idle sway: slower than breath, ±1.4 degrees.
const SWAY_HZ: f64 = 0.11;
const SWAY_RAD: f64 = 0.025;

/// Blink schedule: one blink per slot, slot length in seconds, blink
/// duration in seconds. Deterministic (hashed slot index jitters the phase)
/// so tests can pin it and replays are reproducible.
const BLINK_SLOT: f64 = 4.0;
const BLINK_SECS: f64 = 0.14;

/// Settle pop: initial scale overshoot and the damped-spring constants.
const POP_SCALE: f64 = 0.045;
const POP_DECAY_TAU: f64 = 0.14;
const POP_HZ: f64 = 3.3;
/// How long after the trigger the pop is over (several decay constants).
const POP_TOTAL_SECS: f64 = 0.8;

/// Turns `(state, level, time)` into smooth [`SkullPose`]s. One instance
/// lives in the render model; `step` is called once per animation frame.
#[derive(Debug)]
pub struct SkullAnimator {
    jaw: f64,
    /// When the finalize settle was triggered, in the caller's clock.
    settle_at: Option<f64>,
}

impl SkullAnimator {
    pub fn new() -> Self {
        SkullAnimator {
            jaw: 0.0,
            settle_at: None,
        }
    }

    /// The commit gesture: called once when the utterance finalizes
    /// (Listening → Transcribing). The jaw eases shut through the normal
    /// release envelope; this adds the one-shot spring pop.
    pub fn trigger_settle(&mut self, now: f64) {
        self.settle_at = Some(now);
    }

    /// Advance one frame. `level` is the *shaped* mic level (0..=1); `now`
    /// and `dt` are seconds on the caller's monotonic clock.
    pub fn step(
        &mut self,
        now: f64,
        dt: f64,
        state: OverlayState,
        level: f64,
        reduce_motion: bool,
    ) -> SkullPose {
        // Jaw: follows the level only while the mic is hot; every other
        // state closes the mouth. Asymmetric exponential envelope.
        let target = if state == OverlayState::Listening {
            level.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if reduce_motion {
            // Direct tracking: the jaw is level *communication*, so it
            // stays, but with no envelope dynamics layered on top.
            self.jaw = target;
        } else {
            let tau = if target > self.jaw {
                JAW_ATTACK_TAU
            } else {
                JAW_RELEASE_TAU
            };
            let ease = 1.0 - (-dt.max(0.0) / tau).exp();
            self.jaw += (target - self.jaw) * ease;
        }

        let eye_glow = eye_glow(state, level, now, reduce_motion);

        if reduce_motion {
            return SkullPose {
                jaw_open: self.jaw,
                eye_open: 1.0,
                eye_glow,
                scale: 1.0,
                tilt: 0.0,
            };
        }

        // Settle pop: a damped cosine spring, one-shot.
        let mut scale = 1.0 + BREATHE_DEPTH * (std::f64::consts::TAU * BREATHE_HZ * now).sin();
        if let Some(t0) = self.settle_at {
            let t = now - t0;
            if t >= POP_TOTAL_SECS {
                self.settle_at = None;
            } else if t >= 0.0 {
                scale += POP_SCALE
                    * (-t / POP_DECAY_TAU).exp()
                    * (std::f64::consts::TAU * POP_HZ * t).cos();
            }
        }

        SkullPose {
            jaw_open: self.jaw,
            eye_open: blink_openness(now),
            eye_glow,
            scale,
            tilt: SWAY_RAD * (std::f64::consts::TAU * SWAY_HZ * now + 1.0).sin(),
        }
    }
}

impl Default for SkullAnimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-state eye behaviour, the skull's version of the state table's glyph
/// column: listening brightens with the voice, transcribing shimmers
/// ("machine working, not hung"), loading pulses, failure states hold a
/// steady stare (motion soothes, and an error should not).
fn eye_glow(state: OverlayState, level: f64, now: f64, reduce_motion: bool) -> f64 {
    use OverlayState::*;
    if reduce_motion {
        return match state {
            Listening => 0.5 + 0.5 * level.clamp(0.0, 1.0),
            Transcribing => 1.0,
            ModelLoading => 0.5,
            _ => 0.85,
        };
    }
    match state {
        Listening => 0.55 + 0.45 * level.clamp(0.0, 1.0),
        Transcribing => {
            let s = (now * std::f64::consts::TAU / 1.2).sin() * 0.5 + 0.5;
            0.5 + 0.5 * s
        }
        ModelLoading => {
            let s = (now * std::f64::consts::TAU * crate::theme::PULSE_HZ).sin() * 0.5 + 0.5;
            0.3 + 0.7 * s
        }
        _ => 0.9,
    }
}

/// Eyelid openness at `now`. Each [`BLINK_SLOT`]-second slot contains one
/// blink whose phase inside the slot is jittered by a hash of the slot
/// index, so blinks look irregular while staying fully deterministic.
fn blink_openness(now: f64) -> f64 {
    let slot = (now / BLINK_SLOT).floor();
    // Cheap deterministic hash to 0..1, the classic sin-fract construction.
    let jitter = ((slot + 1.0) * 127.1).sin().abs().fract();
    let blink_at = slot * BLINK_SLOT + 0.4 + jitter * (BLINK_SLOT - BLINK_SECS - 0.8);
    let t = (now - blink_at) / BLINK_SECS;
    if !(0.0..=1.0).contains(&t) {
        return 1.0;
    }
    // Triangle close-then-open, smoothed: 1 → 0 at mid-blink → 1.
    let tri = 1.0 - (1.0 - (2.0 * t - 1.0).abs());
    tri * tri * (3.0 - 2.0 * tri)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(poly: &[Point]) -> (f64, f64, f64, f64) {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in poly {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        (x0, y0, x1, y1)
    }

    #[test]
    fn resting_geometry_stays_inside_the_unit_box() {
        let geo = posed_geometry(&SkullPose::at_rest());
        let mut all: Vec<&Point> = geo.cranium.iter().collect();
        all.extend(geo.jaw.iter());
        all.extend(geo.nose.iter());
        for poly in geo.sockets.iter().chain(geo.eyes.iter()) {
            all.extend(poly.iter());
        }
        for t in &geo.teeth {
            all.extend(t.iter());
        }
        for p in all {
            assert!(
                (-0.01..=1.01).contains(&p.x) && (-0.01..=1.01).contains(&p.y),
                "point escaped the unit box: {p:?}"
            );
        }
    }

    #[test]
    fn jaw_open_moves_the_mandible_down_not_the_cranium() {
        let closed = posed_geometry(&SkullPose::at_rest());
        let open = posed_geometry(&SkullPose {
            jaw_open: 1.0,
            ..SkullPose::at_rest()
        });
        let (_, _, _, closed_jaw_bottom) = bounds(&closed.jaw);
        let (_, _, _, open_jaw_bottom) = bounds(&open.jaw);
        assert!(
            open_jaw_bottom - closed_jaw_bottom > JAW_DROP * 0.9,
            "jaw must drop by ~JAW_DROP"
        );
        assert_eq!(closed.cranium, open.cranium, "cranium must not move");
    }

    #[test]
    fn blink_scales_socket_height() {
        let open = posed_geometry(&SkullPose::at_rest());
        let shut = posed_geometry(&SkullPose {
            eye_open: 0.0,
            ..SkullPose::at_rest()
        });
        let h = |poly: &[Point]| {
            let (_, y0, _, y1) = bounds(poly);
            y1 - y0
        };
        assert!(h(&shut.sockets[0]) < h(&open.sockets[0]) * 0.3);
    }

    #[test]
    fn jaw_attack_is_faster_than_release() {
        let mut a = SkullAnimator::new();
        let dt = 1.0 / 60.0;
        // One frame of full level from rest.
        let up = a
            .step(0.0, dt, OverlayState::Listening, 1.0, false)
            .jaw_open;
        // Now drop the level to zero for one frame from wherever we are.
        let before = up;
        let down = a.step(dt, dt, OverlayState::Listening, 0.0, false).jaw_open;
        let rise = up - 0.0;
        let fall = before - down;
        assert!(
            rise > fall,
            "attack must move farther per frame than release (rise {rise:.3} vs fall {fall:.3})"
        );
    }

    #[test]
    fn non_listening_states_close_the_jaw() {
        let mut a = SkullAnimator::new();
        // Open it.
        for i in 0..30 {
            a.step(
                i as f64 / 60.0,
                1.0 / 60.0,
                OverlayState::Listening,
                1.0,
                false,
            );
        }
        // Then transcribe for a second: jaw must ease shut.
        let mut pose = SkullPose::at_rest();
        for i in 30..90 {
            pose = a.step(
                i as f64 / 60.0,
                1.0 / 60.0,
                OverlayState::Transcribing,
                0.0,
                false,
            );
        }
        assert!(pose.jaw_open < 0.05, "jaw still open: {}", pose.jaw_open);
    }

    #[test]
    fn reduce_motion_removes_every_oscillation() {
        let mut a = SkullAnimator::new();
        a.trigger_settle(0.0); // even a pending settle must not move it
        let p1 = a.step(1.0, 1.0 / 60.0, OverlayState::Listening, 0.5, true);
        let p2 = a.step(2.5, 1.5, OverlayState::Listening, 0.5, true);
        assert_eq!(p1, p2, "constant input must produce a constant pose");
        assert_eq!(p1.scale, 1.0);
        assert_eq!(p1.tilt, 0.0);
        assert_eq!(p1.eye_open, 1.0);
        assert_eq!(p1.jaw_open, 0.5, "level tracking stays: it is information");
    }

    #[test]
    fn settle_pop_fires_then_decays_away() {
        let mut a = SkullAnimator::new();
        a.trigger_settle(1.0);
        let early = a.step(1.02, 0.02, OverlayState::Transcribing, 0.0, false);
        assert!(
            (early.scale - 1.0).abs() > 0.01,
            "pop must be visible just after trigger (scale {})",
            early.scale
        );
        let late = a.step(3.0, 0.5, OverlayState::Transcribing, 0.0, false);
        // After the pop only the breathe remains, bounded by its depth.
        assert!((late.scale - 1.0).abs() <= BREATHE_DEPTH + 1e-9);
    }

    #[test]
    fn blink_is_deterministic_bounded_and_mostly_open() {
        let mut open_frames = 0;
        let mut total = 0;
        let mut t = 0.0;
        while t < 60.0 {
            let o = blink_openness(t);
            assert!(
                (0.0..=1.0).contains(&o),
                "openness out of range at {t}: {o}"
            );
            assert_eq!(o, blink_openness(t), "must be deterministic");
            if o > 0.99 {
                open_frames += 1;
            }
            total += 1;
            t += 1.0 / 60.0;
        }
        let frac = open_frames as f64 / total as f64;
        assert!(
            frac > 0.9,
            "eyes should be open ~96% of the time, got {frac}"
        );
        assert!(frac < 1.0, "some blinks must actually happen");
    }

    #[test]
    fn poses_within_epsilon_do_not_demand_a_repaint() {
        let p = SkullPose::at_rest();
        assert!(!p.visibly_differs(&p));
        let q = SkullPose {
            jaw_open: p.jaw_open + 0.5,
            ..p
        };
        assert!(p.visibly_differs(&q));
    }
}
