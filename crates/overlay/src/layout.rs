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
///
/// 10, up from 8: at 8 the card visually collided with the descenders of
/// the line being dictated into on Retina displays, which read as the
/// overlay covering the text even though it did not.
const ANCHOR_GAP: f64 = 10.0;
/// Minimum distance from any screen edge. Keeps the overlay clear of rounded
/// display corners, the Dock, and the notch shadow.
///
/// 16, up from 12: matched to the theme's 18pt card radius. A rounded card
/// inset by less than its own radius reads as jammed against the edge, and
/// on displays with rounded corners the card's corner disappeared into the
/// screen's.
const SCREEN_MARGIN: f64 = 16.0;

/// The overlay card's size in points, shared by every backend.
///
/// Lives here rather than in a backend so macOS and Windows cannot drift,
/// and so the placement tests exercise the size the product actually ships.
///
/// 380x64, from 340x72. Wider and shorter is the single change that most
/// moves our silhouette toward Aqua's "floating bar": theirs is a wide, low
/// bar, while a squarer card reads as a dialog. The extra 40pt of width also
/// buys roughly ten more characters of partial tail, which is the content
/// the card exists to show.
pub const OVERLAY_SIZE: Size = Size {
    width: 380.0,
    height: 64.0,
};

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

/// Bottom-center placement: the overlay horizontally centered on `screen`
/// and inset from its bottom edge by the standard margin.
///
/// This is what Aqua Voice does unconditionally — their FAQ calls it "the
/// black floating bar at the bottom" — and we offer it as an explicit
/// preference for people migrating from it. It is deliberately *not* an
/// [`Anchor`] variant: an anchor describes something the host discovered
/// about the user's attention (a caret, a pointer), while this is a fixed
/// user preference that ignores all of that. Keeping it a separate function
/// also means adding it did not change the exhaustive `match` in every
/// platform backend.
///
/// We do not make it the default. Bottom-center puts the feedback far from
/// the caret, outside the user's locus of attention
/// (`docs/ux/02-core-interaction.md`).
pub fn place_bottom_center(overlay: Size, screen: Rect) -> Rect {
    let x = screen.origin.x + (screen.size.width - overlay.width) / 2.0;
    let y = screen.max_y() - SCREEN_MARGIN - overlay.height;
    // Clamp for the pathological case of an overlay wider than the display,
    // matching `place`'s guarantee that the result is always on screen.
    let x = x.max(screen.origin.x + SCREEN_MARGIN);
    let y = y.max(screen.origin.y + SCREEN_MARGIN);
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

// ---------------------------------------------------------------------------
// The rolling window (docs/overlay-redesign.md §1–§2), as pure data.
//
// This is the model the redesigned macOS overlay renders: a bounded lane of
// word slots where *position* decides whether a word must fade, *age*
// decides stale decay during pauses, and the commit horizon decides
// *styling*. It lives here rather than in the backend for the same reason
// `place` does: it must compile and be unit-tested on headless CI, and the
// Windows backend must not be able to drift from macOS when it adopts the
// redesign.
// ---------------------------------------------------------------------------

/// Width budget of the text lane, in points — never characters. Words are
/// measured by the backend in points, so a CJK lane simply holds more,
/// narrower units and the same overflow rule applies (this replaces the old
/// `TAIL_CHARS = 44` character-count anti-pattern).
pub const LANE_WIDTH: f64 = 440.0;
/// Distance past the lane's left edge over which a word ramps 1 → 0.
pub const FADE_RAMP: f64 = 48.0;
/// Gap between adjacent words, in points.
pub const WORD_GAP: f64 = 6.5;
/// Opacity easing time constant, seconds. Exponential approach: frame-rate
/// independent, no start/stop discontinuity, and it bends smoothly when a
/// target changes mid-fade. This is the "flowy vs jerky" knob.
pub const EASE_TAU: f64 = 0.22;

/// Delay added per word when several arrive in one hypothesis.
///
/// Small on purpose. At 55ms a four-word burst finishes staggering in
/// 165ms, comfortably faster than the ~1.3s between hypotheses, so the
/// cascade always completes before the next batch lands and never becomes
/// a backlog. Large enough to be legible as motion; small enough that
/// nobody waits for it.
pub const BIRTH_STAGGER: f64 = 0.055;
/// Committed words start decaying this long after commitment: the text
/// already lives in the target field, so the overlay repeating it forever
/// is noise. In-flight words never stale-decay — text that might still
/// change must stay visible until the horizon resolves it.
pub const STALE_AFTER: f64 = 4.0;
/// Duration of the stale decay ramp, seconds.
pub const STALE_FADE: f64 = 2.0;
/// How many consecutive *distinct* hypotheses must agree on a word before
/// it renders as committed. Mirrors `stream::HorizonConfig::stability`'s
/// default (3): the overlay applies the same LocalAgreement policy to the
/// hypothesis stream the pipeline already publishes, so the white/tinted
/// boundary tracks the same stability signal the injection horizon uses.
pub const STABILITY: usize = 3;
/// Trailing words held back from commit styling even when stable, mirroring
/// `stream::HorizonConfig::lookback_words`: recognizers revise most near
/// the audio frontier even when hypotheses happen to agree.
pub const LOOKBACK_WORDS: usize = 1;
/// Displayed opacity below which a word is dead for rendering purposes.
pub const DEAD_OPACITY: f64 = 0.02;

/// One word in the rolling window.
#[derive(Debug, Clone)]
pub struct WordSlot {
    pub text: String,
    /// Measured width in points, cached when the text is (re)set: measuring
    /// needs platform text APIs, so the backend supplies it via a callback
    /// and this model never re-measures per frame.
    pub width: f64,
    /// Whether the display-side horizon has settled this word. Committed
    /// and in-flight words are styled differently on purpose: the boundary
    /// between them IS the commit horizon, made visible.
    pub committed: bool,
    /// When the word committed, for the stale decay clock.
    committed_at: Option<f64>,
    /// How many consecutive distinct hypotheses have agreed on this word.
    stable_updates: usize,
    /// Displayed opacity, eased toward the per-frame target.
    pub opacity: f64,
    /// Seconds still to wait before this word begins to bloom in.
    ///
    /// Exists because the recognizer does not deliver words one at a time.
    /// Apple's `SpeechTranscriber` emits a whole revised hypothesis every
    /// ~1.3s, so several new words arrive in the same frame and, sharing one
    /// easing constant, used to bloom in perfect unison. That reads as a
    /// block of text being stamped down, which is exactly the jolt this
    /// staggering removes: the words fade up left to right instead, so an
    /// arrival looks like speech landing rather than a paste.
    ///
    /// It buys nothing in latency and is not meant to. The first word is
    /// never delayed; only its followers are, and only by a few frames.
    birth_delay: f64,
}

/// The rolling window of transcribed words.
///
/// Feed it the whole current hypothesis with [`RollingWindow::ingest`]
/// (idempotent per distinct hypothesis, so polling at frame rate is free),
/// then advance the animation with [`RollingWindow::step`] once per frame.
#[derive(Debug, Default)]
pub struct RollingWindow {
    words: Vec<WordSlot>,
    /// Committed words removed from the front after fading out. Needed to
    /// realign the hypothesis' unit list with the surviving slots: the
    /// hypothesis still contains those words, the display no longer does.
    dropped: usize,
    /// The last hypothesis ingested. Stability counts *distinct* updates,
    /// not frames, so re-ingesting an unchanged hypothesis is a no-op.
    last_hypothesis: String,
}

/// Split a hypothesis into displayable units.
///
/// Whitespace separates words; CJK codepoints (Han, kana, Hangul) each form
/// their own unit, approximating UAX #29's per-syllable segmentation
/// without pulling `unicode-segmentation` into this crate. The lane budget
/// is points, so the only job of segmentation is fade granularity: one
/// glyph per unit is exactly what UAX #29 yields for these scripts too.
pub fn split_units(text: &str) -> Vec<String> {
    let mut units = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                units.push(std::mem::take(&mut current));
            }
        } else if is_cjk(ch) {
            if !current.is_empty() {
                units.push(std::mem::take(&mut current));
            }
            units.push(ch.to_string());
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

/// Codepoints that segment one-per-unit: CJK ideographs, kana, Hangul.
fn is_cjk(ch: char) -> bool {
    matches!(u32::from(ch),
        0x3400..=0x4DBF   // CJK ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xF900..=0xFAFF // CJK compat
        | 0x3040..=0x309F // hiragana
        | 0x30A0..=0x30FF // katakana
        | 0xAC00..=0xD7AF // hangul syllables
        | 0x20000..=0x2FA1F // CJK ext B..F + compat supplement
    )
}

impl RollingWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current slots, oldest first.
    pub fn words(&self) -> &[WordSlot] {
        &self.words
    }

    /// Forget everything. Called at the start of a new utterance and when
    /// the overlay hides.
    pub fn reset(&mut self) {
        self.words.clear();
        self.dropped = 0;
        self.last_hypothesis.clear();
    }

    /// Absorb the current whole hypothesis. `measure` maps a unit to its
    /// rendered width in points; it is only called for new or rewritten
    /// text, never for stable words.
    ///
    /// Semantics per the redesign: committed words are append-only and
    /// never touched; the in-flight tail is diffed unit-wise and rewritten
    /// in place, so revision only ever churns the tinted zone.
    pub fn ingest(&mut self, hypothesis: &str, now: f64, measure: &mut dyn FnMut(&str) -> f64) {
        if hypothesis == self.last_hypothesis {
            return; // frame-rate polling of an unchanged hypothesis
        }
        let units = split_units(hypothesis);
        // Counts words created by THIS call, so the stagger resets per
        // hypothesis rather than growing unbounded across an utterance.
        let mut born_this_ingest = 0usize;
        // Units the display already retired (committed, faded, removed).
        // The horizon's never-retract property means the hypothesis still
        // starts with them; if a recognizer rewrites that deeply anyway we
        // simply render the tail from where our record ends.
        let fresh = units.into_iter().skip(self.dropped);
        let mut j = 0usize;
        for unit in fresh {
            match self.words.get_mut(j) {
                Some(w) if w.committed => {
                    // Committed text is visually guaranteed stable: even a
                    // divergent hypothesis does not rewrite the white zone
                    // (matches stream::CommitHorizon's monotonic commits).
                }
                Some(w) => {
                    if w.text == unit {
                        w.stable_updates += 1;
                    } else {
                        // Rewritten in place. Opacity is kept: a revision
                        // replaces the glyphs, it does not re-bloom, which
                        // would read as new speech.
                        w.width = measure(&unit);
                        w.text = unit;
                        w.stable_updates = 0;
                    }
                }
                None => {
                    let width = measure(&unit);
                    self.words.push(WordSlot {
                        text: unit,
                        width,
                        committed: false,
                        committed_at: None,
                        stable_updates: 0,
                        opacity: 0.0, // words are born transparent and bloom in
                        // Stagger only within THIS hypothesis. The first new
                        // word never waits, so nothing about perceived
                        // latency changes; each later one starts a beat
                        // after, which is what turns a simultaneous batch
                        // into a left-to-right cascade.
                        birth_delay: born_this_ingest as f64 * BIRTH_STAGGER,
                    });
                    born_this_ingest += 1;
                }
            }
            j += 1;
        }
        // Hypothesis shrank: drop the in-flight slots past its end.
        // (Committed slots are never past the in-flight zone, but guard
        // anyway so a pathological feed cannot erase white text.)
        while self.words.len() > j && !self.words.last().is_none_or(|w| w.committed) {
            self.words.pop();
        }
        // Prefix-wise commit pass, mirroring the LocalAgreement policy: a
        // word renders committed once every word before it is committed,
        // it has survived `STABILITY` consecutive distinct hypotheses
        // unchanged, and it is clear of the lookback frontier.
        let commit_limit = self.words.len().saturating_sub(LOOKBACK_WORDS);
        for i in 0..commit_limit {
            if self.words[i].committed {
                continue;
            }
            if self.words[i].stable_updates + 1 >= STABILITY {
                self.words[i].committed = true;
                self.words[i].committed_at = Some(now);
            } else {
                break; // commits are a prefix; nothing later may commit
            }
        }
        self.last_hypothesis = hypothesis.to_string();
    }

    /// The utterance finalized: everything on screen is now committed text
    /// (the finalizer's transcript replaces hypotheses wholesale, so
    /// stability no longer applies — same rule as `CommitHorizon::finish`).
    pub fn finalize(&mut self, now: f64) {
        for w in &mut self.words {
            if !w.committed {
                w.committed = true;
                w.committed_at = Some(now);
            }
        }
    }

    /// Left edge of each word relative to the glance anchor (0.0 = the
    /// newest word's left edge; older words extend negative/leftward).
    /// Returned oldest-first, parallel to [`Self::words`].
    pub fn positions(&self) -> Vec<f64> {
        let mut xs = vec![0.0f64; self.words.len()];
        let mut x = 0.0f64;
        for (i, w) in self.words.iter().enumerate().rev() {
            if i + 1 < self.words.len() {
                x -= w.width + WORD_GAP;
            }
            xs[i] = x;
        }
        xs
    }

    /// Advance one frame: compute each word's target opacity (overflow ∧
    /// staleness), ease the displayed opacity toward it, and retire dead
    /// committed words from the front. Pure function of `(now, dt)` plus
    /// the model — a dropped frame degrades smoothness, never correctness.
    ///
    /// Returns `true` when any displayed opacity actually moved (or a word
    /// was retired), so a renderer can skip repainting a fully settled
    /// frame — that is what lets a visible-but-static overlay (a steady
    /// error line, say) cost no CPU between host updates.
    pub fn step(&mut self, now: f64, dt: f64, reduce_motion: bool) -> bool {
        let xs = self.positions();
        let ease = 1.0 - (-dt.max(0.0) / EASE_TAU).exp();
        let mut moved = false;
        for (w, &x) in self.words.iter_mut().zip(&xs) {
            // 1. Overflow (position): fade over one ramp past the lane's
            //    left edge. This is what produces the brief's example: new
            //    words push old ones across the edge *while the user is
            //    still speaking*, not on a timer.
            let lane_left = -LANE_WIDTH;
            let overflow = if x >= lane_left {
                1.0
            } else {
                (1.0 - (lane_left - x) / FADE_RAMP).max(0.0)
            };
            // 2. Staleness (age): committed words drain after a pause; the
            //    in-flight tail must stay until the horizon resolves it.
            let stale = match w.committed_at {
                Some(t) => {
                    let age = now - t - STALE_AFTER;
                    if age <= 0.0 {
                        1.0
                    } else {
                        (1.0 - age / STALE_FADE).max(0.0)
                    }
                }
                None => 1.0,
            };
            let target = overflow.min(stale);
            if reduce_motion {
                // Reduced motion: three discrete tiers with instant steps.
                // The window still rolls (information preserved); it just
                // does not continuously animate.
                let tier = if target > 0.66 {
                    1.0
                } else if target > 0.15 {
                    0.5
                } else {
                    0.0
                };
                if (tier - w.opacity).abs() > f64::EPSILON {
                    moved = true;
                }
                w.opacity = tier;
            } else {
                // Spend elapsed time on the birth delay first, then ease with
                // whatever remains. Splitting it this way keeps the stagger
                // wall-clock accurate at any refresh rate AND stops a coarse
                // frame from being swallowed whole: a step longer than the
                // delay still leaves the word visibly easing, rather than
                // arriving and sitting blank for a frame.
                let mut remaining = dt;
                if w.birth_delay > 0.0 {
                    let spent = w.birth_delay.min(remaining);
                    w.birth_delay -= spent;
                    remaining -= spent;
                    // Pending work, so never report the frame as settled: a
                    // skipped repaint here is precisely how the cascade would
                    // collapse back into a simultaneous pop.
                    moved = true;
                }
                let ease = if remaining <= 0.0 {
                    0.0
                } else if remaining >= dt {
                    ease
                } else {
                    1.0 - (-remaining / EASE_TAU).exp()
                };
                let delta = (target - w.opacity) * ease;
                // A word still inside its birth delay must not be snapped:
                // the snap below exists to finish an easing that has nearly
                // converged, and applying it to a word that has not begun
                // easing at all would slam it to full opacity, which is the
                // simultaneous pop the stagger exists to prevent.
                if w.birth_delay > 0.0 {
                    continue;
                }
                // Snap when within a quarter-percent: exponential decay
                // never reaches its target, and without a snap "settled"
                // would never be true and the skip-repaint path dead.
                if delta.abs() > 0.0025 {
                    w.opacity += delta;
                    moved = true;
                } else if (target - w.opacity).abs() > f64::EPSILON {
                    w.opacity = target;
                    moved = true;
                }
            }
        }
        // Retire dead words from the front only: the window fades
        // oldest-first, and ordered removal keeps layout stable. Only
        // committed words leave — an overflowed in-flight word (possible
        // under continuous churn) keeps its slot so the hypothesis diff
        // stays aligned; it simply draws nothing at ~0 opacity.
        while let Some(w) = self.words.first() {
            if w.committed && w.opacity < DEAD_OPACITY {
                self.words.remove(0);
                self.dropped += 1;
                moved = true;
            } else {
                break;
            }
        }
        moved
    }
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
    const OVERLAY: Size = OVERLAY_SIZE;

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
    fn bottom_center_is_centered_and_bottom_inset() {
        // The Aqua-compatible placement. Horizontal centering must be exact,
        // because a slightly-off bar is more distracting than an obviously
        // corner-pinned one.
        let r = place_bottom_center(OVERLAY, SCREEN);
        let left = r.origin.x - SCREEN.origin.x;
        let right = SCREEN.max_x() - r.max_x();
        assert!((left - right).abs() < 1e-9, "not centered: {r:?}");
        assert_eq!(r.max_y(), SCREEN.max_y() - SCREEN_MARGIN);
        assert_on_screen(r);
    }

    #[test]
    fn bottom_center_respects_a_secondary_screen() {
        // Centering must be relative to the screen the overlay is on, not to
        // the global coordinate space, or a second monitor drags the bar
        // toward the primary display.
        let screen = Rect::new(-1920.0, 0.0, 1920.0, 1080.0);
        let r = place_bottom_center(OVERLAY, screen);
        let left = r.origin.x - screen.origin.x;
        let right = screen.max_x() - r.max_x();
        assert!(
            (left - right).abs() < 1e-9,
            "not centered on its screen: {r:?}"
        );
    }

    #[test]
    fn bottom_center_survives_an_overlay_wider_than_the_screen() {
        let huge = Size {
            width: 5000.0,
            height: 5000.0,
        };
        let r = place_bottom_center(huge, SCREEN);
        assert_eq!(r.origin.x, SCREEN.origin.x + SCREEN_MARGIN);
        assert_eq!(r.origin.y, SCREEN.origin.y + SCREEN_MARGIN);
    }

    #[test]
    fn the_card_is_a_wide_low_bar_not_a_dialog() {
        // Aqua-parity silhouette: the shipping size must stay markedly
        // wider than tall. A future "size to fit the text" change that
        // squares the card off would lose the family resemblance.
        const {
            assert!(
                OVERLAY_SIZE.width >= OVERLAY_SIZE.height * 4.0,
                "the overlay is too square to read as a floating bar"
            );
        }
    }

    #[test]
    fn screen_margin_clears_the_card_radius() {
        // A rounded card inset by less than its own corner radius looks
        // jammed into the screen edge, and vanishes into rounded displays.
        const {
            assert!(
                SCREEN_MARGIN >= crate::theme::CARD_RADIUS - 4.0,
                "screen margin is too small for the card's corner radius"
            );
        }
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

    // ---- Rolling window ----

    /// Fixed-width measurer: every unit is 60pt, so LANE_WIDTH (440) holds
    /// about 6-7 words before overflow.
    fn measure60(_: &str) -> f64 {
        60.0
    }

    /// Ingest `hyp` and settle animation by stepping well past τ.
    fn settle(win: &mut RollingWindow, now: f64) {
        for i in 0..120 {
            win.step(now + i as f64 * (1.0 / 60.0), 1.0 / 60.0, false);
        }
    }

    #[test]
    fn words_bloom_in_from_transparent() {
        let mut w = RollingWindow::new();
        w.ingest("hello", 0.0, &mut measure60);
        assert_eq!(w.words()[0].opacity, 0.0, "born transparent");
        w.step(0.1, 0.1, false);
        assert!(w.words()[0].opacity > 0.2, "eases up within one τ");
    }

    #[test]
    fn overflow_fades_the_oldest_while_still_speaking() {
        // The owner's example: by "has a lot of fun", "the dog is brown"
        // must be fading. 12 sixty-point words = 780pt >> the 440pt lane.
        let mut w = RollingWindow::new();
        let hyp = "w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11";
        w.ingest(hyp, 0.0, &mut measure60);
        settle(&mut w, 0.0);
        let words = w.words();
        assert!(
            words.first().unwrap().opacity < 0.1,
            "oldest word must have overflowed out, got {}",
            words.first().unwrap().opacity
        );
        assert!(
            words.last().unwrap().opacity > 0.9,
            "newest word must be fully visible"
        );
        // No staleness involved: nothing here is committed, and the fade
        // happened at now=0, i.e. purely positional.
        assert!(words.iter().all(|x| !x.committed));
    }

    #[test]
    fn stability_commits_a_prefix_and_lookback_holds_the_frontier() {
        let mut w = RollingWindow::new();
        // Three distinct agreeing hypotheses = STABILITY(3) reached.
        w.ingest("the dog", 0.0, &mut measure60);
        w.ingest("the dog is", 0.1, &mut measure60);
        w.ingest("the dog is brown", 0.2, &mut measure60);
        let committed: Vec<_> = w.words().iter().map(|x| x.committed).collect();
        // "the" and "dog" survived 3 hypotheses; "is" only 2; "brown" is
        // both unstable and inside the lookback frontier.
        assert_eq!(committed, [true, true, false, false]);
    }

    #[test]
    fn revision_only_churns_the_inflight_tail() {
        let mut w = RollingWindow::new();
        w.ingest("recognize speech", 0.0, &mut measure60);
        w.ingest("recognize speech", 0.1, &mut measure60);
        // Unchanged hypothesis re-ingested: idempotent (frame-rate polling).
        let before = w.words().len();
        w.ingest("recognize speech", 0.1, &mut measure60);
        assert_eq!(w.words().len(), before);
        // A revision rewrites in-flight slots in place, keeping opacity.
        w.step(0.2, 0.2, false);
        let old_opacity = w.words()[1].opacity;
        assert!(old_opacity > 0.0);
        w.ingest("recognize speedy", 0.3, &mut measure60);
        assert_eq!(w.words()[1].text, "speedy");
        assert_eq!(
            w.words()[1].opacity,
            old_opacity,
            "revision must not re-bloom"
        );
    }

    #[test]
    fn committed_words_stale_decay_but_inflight_never_does() {
        let mut w = RollingWindow::new();
        for t in [0.0, 0.1, 0.2, 0.3] {
            w.ingest("alpha beta gamma", t, &mut measure60);
            // Distinct hypotheses required for stability: append a tail.
        }
        // Force distinct updates so the prefix commits.
        w.ingest("alpha beta gamma d", 0.4, &mut measure60);
        w.ingest("alpha beta gamma de", 0.5, &mut measure60);
        assert!(w.words()[0].committed);
        let n_inflight = w.words().iter().filter(|x| !x.committed).count();
        assert!(n_inflight > 0);
        // Long pause: past STALE_AFTER + STALE_FADE the committed prefix
        // drains; the in-flight tail stays visible.
        settle(&mut w, 0.5 + STALE_AFTER + STALE_FADE + 1.0);
        assert!(
            w.words()
                .iter()
                .all(|x| !x.committed || x.opacity < DEAD_OPACITY),
            "committed words must have drained"
        );
        assert!(
            w.words()
                .iter()
                .filter(|x| !x.committed)
                .all(|x| x.opacity > 0.9),
            "in-flight words must never stale-decay"
        );
    }

    #[test]
    fn continuous_speech_is_bounded_in_memory() {
        // 30s of speech at ~3 words/s: the slot list must stay bounded by
        // the lane, not grow with utterance length. Committed words that
        // overflow are removed outright.
        let mut w = RollingWindow::new();
        let mut hyp = String::new();
        for i in 0..90 {
            let now = i as f64 / 3.0;
            if !hyp.is_empty() {
                hyp.push(' ');
            }
            hyp.push_str(&format!("w{i}"));
            w.ingest(&hyp, now, &mut measure60);
            for f in 0..20 {
                w.step(now + f as f64 / 60.0, 1.0 / 60.0, false);
            }
        }
        assert!(
            w.words().len() < 20,
            "window must be bounded, got {} slots",
            w.words().len()
        );
    }

    #[test]
    fn finalize_commits_everything_on_screen() {
        let mut w = RollingWindow::new();
        w.ingest("done and dusted", 0.0, &mut measure60);
        w.finalize(0.1);
        assert!(w.words().iter().all(|x| x.committed));
    }

    #[test]
    fn reduce_motion_steps_between_three_tiers() {
        let mut w = RollingWindow::new();
        w.ingest("only", 0.0, &mut measure60);
        w.step(0.0, 1.0 / 60.0, true);
        // Instant step to the visible tier, no gradual bloom.
        assert_eq!(w.words()[0].opacity, 1.0);
    }

    #[test]
    fn cjk_segments_per_syllable_unit() {
        // Point budget, per-syllable units: the design's CJK-correct-by-
        // construction claim depends on this segmentation.
        assert_eq!(split_units("今日は良い"), ["今", "日", "は", "良", "い"]);
        assert_eq!(split_units("hello 世界 ok"), ["hello", "世", "界", "ok"]);
    }

    #[test]
    fn newest_word_is_the_position_origin() {
        let mut w = RollingWindow::new();
        w.ingest("a b c", 0.0, &mut measure60);
        let xs = w.positions();
        // Glance anchor: the newest word sits at 0; older words extend left.
        assert_eq!(*xs.last().unwrap(), 0.0);
        assert!(xs[0] < xs[1] && xs[1] < xs[2]);
    }

    /// Words arriving together must fade up in sequence, not in unison.
    ///
    /// This is the whole point of the stagger. Apple's SpeechTranscriber
    /// emits a revised hypothesis roughly every 1.3s, so a burst of words
    /// lands in one frame; without staggering they shared a single easing
    /// constant and appeared simultaneously, which reads as a block of text
    /// being stamped down rather than as speech arriving.
    #[test]
    fn a_burst_of_words_cascades_rather_than_flashing() {
        let mut w = RollingWindow::new();
        w.ingest("one two three four", 0.0, &mut measure60);

        // One frame at 120Hz: only the first word has had any time to ease,
        // because each later word is still burning its birth delay.
        w.step(1.0 / 120.0, 1.0 / 120.0, false);
        let op: Vec<f64> = w.words().iter().map(|x| x.opacity).collect();
        assert!(op[0] > 0.0, "the first word never waits: {op:?}");
        assert!(
            op[0] > op[1] && op[1] >= op[2] && op[2] >= op[3],
            "opacity must decrease left to right during the cascade: {op:?}"
        );

        // Well past the whole cascade, everything has caught up, so the
        // stagger costs nothing in the steady state.
        for _ in 0..120 {
            w.step(1.0 / 120.0, 1.0 / 120.0, false);
        }
        let op: Vec<f64> = w.words().iter().map(|x| x.opacity).collect();
        assert!(
            op.iter().all(|&o| o > 0.9),
            "all words settle once the cascade finishes: {op:?}"
        );
    }

    /// Reduced motion keeps its instant tiers: the cascade is animation, and
    /// a user who asked for less of it should not be made to wait for words.
    #[test]
    fn reduced_motion_skips_the_cascade() {
        let mut w = RollingWindow::new();
        w.ingest("one two three four", 0.0, &mut measure60);
        w.step(1.0 / 120.0, 1.0 / 120.0, true);
        let op: Vec<f64> = w.words().iter().map(|x| x.opacity).collect();
        assert!(
            op.iter().all(|&o| o > 0.9),
            "reduced motion shows the burst at once: {op:?}"
        );
    }
}
