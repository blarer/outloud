//! The cat: the overlay's animated mascot, as pure geometry and a pure
//! animator. No AppKit anywhere in this module, for the same reason
//! [`crate::mark`] is pure: the headless build must compile it, CI must be
//! able to assert its properties without a display, and a future Windows
//! backend must render the *same* cat rather than a hand-ported cousin.
//!
//! # Shape
//!
//! A front-facing cartoon cat head in a unit box (`0.0..=1.0`, y down, same
//! convention as [`crate::layout`]), modelled on one specific cat: a dilute
//! calico domestic longhair. The features that make her *her*, and which
//! this geometry therefore keeps, are:
//!
//! * the big white chest ruff under the chin (the silhouette is a pear,
//!   not a circle),
//! * the asymmetric face: a cream patch above the left eye, a grey patch
//!   sweeping over the right temple, and a small grey smudge beside the
//!   nose — at 42pt these patches ARE her identity, so they survive while
//!   fur texture, the harness, and toe detail do not,
//! * moss-green eyes with round dark pupils, and a dusty-pink nose.
//!
//! Everything is pre-sampled polylines/polygons exactly like `mark.rs`, so
//! any backend that can fill a polygon renders it.
//!
//! # Motion
//!
//! [`CatAnimator`] turns `(state, mic level, time)` into a [`CatPose`] per
//! frame. The skull's proven dynamics are kept where they encode product
//! requirements (mouth follows the real level with an asymmetric envelope;
//! never frozen while visible; a settle on commit; entry grows in), and the
//! cat adds the vocabulary a cat actually has:
//!
//! * **Ears carry the state.** Perked while listening, drowsy-lowered while
//!   the model loads, flat "airplane ears" on error — the one cat posture
//!   every human reads correctly at a glance.
//! * **Pupils dilate with the voice.** A cat's pupils blow wide when it is
//!   locked onto something; mapping dilation to the mic level makes
//!   "the mic hears me" visible even when the mouth is between words.
//! * **The commit gesture is a slow blink.** Cats slow-blink to say
//!   "acknowledged, all is well", which is precisely the semantics of the
//!   finalize settle — so it plays alongside the skull's damped-spring pop
//!   rather than replacing it.
//! * **Reduce Motion is a design, not an off switch.** Oscillations
//!   (breathe, sway, blink, shimmer, spring, entry) are removed; the mouth
//!   and pupils still track the level directly because they are
//!   *communication* (is the mic hearing me?), and the ears still take
//!   their per-state posture because that is state signalling, not motion.

use crate::layout::Point;
use crate::state::OverlayState;

// ---------------------------------------------------------------------------
// Geometry: the cat at rest, in the unit box, y down.
// ---------------------------------------------------------------------------

/// Head ellipse: centre and radii. The head sits high so the ruff has room
/// below the chin, matching the reference's head-over-fluff silhouette.
const HEAD_CX: f64 = 0.5;
const HEAD_CY: f64 = 0.58;
const HEAD_RX: f64 = 0.30;
const HEAD_RY: f64 = 0.27;
/// Segments for the head outline. 32 keeps the cheek fur tufts (alternate
/// samples pushed out) readable while staying cheap for polygon backends.
const HEAD_SEGMENTS: usize = 32;

/// Vertical reach of the mouth cavity at `mouth_open == 1.0`, in unit
/// space. Same order as the skull's jaw drop: cartoonishly readable at
/// 42pt without unhinging the face.
const MOUTH_DROP: f64 = 0.13;

/// Eyes: centres and radii. Larger relative to the head than a real cat's,
/// because at 42pt the eyes are the only place state colour can live.
const EYE_Y: f64 = 0.53;
const EYE_DX: f64 = 0.115;
const EYE_RX: f64 = 0.075;
const EYE_RY: f64 = 0.065;
const EYE_SEGMENTS: usize = 16;

/// Pupil: a tall ellipse whose width breathes with dilation. The height is
/// most of the iris so the pupil reads as a cat's, not a dot.
const PUPIL_RY: f64 = 0.052;
const PUPIL_RX_MIN: f64 = 0.020;
const PUPIL_RX_RANGE: f64 = 0.030;

/// The pivot everything scales and rotates about: between the eyes, so
/// breathing reads as the head swelling, not sliding. Same point as the
/// skull's so the two mascots share motion character.
const PIVOT: Point = Point { x: 0.5, y: 0.55 };

/// Ear anchors. The base chord sits just inside the head outline so the
/// ear and head always overlap; only the tip moves. Lerping the tip
/// between an explicit perked position and an explicit flat position —
/// rather than rotating the whole triangle — keeps both extremes inside
/// the unit box by construction instead of by trigonometry.
const EAR_L_BASE_OUTER: Point = Point { x: 0.26, y: 0.43 };
const EAR_L_BASE_INNER: Point = Point { x: 0.39, y: 0.33 };
const EAR_L_TIP_PERKED: Point = Point { x: 0.19, y: 0.085 };
const EAR_L_TIP_FLAT: Point = Point { x: 0.06, y: 0.33 };

/// Everything a backend needs to draw one posed cat frame. All polygons
/// are closed (first point NOT repeated; backends close the path), in the
/// unit box, already transformed by the pose. Fields are ordered roughly
/// back-to-front as a backend should draw them.
#[derive(Debug, Clone, PartialEq)]
pub struct CatGeometry {
    /// The white chest ruff: a jagged-bottomed fan behind the chin. Drawn
    /// first so the head overlaps it.
    pub ruff: Vec<Point>,
    /// Ear outer triangles, left then right, filled grey (the reference's
    /// ears are grey against the white face).
    pub ears: [Vec<Point>; 2],
    /// Inner-ear triangles, filled pink.
    pub ear_inners: [Vec<Point>; 2],
    /// The head, one filled polygon with fur-tuft jags on the cheeks.
    pub head: Vec<Point>,
    /// Cream patch above the left eye — half of the dilute-calico mask.
    pub patch_cream: Vec<Point>,
    /// Grey patch over the right temple — the other half.
    pub patch_grey: Vec<Point>,
    /// The grey smudge beside the nose, the reference's beauty mark.
    pub smudge: Vec<Point>,
    /// The mouth cavity: grows downward as the mouth opens, filled dark so
    /// the desktop never shows through (the panel behind is transparent).
    pub mouth: Vec<Point>,
    /// Two small fangs hanging from the cavity's top edge; their length
    /// scales with the opening so a closed mouth hides them. The reference
    /// mid-meow shows exactly this.
    pub fangs: Vec<Vec<Point>>,
    /// Nose triangle, filled pink.
    pub nose: Vec<Point>,
    /// Whiskers as thin quads, three per side. Quads, not lines, so the
    /// polygon-only backend contract holds.
    pub whiskers: Vec<Vec<Point>>,
    /// Eyes (iris), left then right, filled moss green. Height already
    /// scaled by the blink openness.
    pub eyes: [Vec<Point>; 2],
    /// Pupils, concentric with the eyes, filled dark. Width already scaled
    /// by the pose's dilation.
    pub pupils: [Vec<Point>; 2],
    /// Pupil glints, concentric with the pupils, filled with the state
    /// accent at the pose's `eye_glow` alpha — a cat's eyes catching light.
    pub glints: [Vec<Point>; 2],
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

/// The head outline: a sampled ellipse with alternate samples on the lower
/// cheeks pushed outward, so the silhouette carries fur without any
/// texture pass. The jag depth (~2pt at overlay size) is the smallest that
/// still reads as fluff rather than aliasing.
fn head_outline() -> Vec<Point> {
    (0..HEAD_SEGMENTS)
        .map(|i| {
            let a = i as f64 / HEAD_SEGMENTS as f64 * std::f64::consts::TAU;
            let deg = a.to_degrees();
            // y-down: 20°..70° is the right jowl, 110°..160° the left.
            let cheek = (20.0..=70.0).contains(&deg) || (110.0..=160.0).contains(&deg);
            let f = if cheek && i % 2 == 0 { 1.08 } else { 1.0 };
            Point {
                x: HEAD_CX + HEAD_RX * f * a.cos(),
                y: HEAD_CY + HEAD_RY * f * a.sin(),
            }
        })
        .collect()
}

/// The chest ruff: top chord tucked behind the head, bottom edge zigzagged
/// into tufts. The tufts are what turn "circle on a stalk" into the
/// reference's pear-shaped fluff silhouette.
fn ruff_outline() -> Vec<Point> {
    vec![
        Point { x: 0.22, y: 0.70 },
        Point { x: 0.155, y: 0.79 },
        Point { x: 0.255, y: 0.775 },
        Point { x: 0.235, y: 0.895 },
        Point { x: 0.345, y: 0.845 },
        Point { x: 0.385, y: 0.955 },
        Point { x: 0.475, y: 0.865 },
        Point { x: 0.555, y: 0.95 },
        Point { x: 0.635, y: 0.85 },
        Point { x: 0.705, y: 0.915 },
        Point { x: 0.72, y: 0.795 },
        Point { x: 0.825, y: 0.80 },
        Point { x: 0.78, y: 0.70 },
        Point { x: 0.5, y: 0.635 },
    ]
}

/// Mirror a point across the head's vertical centreline.
fn mirror(p: Point) -> Point {
    Point { x: 1.0 - p.x, y: p.y }
}

fn lerp(a: Point, b: Point, t: f64) -> Point {
    Point {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
    }
}

/// One posed ear: base fixed on the head, tip lerped between flat and
/// perked. `side` is -1 for left, +1 for right.
fn ear(perk: f64, side: f64) -> Vec<Point> {
    let (outer, inner, tip_perked, tip_flat) = if side < 0.0 {
        (
            EAR_L_BASE_OUTER,
            EAR_L_BASE_INNER,
            EAR_L_TIP_PERKED,
            EAR_L_TIP_FLAT,
        )
    } else {
        (
            mirror(EAR_L_BASE_OUTER),
            mirror(EAR_L_BASE_INNER),
            mirror(EAR_L_TIP_PERKED),
            mirror(EAR_L_TIP_FLAT),
        )
    };
    let tip = lerp(tip_flat, tip_perked, perk.clamp(0.0, 1.0));
    vec![outer, tip, inner]
}

/// The pink inner ear: the outer triangle scaled toward its centroid.
fn ear_inner(outer: &[Point]) -> Vec<Point> {
    let n = outer.len() as f64;
    let cx = outer.iter().map(|p| p.x).sum::<f64>() / n;
    let cy = outer.iter().map(|p| p.y).sum::<f64>() / n;
    outer
        .iter()
        .map(|p| Point {
            x: cx + (p.x - cx) * 0.55,
            y: cy + (p.y - cy) * 0.55,
        })
        .collect()
}

fn nose_outline() -> Vec<Point> {
    vec![
        Point { x: 0.465, y: 0.615 },
        Point { x: 0.535, y: 0.615 },
        Point { x: 0.5, y: 0.665 },
    ]
}

/// A whisker as a thin quad from `a` (muzzle) to `b` (tip).
fn whisker(a: Point, b: Point) -> Vec<Point> {
    const HALF_W: f64 = 0.008;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-9);
    let (nx, ny) = (-dy / len * HALF_W, dx / len * HALF_W);
    vec![
        Point { x: a.x + nx, y: a.y + ny },
        Point { x: b.x + nx, y: b.y + ny },
        Point { x: b.x - nx, y: b.y - ny },
        Point { x: a.x - nx, y: a.y - ny },
    ]
}

fn whiskers() -> Vec<Vec<Point>> {
    let left = [
        (Point { x: 0.415, y: 0.635 }, Point { x: 0.055, y: 0.555 }),
        (Point { x: 0.42, y: 0.665 }, Point { x: 0.045, y: 0.645 }),
        (Point { x: 0.415, y: 0.695 }, Point { x: 0.075, y: 0.745 }),
    ];
    let mut out = Vec::with_capacity(6);
    for (a, b) in left {
        out.push(whisker(a, b));
        out.push(whisker(mirror(a), mirror(b)));
    }
    out
}

/// The cream patch above the left eye. Placed to touch the eye's top edge,
/// as in the reference, and shaped to stay inside the head outline so no
/// clipping is needed anywhere in the pipeline.
fn patch_cream_outline() -> Vec<Point> {
    vec![
        Point { x: 0.30, y: 0.46 },
        Point { x: 0.32, y: 0.40 },
        Point { x: 0.40, y: 0.37 },
        Point { x: 0.48, y: 0.38 },
        Point { x: 0.50, y: 0.44 },
        Point { x: 0.46, y: 0.475 },
        Point { x: 0.36, y: 0.48 },
    ]
}

/// The grey patch over the right temple, sweeping down around the right
/// eye. The eye is drawn after it, so the patch reads as fur behind the
/// eye rather than over it.
fn patch_grey_outline() -> Vec<Point> {
    vec![
        Point { x: 0.53, y: 0.40 },
        Point { x: 0.62, y: 0.36 },
        Point { x: 0.70, y: 0.40 },
        Point { x: 0.74, y: 0.47 },
        Point { x: 0.71, y: 0.53 },
        Point { x: 0.63, y: 0.55 },
        Point { x: 0.56, y: 0.50 },
    ]
}

// ---------------------------------------------------------------------------
// Pose: the animator's per-frame output.
// ---------------------------------------------------------------------------

/// One frame of cat motion, all values already smoothed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CatPose {
    /// Mouth openness, `0.0..=1.0`.
    pub mouth_open: f64,
    /// Eyelid openness, `0.0` (mid-blink) to `1.0`.
    pub eye_open: f64,
    /// Glint strength, `0.0..=1.0`; the backend maps it to accent alpha.
    pub eye_glow: f64,
    /// Pupil dilation, `0.0` (slit) to `1.0` (blown wide).
    pub pupil: f64,
    /// Ear posture, `0.0` (flat back) to `1.0` (fully perked).
    pub ear_perk: f64,
    /// Uniform scale about [`PIVOT`]: breathing plus the settle pop.
    pub scale: f64,
    /// Rotation about [`PIVOT`] in radians: the idle sway.
    pub tilt: f64,
    /// Overall opacity, `0.0..=1.0`. Only the entry animation drives this
    /// below 1.0; a hard cut into view was the one moment of the
    /// interaction that read as abrupt, and it is also the most-seen frame
    /// in the product because it happens on every keypress.
    pub opacity: f64,
}

impl CatPose {
    /// The dead-still pose reduced-motion error states render.
    pub fn at_rest() -> Self {
        CatPose {
            mouth_open: 0.0,
            eye_open: 1.0,
            eye_glow: 0.8,
            pupil: 0.5,
            ear_perk: 0.85,
            scale: 1.0,
            tilt: 0.0,
            opacity: 1.0,
        }
    }

    /// Whether two consecutive poses differ enough that a repaint is worth
    /// scheduling. Thresholds are below one device pixel of effect at
    /// overlay size, so skipping equal-within-epsilon frames is invisible.
    pub fn visibly_differs(&self, other: &CatPose) -> bool {
        (self.mouth_open - other.mouth_open).abs() > 0.002
            || (self.eye_open - other.eye_open).abs() > 0.01
            || (self.eye_glow - other.eye_glow).abs() > 0.005
            || (self.pupil - other.pupil).abs() > 0.01
            || (self.ear_perk - other.ear_perk).abs() > 0.005
            || (self.scale - other.scale).abs() > 0.0005
            || (self.tilt - other.tilt).abs() > 0.0005
            || (self.opacity - other.opacity).abs() > 0.004
    }
}

/// Build the posed geometry for one frame: articulate the mouth, ears,
/// pupils and blink, then apply the pose's whole-head scale and tilt about
/// [`PIVOT`].
pub fn posed_geometry(pose: &CatPose) -> CatGeometry {
    let open = pose.mouth_open.clamp(0.0, 1.0);

    // Mouth cavity: widens slightly as it drops, like a real meow. The
    // closed cavity is a 0.005-tall seam — sub-pixel at overlay size, so a
    // closed mouth simply reads as no mouth, which is the resting face.
    let half_w = 0.055 + 0.03 * open;
    let top = 0.685;
    let bottom = top + 0.005 + MOUTH_DROP * open;
    let mouth = vec![
        Point { x: 0.5 - half_w, y: top },
        Point { x: 0.5 + half_w, y: top },
        Point {
            x: 0.5 + half_w * 0.8,
            y: bottom,
        },
        Point {
            x: 0.5 - half_w * 0.8,
            y: bottom,
        },
    ];
    // Fangs hang from the cavity's top corners; length scales with the
    // opening so they only appear mid-meow, as in the reference photo.
    let fang = |cx: f64| {
        vec![
            Point { x: cx - 0.012, y: top },
            Point { x: cx + 0.012, y: top },
            Point {
                x: cx,
                y: top + 0.045 * open,
            },
        ]
    };
    let fangs = vec![fang(0.5 - half_w * 0.55), fang(0.5 + half_w * 0.55)];

    let blink = 0.15 + 0.85 * pose.eye_open.clamp(0.0, 1.0);
    let eye_ry = EYE_RY * blink;
    let pupil_rx = PUPIL_RX_MIN + PUPIL_RX_RANGE * pose.pupil.clamp(0.0, 1.0);
    let pupil_ry = PUPIL_RY * blink;
    let eye = |dx: f64| ellipse(0.5 + dx, EYE_Y, EYE_RX, eye_ry, EYE_SEGMENTS);
    let pupil = |dx: f64| ellipse(0.5 + dx, EYE_Y, pupil_rx, pupil_ry, EYE_SEGMENTS);
    let glint = |dx: f64| ellipse(0.5 + dx, EYE_Y, pupil_rx * 0.6, pupil_ry * 0.6, EYE_SEGMENTS);

    let perk = pose.ear_perk;
    let ears = [ear(perk, -1.0), ear(perk, 1.0)];
    let ear_inners = [ear_inner(&ears[0]), ear_inner(&ears[1])];

    let mut geo = CatGeometry {
        ruff: ruff_outline(),
        ears,
        ear_inners,
        head: head_outline(),
        patch_cream: patch_cream_outline(),
        patch_grey: patch_grey_outline(),
        smudge: ellipse(0.415, 0.655, 0.030, 0.021, 12),
        mouth,
        fangs,
        nose: nose_outline(),
        whiskers: whiskers(),
        eyes: [eye(-EYE_DX), eye(EYE_DX)],
        pupils: [pupil(-EYE_DX), pupil(EYE_DX)],
        glints: [glint(-EYE_DX), glint(EYE_DX)],
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
    apply(&mut geo.ruff);
    geo.ears.iter_mut().for_each(&apply);
    geo.ear_inners.iter_mut().for_each(&apply);
    apply(&mut geo.head);
    apply(&mut geo.patch_cream);
    apply(&mut geo.patch_grey);
    apply(&mut geo.smudge);
    apply(&mut geo.mouth);
    geo.fangs.iter_mut().for_each(&apply);
    apply(&mut geo.nose);
    geo.whiskers.iter_mut().for_each(&apply);
    geo.eyes.iter_mut().for_each(&apply);
    geo.pupils.iter_mut().for_each(&apply);
    geo.glints.iter_mut().for_each(apply);
    geo
}

// ---------------------------------------------------------------------------
// Animator.
// ---------------------------------------------------------------------------

/// Mouth attack time constant, seconds. Fast: a syllable's onset must read
/// within two 60 Hz frames or the mouth looks dubbed.
const MOUTH_ATTACK_TAU: f64 = 0.045;
/// Mouth release time constant. Slow relative to attack so inter-word gaps
/// ease shut instead of flapping at the 30 ms VAD frame rate.
const MOUTH_RELEASE_TAU: f64 = 0.16;

/// Ear time constant. Faster than the mouth's release: ears are a cat's
/// most reactive feature, and a slow ear transition reads as confusion
/// rather than a state change.
const EAR_TAU: f64 = 0.10;
/// Pupil time constant. Slower than the ears: real pupils take a beat.
const PUPIL_TAU: f64 = 0.18;

/// Breathing: rate and depth. ~0.22 Hz is resting breath; ±1.2% scale is
/// visible in the periphery without being distracting.
const BREATHE_HZ: f64 = 0.22;
const BREATHE_DEPTH: f64 = 0.012;
/// Idle sway: slower than breath, ±1.4 degrees.
const SWAY_HZ: f64 = 0.11;
const SWAY_RAD: f64 = 0.025;

/// Blink schedule: one blink per slot, slot length in seconds, blink
/// duration in seconds. Deterministic (hashed slot index jitters the
/// phase) so tests can pin it and replays are reproducible.
const BLINK_SLOT: f64 = 4.0;
const BLINK_SECS: f64 = 0.14;

/// Settle pop: initial scale overshoot and the damped-spring constants.
const POP_SCALE: f64 = 0.045;
const POP_DECAY_TAU: f64 = 0.14;
const POP_HZ: f64 = 3.3;
/// How long after the trigger the pop is over (several decay constants).
const POP_TOTAL_SECS: f64 = 0.8;

/// The slow blink on commit: duration and how far the lids close. 0.55 s
/// and most-of-the-way shut is the tempo of a real cat's contentment
/// blink; a full close would read as the ordinary blink it must not be
/// confused with.
const SLOW_BLINK_SECS: f64 = 0.55;
const SLOW_BLINK_DEPTH: f64 = 0.85;

/// Entry: the cat scaling and fading in when the hotkey goes down.
///
/// 150ms because this sits between the user pressing a key and being ready
/// to speak. Slower reads as the UI making them wait, which is worse than
/// the hard cut it replaces; much faster is indistinguishable from no
/// animation at all.
const ENTRY_SECS: f64 = 0.15;
/// Starting scale. Small enough to read as growing, large enough that the
/// first frame is recognisably a cat rather than a dot.
const ENTRY_FROM_SCALE: f64 = 0.72;
/// Overshoot past rest before settling, matching the commit pop's idiom so
/// entry and finalize feel like the same object moving.
const ENTRY_OVERSHOOT: f64 = 0.04;

/// Turns `(state, level, time)` into smooth [`CatPose`]s. One instance
/// lives in the render model; `step` is called once per animation frame.
#[derive(Debug)]
pub struct CatAnimator {
    mouth: f64,
    ear: f64,
    pupil: f64,
    /// When the finalize settle was triggered, in the caller's clock.
    settle_at: Option<f64>,
    /// When the panel last became visible, in the caller's clock.
    entry_at: Option<f64>,
}

impl CatAnimator {
    pub fn new() -> Self {
        CatAnimator {
            mouth: 0.0,
            ear: CatPose::at_rest().ear_perk,
            pupil: CatPose::at_rest().pupil,
            settle_at: None,
            entry_at: None,
        }
    }

    /// The commit gesture: called once when the utterance finalizes
    /// (Listening → Transcribing). The mouth eases shut through the normal
    /// release envelope; this adds the one-shot spring pop and the slow
    /// blink.
    pub fn trigger_settle(&mut self, now: f64) {
        self.settle_at = Some(now);
    }

    /// The entry gesture: called once when the panel becomes visible, i.e.
    /// when the user presses the hotkey. Scales and fades the cat in
    /// instead of cutting to it.
    pub fn trigger_entry(&mut self, now: f64) {
        self.entry_at = Some(now);
    }

    /// Entry progress at `now`: `None` once the animation is over, so the
    /// steady state costs nothing.
    fn entry_phase(&self, now: f64) -> Option<f64> {
        let started = self.entry_at?;
        let t = (now - started) / ENTRY_SECS;
        (0.0..1.0).contains(&t).then_some(t)
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
    ) -> CatPose {
        let level = level.clamp(0.0, 1.0);

        // Mouth: follows the level only while the mic is hot; every other
        // state closes it. Asymmetric exponential envelope.
        let mouth_target = if state == OverlayState::Listening {
            level
        } else {
            0.0
        };
        // Ears and pupils: per-state postures (see the module doc for the
        // mapping's rationale).
        let ear_target = ear_perk(state);
        let pupil_target = pupil_dilation(state, level);

        if reduce_motion {
            // Direct tracking: mouth and pupils are level *communication*
            // and the ears are state signalling, so all three stay — but
            // with no envelope dynamics layered on top.
            self.mouth = mouth_target;
            self.ear = ear_target;
            self.pupil = pupil_target;
        } else {
            let ease = |tau: f64| 1.0 - (-dt.max(0.0) / tau).exp();
            let mouth_tau = if mouth_target > self.mouth {
                MOUTH_ATTACK_TAU
            } else {
                MOUTH_RELEASE_TAU
            };
            self.mouth += (mouth_target - self.mouth) * ease(mouth_tau);
            self.ear += (ear_target - self.ear) * ease(EAR_TAU);
            self.pupil += (pupil_target - self.pupil) * ease(PUPIL_TAU);
        }

        let eye_glow = eye_glow(state, level, now, reduce_motion);

        if reduce_motion {
            return CatPose {
                mouth_open: self.mouth,
                eye_open: 1.0,
                eye_glow,
                pupil: self.pupil,
                ear_perk: self.ear,
                scale: 1.0,
                tilt: 0.0,
                // No fade: the entry is decoration, so reduce-motion gets
                // the cat immediately rather than a slower nothing.
                opacity: 1.0,
            };
        }

        // Settle pop: a damped cosine spring, one-shot. The slow blink
        // rides the same trigger.
        let mut scale = 1.0 + BREATHE_DEPTH * (std::f64::consts::TAU * BREATHE_HZ * now).sin();
        let mut eye_open = blink_openness(now);
        if let Some(t0) = self.settle_at {
            let t = now - t0;
            if t >= POP_TOTAL_SECS {
                self.settle_at = None;
            } else if t >= 0.0 {
                scale += POP_SCALE
                    * (-t / POP_DECAY_TAU).exp()
                    * (std::f64::consts::TAU * POP_HZ * t).cos();
                if t < SLOW_BLINK_SECS {
                    // Half-sine dip: lids ease most of the way shut and
                    // back — the cat's "got it" alongside the pop.
                    let dip = (std::f64::consts::PI * t / SLOW_BLINK_SECS).sin();
                    eye_open *= 1.0 - SLOW_BLINK_DEPTH * dip;
                }
            }
        }

        // Entry: scale and fade in, overshooting slightly before settling,
        // so the panel grows into place rather than cutting to it. Applied
        // last and multiplicatively, so it composes with breathing and the
        // settle pop instead of fighting them.
        let mut opacity = 1.0;
        if let Some(t) = self.entry_phase(now) {
            // Cubic ease-out: most of the motion is in the first third,
            // which is what makes a short animation still read as smooth.
            let eased = 1.0 - (1.0 - t).powi(3);
            let overshoot = ENTRY_OVERSHOOT * (std::f64::consts::PI * t).sin();
            scale *= ENTRY_FROM_SCALE + (1.0 - ENTRY_FROM_SCALE) * eased + overshoot;
            // Opacity leads the scale slightly: fully opaque before the
            // shape stops moving, so the settle is visible rather than
            // happening behind a fade.
            opacity = (t / 0.7).min(1.0);
        } else if self.entry_at.is_some_and(|t0| now >= t0 + ENTRY_SECS) {
            // One-shot: stop paying for the branch once it is over.
            self.entry_at = None;
        }

        CatPose {
            mouth_open: self.mouth,
            eye_open,
            eye_glow,
            pupil: self.pupil,
            ear_perk: self.ear,
            scale,
            tilt: SWAY_RAD * (std::f64::consts::TAU * SWAY_HZ * now + 1.0).sin(),
            opacity,
        }
    }
}

impl Default for CatAnimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-state ear posture. The mapping leans on postures humans already
/// read: perked = attending, lowered = drowsy, flat "airplane ears" =
/// something is wrong. Failure states get the flattest ears because they
/// are the states that want a human, and a flat-eared cat is impossible to
/// mistake for a content one.
fn ear_perk(state: OverlayState) -> f64 {
    use OverlayState::*;
    match state {
        Listening => 1.0,
        Transcribing => 0.85,
        ModelLoading => 0.55,
        NoPermission => 0.22,
        Error => 0.0,
        _ => 0.85,
    }
}

/// Per-state pupil dilation. Listening dilates with the voice (a locked-on
/// cat's pupils blow wide); errors hold fully dilated — the alarmed stare;
/// transcribing narrows slightly, the concentrating squint.
fn pupil_dilation(state: OverlayState, level: f64) -> f64 {
    use OverlayState::*;
    match state {
        Listening => 0.45 + 0.55 * level,
        Transcribing => 0.35,
        Error => 1.0,
        NoPermission => 0.8,
        _ => 0.5,
    }
}

/// Per-state glint behaviour, unchanged in semantics from the skull's eye
/// glow: listening brightens with the voice, transcribing shimmers
/// ("machine working, not hung"), loading pulses, failure states hold a
/// steady stare (motion soothes, and an error should not).
fn eye_glow(state: OverlayState, level: f64, now: f64, reduce_motion: bool) -> f64 {
    use OverlayState::*;
    if reduce_motion {
        return match state {
            Listening => 0.5 + 0.5 * level,
            Transcribing => 1.0,
            ModelLoading => 0.5,
            _ => 0.85,
        };
    }
    match state {
        Listening => 0.55 + 0.45 * level,
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

    fn all_polys(geo: &CatGeometry) -> Vec<&Vec<Point>> {
        let mut v: Vec<&Vec<Point>> = vec![
            &geo.ruff,
            &geo.head,
            &geo.patch_cream,
            &geo.patch_grey,
            &geo.smudge,
            &geo.mouth,
            &geo.nose,
        ];
        v.extend(geo.ears.iter());
        v.extend(geo.ear_inners.iter());
        v.extend(geo.fangs.iter());
        v.extend(geo.whiskers.iter());
        v.extend(geo.eyes.iter());
        v.extend(geo.pupils.iter());
        v.extend(geo.glints.iter());
        v
    }

    #[test]
    fn resting_geometry_stays_inside_the_unit_box() {
        // Both ear extremes, because the flat posture swings the tips
        // furthest toward the box edge.
        for perk in [0.0, 1.0] {
            let geo = posed_geometry(&CatPose {
                ear_perk: perk,
                ..CatPose::at_rest()
            });
            for poly in all_polys(&geo) {
                for p in poly {
                    assert!(
                        (-0.01..=1.01).contains(&p.x) && (-0.01..=1.01).contains(&p.y),
                        "point escaped the unit box at perk {perk}: {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn mouth_open_grows_the_cavity_not_the_head() {
        let closed = posed_geometry(&CatPose::at_rest());
        let open = posed_geometry(&CatPose {
            mouth_open: 1.0,
            ..CatPose::at_rest()
        });
        let (_, _, _, closed_bottom) = bounds(&closed.mouth);
        let (_, _, _, open_bottom) = bounds(&open.mouth);
        assert!(
            open_bottom - closed_bottom > MOUTH_DROP * 0.9,
            "cavity must grow by ~MOUTH_DROP"
        );
        assert_eq!(closed.head, open.head, "head must not move");
    }

    #[test]
    fn fangs_are_hidden_when_the_mouth_is_closed() {
        let closed = posed_geometry(&CatPose::at_rest());
        let open = posed_geometry(&CatPose {
            mouth_open: 1.0,
            ..CatPose::at_rest()
        });
        let h = |poly: &[Point]| {
            let (_, y0, _, y1) = bounds(poly);
            y1 - y0
        };
        assert!(h(&closed.fangs[0]) < 0.002, "closed mouth must hide fangs");
        assert!(h(&open.fangs[0]) > 0.03, "open mouth must show fangs");
    }

    #[test]
    fn blink_scales_eye_height() {
        let open = posed_geometry(&CatPose::at_rest());
        let shut = posed_geometry(&CatPose {
            eye_open: 0.0,
            ..CatPose::at_rest()
        });
        let h = |poly: &[Point]| {
            let (_, y0, _, y1) = bounds(poly);
            y1 - y0
        };
        assert!(h(&shut.eyes[0]) < h(&open.eyes[0]) * 0.3);
    }

    #[test]
    fn pupils_widen_with_dilation_but_stay_inside_the_iris() {
        let slit = posed_geometry(&CatPose {
            pupil: 0.0,
            ..CatPose::at_rest()
        });
        let wide = posed_geometry(&CatPose {
            pupil: 1.0,
            ..CatPose::at_rest()
        });
        let w = |poly: &[Point]| {
            let (x0, _, x1, _) = bounds(poly);
            x1 - x0
        };
        assert!(w(&wide.pupils[0]) > w(&slit.pupils[0]) * 2.0);
        assert!(
            w(&wide.pupils[0]) < w(&wide.eyes[0]),
            "a pupil wider than its iris is an escaped pupil"
        );
    }

    #[test]
    fn flat_ears_drop_the_tips_and_swing_them_outward() {
        let perked = posed_geometry(&CatPose {
            ear_perk: 1.0,
            ..CatPose::at_rest()
        });
        let flat = posed_geometry(&CatPose {
            ear_perk: 0.0,
            ..CatPose::at_rest()
        });
        // The tip is the polygon's second vertex (base-outer, tip,
        // base-inner; see `ear`).
        let (p, f) = (perked.ears[0][1], flat.ears[0][1]);
        assert!(f.y > p.y + 0.1, "flat ear tip must drop visibly");
        assert!(f.x < p.x, "left flat ear tip must swing outward (left)");
        let (p, f) = (perked.ears[1][1], flat.ears[1][1]);
        assert!(f.x > p.x, "right flat ear tip must swing outward (right)");
    }

    #[test]
    fn face_patches_sit_inside_the_head() {
        // The patches are drawn without clipping, so escaping the head
        // outline would paint fur onto the desktop.
        let geo = posed_geometry(&CatPose::at_rest());
        for poly in [&geo.patch_cream, &geo.patch_grey, &geo.smudge] {
            for p in poly {
                let dx = (p.x - HEAD_CX) / HEAD_RX;
                let dy = (p.y - HEAD_CY) / HEAD_RY;
                assert!(
                    dx * dx + dy * dy <= 1.02,
                    "patch point outside the head ellipse: {p:?}"
                );
            }
        }
    }

    #[test]
    fn mouth_attack_is_faster_than_release() {
        let mut a = CatAnimator::new();
        let dt = 1.0 / 60.0;
        let up = a
            .step(0.0, dt, OverlayState::Listening, 1.0, false)
            .mouth_open;
        let before = up;
        let down = a
            .step(dt, dt, OverlayState::Listening, 0.0, false)
            .mouth_open;
        let rise = up - 0.0;
        let fall = before - down;
        assert!(
            rise > fall,
            "attack must move farther per frame than release (rise {rise:.3} vs fall {fall:.3})"
        );
    }

    #[test]
    fn non_listening_states_close_the_mouth() {
        let mut a = CatAnimator::new();
        for i in 0..30 {
            a.step(
                i as f64 / 60.0,
                1.0 / 60.0,
                OverlayState::Listening,
                1.0,
                false,
            );
        }
        let mut pose = CatPose::at_rest();
        for i in 30..90 {
            pose = a.step(
                i as f64 / 60.0,
                1.0 / 60.0,
                OverlayState::Transcribing,
                0.0,
                false,
            );
        }
        assert!(
            pose.mouth_open < 0.05,
            "mouth still open: {}",
            pose.mouth_open
        );
    }

    #[test]
    fn error_flattens_the_ears() {
        let mut a = CatAnimator::new();
        let mut pose = CatPose::at_rest();
        for i in 0..120 {
            pose = a.step(i as f64 / 60.0, 1.0 / 60.0, OverlayState::Error, 0.0, false);
        }
        assert!(pose.ear_perk < 0.05, "ears still up: {}", pose.ear_perk);
        // And back: recovery must not stick, or every error leaves a
        // permanently miserable cat.
        for i in 120..300 {
            pose = a.step(
                i as f64 / 60.0,
                1.0 / 60.0,
                OverlayState::Listening,
                0.0,
                false,
            );
        }
        assert!(pose.ear_perk > 0.9, "ears must recover: {}", pose.ear_perk);
    }

    #[test]
    fn listening_dilates_pupils_with_the_voice() {
        let mut quiet = CatAnimator::new();
        let mut loud = CatAnimator::new();
        let mut pq = CatPose::at_rest();
        let mut pl = CatPose::at_rest();
        for i in 0..120 {
            let t = i as f64 / 60.0;
            pq = quiet.step(t, 1.0 / 60.0, OverlayState::Listening, 0.0, false);
            pl = loud.step(t, 1.0 / 60.0, OverlayState::Listening, 1.0, false);
        }
        assert!(
            pl.pupil > pq.pupil + 0.3,
            "voice must dilate pupils (quiet {} vs loud {})",
            pq.pupil,
            pl.pupil
        );
    }

    #[test]
    fn reduce_motion_removes_every_oscillation() {
        let mut a = CatAnimator::new();
        a.trigger_settle(0.0); // even a pending settle must not move it
        let p1 = a.step(1.0, 1.0 / 60.0, OverlayState::Listening, 0.5, true);
        let p2 = a.step(2.5, 1.5, OverlayState::Listening, 0.5, true);
        assert_eq!(p1, p2, "constant input must produce a constant pose");
        assert_eq!(p1.scale, 1.0);
        assert_eq!(p1.tilt, 0.0);
        assert_eq!(p1.eye_open, 1.0);
        assert_eq!(p1.mouth_open, 0.5, "level tracking stays: it is information");
        assert_eq!(p1.ear_perk, 1.0, "ear posture stays: it is state signalling");
    }

    #[test]
    fn settle_pop_fires_then_decays_away() {
        let mut a = CatAnimator::new();
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
    fn settle_plays_a_slow_blink_that_ends_open() {
        let mut a = CatAnimator::new();
        a.trigger_settle(10.0);
        // Mid slow-blink: lids visibly lowered.
        let mid = a.step(
            10.0 + SLOW_BLINK_SECS / 2.0,
            0.016,
            OverlayState::Transcribing,
            0.0,
            false,
        );
        assert!(
            mid.eye_open < 0.4,
            "slow blink must visibly lower the lids, got {}",
            mid.eye_open
        );
        // Well after: back to the ordinary blink schedule (open at t=12,
        // which sits between scheduled blinks).
        let after = a.step(12.0, 0.016, OverlayState::Transcribing, 0.0, false);
        assert!(
            after.eye_open > 0.9,
            "eyes must reopen after the slow blink, got {}",
            after.eye_open
        );
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
        let p = CatPose::at_rest();
        assert!(!p.visibly_differs(&p));
        let q = CatPose {
            mouth_open: p.mouth_open + 0.5,
            ..p
        };
        assert!(p.visibly_differs(&q));
        let r = CatPose {
            ear_perk: p.ear_perk - 0.5,
            ..p
        };
        assert!(p.visibly_differs(&r), "ear posture changes must repaint");
    }

    /// The entry starts small and transparent, and ends at rest.
    #[test]
    fn entry_grows_from_small_and_transparent_to_rest() {
        let mut a = CatAnimator::new();
        a.trigger_entry(0.0);

        let first = a.step(0.0, 0.016, OverlayState::Listening, 0.0, false);
        assert!(
            first.scale < 0.8,
            "entry must start visibly smaller than rest, got {}",
            first.scale
        );
        assert!(
            first.opacity < 0.1,
            "entry must start near-transparent, got {}",
            first.opacity
        );

        let settled = a.step(1.0, 0.016, OverlayState::Listening, 0.0, false);
        assert!(
            (settled.opacity - 1.0).abs() < 1e-9,
            "entry must finish fully opaque, got {}",
            settled.opacity
        );
        assert!(
            (settled.scale - 1.0).abs() < 0.05,
            "entry must settle to roughly rest scale, got {}",
            settled.scale
        );
    }

    /// Opacity leads scale, so the settle is watched rather than hidden.
    #[test]
    fn entry_reaches_full_opacity_before_it_stops_moving() {
        let mut a = CatAnimator::new();
        a.trigger_entry(0.0);
        let late = a.step(
            ENTRY_SECS * 0.75,
            0.016,
            OverlayState::Listening,
            0.0,
            false,
        );
        assert!(
            (late.opacity - 1.0).abs() < 1e-9,
            "opacity should lead the scale, got {} at 75% through",
            late.opacity
        );
    }

    /// Reduced motion appears immediately: no grow, no fade.
    #[test]
    fn reduced_motion_skips_the_entry_entirely() {
        let mut a = CatAnimator::new();
        a.trigger_entry(0.0);
        let pose = a.step(0.0, 0.016, OverlayState::Listening, 0.0, true);
        assert_eq!(pose.opacity, 1.0);
        assert_eq!(pose.scale, 1.0);
    }
}
