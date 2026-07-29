//! The text lane's rolling-window model, pure and headless-testable.
//!
//! This supersedes [`crate::layout::RollingWindow`] for the macOS text
//! lane (that type stays as-is because it is owned by another lane of
//! work; consolidating the two is a known follow-up). What this model
//! changes, and why:
//!
//! 1. **Smoothstep fade ramps.** The linear overflow/stale ramps read as
//!    mechanical wipes: constant-velocity opacity has a visible "corner"
//!    at both ends of the ramp. `smoothstep` (3t² − 2t³) has zero slope
//!    at 0 and 1, so a word eases *into* its fade and eases *out of*
//!    existence — the same C¹-continuity argument the design already
//!    makes for the exponential opacity easing, applied to the target
//!    curve itself.
//! 2. **Group stale decay.** The shipped model timestamped each word at
//!    commitment, so a pause made words decay one-by-one in commit order
//!    — a distracting drip-feed of disappearance. A pause should read as
//!    "hold, then let the whole thought go": the stale clock is now the
//!    time since the hypothesis last changed, shared by every committed
//!    word, so after `STALE_AFTER` of silence the settled text fades out
//!    *as one group*. In-flight words still never stale-decay (text that
//!    might change must stay visible until the horizon resolves it).
//! 3. **Position glide.** Layout positions were instantaneous, so every
//!    new word made all older words jump left by one word-width — the
//!    "text jumps when a word is added" failure the brief names. Each
//!    slot now eases its displayed x toward the layout target with a
//!    short time constant, so the line *flows* leftward. New words are
//!    born at their target (the fixed glance anchor) so the bloom-in
//!    point never moves.
//! 4. **Middle elision for pathological units.** A very long word or a
//!    pasted-looking URL can be wider than the lane; drawn raw it would
//!    escape the panel and be clipped mid-glyph. Units wider than
//!    [`MAX_UNIT_WIDTH`] are elided in the middle ("start…end"), which
//!    keeps both the recognizable head and the distinguishing tail of a
//!    URL. Elision happens once at ingest (measure time), never per
//!    frame. The lane itself never wraps and never clips mid-word: fade
//!    is per whole unit, and CJK segments per syllable upstream
//!    ([`crate::layout::split_units`]) so "no spaces" scripts still get
//!    word-boundary-like granularity.
//!
//! Reduce Motion (`step`'s flag): opacity becomes three discrete tiers
//! with instant steps and positions snap to their targets — the window
//! still rolls and stale text still leaves, it just does not animate.

use crate::layout::{split_units, DEAD_OPACITY, LOOKBACK_WORDS, STABILITY};

/// Width budget of the text lane, in points — never characters (the lane
/// is CJK-correct because units are measured, not counted). 400 rather
/// than the design's 440: the newest word's anchor sits at x≈456 in a
/// 760pt panel, and 400 + the 48pt fade ramp is the widest lane whose
/// fade *completes inside the panel*. At 440 the tail of the ramp fell
/// past the panel's left edge and fading words were hard-clipped
/// mid-glyph instead of ramping out.
pub const LANE_WIDTH: f64 = 400.0;
/// Distance past the lane's left edge over which a word ramps 1 → 0.
pub const FADE_RAMP: f64 = 48.0;
/// Gap between adjacent words, in points.
pub const WORD_GAP: f64 = 6.5;
/// Opacity easing time constant, seconds (exponential approach).
pub const EASE_TAU: f64 = 0.22;
/// Position glide time constant, seconds. Shorter than the opacity τ:
/// position lag longer than ~150ms makes the line feel like it is being
/// dragged; opacity can afford to be lazier than motion.
pub const GLIDE_TAU: f64 = 0.12;
/// Seconds of hypothesis silence before committed words begin to drain.
pub const STALE_AFTER: f64 = 4.0;
/// Duration of the (group) stale fade, seconds.
pub const STALE_FADE: f64 = 2.0;
/// Widest a single displayed unit may be. Chosen so the newest word can
/// never escape the panel's right edge (panel 760 − anchor 456 − margin),
/// and so one monster token cannot occupy the whole lane.
pub const MAX_UNIT_WIDTH: f64 = 280.0;

/// Hermite smoothstep: 3t² − 2t³ on the clamped unit interval. Zero
/// derivative at both ends, which is what makes a fade read as motion
/// instead of a linear wipe.
pub fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Middle-elide `unit` until it measures at most `max` points. Returns
/// the display text and its measured width. Head is favored ~2:1 over
/// tail: the start of a long token identifies it, the tail disambiguates
/// (…the common URL case). Called only when a unit is over-wide, so the
/// repeated measuring never happens in the per-frame path.
pub fn elide_to_width(
    unit: &str,
    max: f64,
    measure: &mut dyn FnMut(&str) -> f64,
) -> (String, f64) {
    let w = measure(unit);
    if w <= max {
        return (unit.to_string(), w);
    }
    let chars: Vec<char> = unit.chars().collect();
    // Shrink the kept-character budget until the elided form fits. Linear
    // scan is fine: this runs once per over-wide unit at ingest.
    let mut keep = chars.len().saturating_sub(1);
    while keep > 1 {
        let head_n = keep * 2 / 3;
        let tail_n = keep - head_n;
        let mut text: String = chars[..head_n].iter().collect();
        text.push('…');
        text.extend(&chars[chars.len() - tail_n..]);
        let tw = measure(&text);
        if tw <= max {
            return (text, tw);
        }
        keep -= 1;
    }
    // Degenerate: even one char + ellipsis is too wide. Show the ellipsis
    // alone rather than clip glyphs.
    let e = "…".to_string();
    let ew = measure(&e);
    (e, ew)
}

/// One display slot in the lane.
#[derive(Debug, Clone)]
pub struct TextSlot {
    /// The raw hypothesis unit, used for diffing against later
    /// hypotheses. Never elided: comparing the elided form against the
    /// raw unit would misread every re-ingest as a revision.
    unit: String,
    /// What is drawn: the unit, middle-elided if over-wide.
    pub text: String,
    /// Measured display width in points, cached at (re)ingest.
    pub width: f64,
    /// Settled by the display-side stability policy. Committed and
    /// in-flight are styled differently on purpose: the boundary between
    /// them IS the commit horizon, made visible.
    pub committed: bool,
    /// Consecutive distinct hypotheses that agreed on this unit.
    stable_updates: usize,
    /// Displayed opacity, eased toward the per-frame target.
    pub opacity: f64,
    /// Displayed left edge relative to the glance anchor (0 = newest
    /// word's left edge), glided toward the layout target. NaN until the
    /// first step snaps it (a new word is born *at* its target).
    pub x: f64,
}

/// The rolling window of transcribed words.
///
/// Feed the whole current hypothesis with [`TextWindow::ingest`]
/// (idempotent per distinct hypothesis, so frame-rate polling is free),
/// then advance the animation with [`TextWindow::step`] once per frame.
#[derive(Debug, Default)]
pub struct TextWindow {
    slots: Vec<TextSlot>,
    /// Units the display already retired (committed, faded, removed).
    /// Needed to realign the hypothesis' unit list with surviving slots.
    dropped: usize,
    last_hypothesis: String,
    /// When the hypothesis last changed (or `finalize` ran): the shared
    /// clock for the *group* stale decay. One clock for all committed
    /// words is what makes a pause read as "hold, then fade together"
    /// instead of a word-by-word drip of disappearance.
    last_change: f64,
}

impl TextWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current slots, oldest first.
    pub fn slots(&self) -> &[TextSlot] {
        &self.slots
    }

    /// Forget everything (new utterance, overlay hidden).
    pub fn reset(&mut self) {
        self.slots.clear();
        self.dropped = 0;
        self.last_hypothesis.clear();
        self.last_change = 0.0;
    }

    /// Absorb the whole current hypothesis. `measure` maps display text
    /// to points; it is only called for new or rewritten units, never in
    /// the frame path. Committed slots are append-only and never
    /// rewritten; the in-flight tail is diffed unit-wise and rewritten
    /// in place, so revision only ever churns the tinted zone.
    pub fn ingest(&mut self, hypothesis: &str, now: f64, measure: &mut dyn FnMut(&str) -> f64) {
        if hypothesis == self.last_hypothesis {
            return; // frame-rate polling of an unchanged hypothesis
        }
        // Speech is active: (re)arm the group stale clock. Doing this on
        // *change* (not on every poll) is what lets a genuine pause run
        // the clock down while an unchanged hypothesis keeps arriving.
        self.last_change = now;
        let units = split_units(hypothesis);
        let fresh = units.into_iter().skip(self.dropped);
        let mut j = 0usize;
        for unit in fresh {
            match self.slots.get_mut(j) {
                Some(s) if s.committed => {
                    // Committed text is visually guaranteed stable, even
                    // against a divergent hypothesis (matches
                    // stream::CommitHorizon's monotonic commits).
                }
                Some(s) => {
                    if s.unit == unit {
                        s.stable_updates += 1;
                    } else {
                        // Rewritten in place. Opacity and position are
                        // kept: a revision replaces glyphs, it does not
                        // re-bloom (which would read as new speech).
                        let (text, width) = elide_to_width(&unit, MAX_UNIT_WIDTH, measure);
                        s.text = text;
                        s.width = width;
                        s.unit = unit;
                        s.stable_updates = 0;
                    }
                }
                None => {
                    let (text, width) = elide_to_width(&unit, MAX_UNIT_WIDTH, measure);
                    self.slots.push(TextSlot {
                        unit,
                        text,
                        width,
                        committed: false,
                        stable_updates: 0,
                        opacity: 0.0, // born transparent; blooms in
                        x: f64::NAN,  // snapped to target on first step
                    });
                }
            }
            j += 1;
        }
        // Hypothesis shrank: drop in-flight slots past its end (guard the
        // committed prefix so a pathological feed cannot erase it).
        while self.slots.len() > j && !self.slots.last().is_none_or(|s| s.committed) {
            self.slots.pop();
        }
        // Prefix-wise commit pass (LocalAgreement mirror): a word renders
        // committed once everything before it is committed, it survived
        // `STABILITY` distinct hypotheses, and it is clear of the
        // lookback frontier.
        let commit_limit = self.slots.len().saturating_sub(LOOKBACK_WORDS);
        for i in 0..commit_limit {
            if self.slots[i].committed {
                continue;
            }
            if self.slots[i].stable_updates + 1 >= STABILITY {
                self.slots[i].committed = true;
            } else {
                break; // commits are a prefix
            }
        }
        self.last_hypothesis = hypothesis.to_string();
    }

    /// The utterance finalized: everything on screen is now committed
    /// (the finalizer's transcript replaces hypotheses wholesale). Also
    /// re-arms the group stale clock so the finalized line holds for a
    /// readable beat before draining.
    pub fn finalize(&mut self, now: f64) {
        for s in &mut self.slots {
            s.committed = true;
        }
        self.last_change = now;
    }

    /// Layout-target left edge of each slot relative to the glance
    /// anchor (0 = newest word's left edge; older words extend
    /// negative). Oldest-first, parallel to [`Self::slots`].
    fn target_positions(&self) -> Vec<f64> {
        let mut xs = vec![0.0f64; self.slots.len()];
        let mut x = 0.0f64;
        for (i, s) in self.slots.iter().enumerate().rev() {
            if i + 1 < self.slots.len() {
                x -= s.width + WORD_GAP;
            }
            xs[i] = x;
        }
        xs
    }

    /// Advance one frame: compute each slot's target opacity
    /// (smoothstepped overflow ∧ group staleness), ease displayed
    /// opacity and glide displayed position toward their targets, and
    /// retire dead committed words from the front. Pure function of
    /// `(now, dt)` plus the model.
    ///
    /// Returns `true` when anything visibly moved, so a renderer can
    /// skip repainting a fully settled frame.
    pub fn step(&mut self, now: f64, dt: f64, reduce_motion: bool) -> bool {
        let targets = self.target_positions();
        let ease = 1.0 - (-dt.max(0.0) / EASE_TAU).exp();
        let glide = 1.0 - (-dt.max(0.0) / GLIDE_TAU).exp();
        // One shared stale factor: silence holds everything at full for
        // STALE_AFTER, then the whole committed group fades over
        // STALE_FADE. If speech resumes mid-fade, `ingest` re-arms the
        // clock and the exponential easing bends opacity back up.
        let idle = now - self.last_change;
        let group_stale = if idle <= STALE_AFTER {
            1.0
        } else {
            smoothstep(1.0 - (idle - STALE_AFTER) / STALE_FADE)
        };
        let mut moved = false;
        for (s, &tx) in self.slots.iter_mut().zip(&targets) {
            // Overflow term, from the layout *target* (the conveyor's
            // truth) rather than the glided position, so fade timing is
            // independent of glide lag.
            let past = -LANE_WIDTH - tx;
            let overflow = if past <= 0.0 {
                1.0
            } else {
                smoothstep(1.0 - past / FADE_RAMP)
            };
            let stale = if s.committed { group_stale } else { 1.0 };
            let target = overflow.min(stale);

            if reduce_motion {
                // Three discrete opacity tiers with instant steps, and
                // positions snap: the window still rolls, it does not
                // flow. Information preserved, oscillation removed.
                let tier = if target > 0.66 {
                    1.0
                } else if target > 0.15 {
                    0.5
                } else {
                    0.0
                };
                if (tier - s.opacity).abs() > f64::EPSILON {
                    s.opacity = tier;
                    moved = true;
                }
                if s.x.is_nan() || (s.x - tx).abs() > f64::EPSILON {
                    s.x = tx;
                    moved = true;
                }
                continue;
            }

            // Opacity: exponential approach with a snap once within a
            // quarter-percent (exponential decay never arrives, and
            // without the snap "settled" would never be true).
            let delta = (target - s.opacity) * ease;
            if delta.abs() > 0.0025 {
                s.opacity += delta;
                moved = true;
            } else if (target - s.opacity).abs() > f64::EPSILON {
                s.opacity = target;
                moved = true;
            }

            // Position: born at the target (the glance anchor never
            // moves), then glided when layout shifts left under it.
            // Sub-pixel throughout — x stays f64 and is never rounded,
            // so a slow glide renders as smooth motion, not 1px steps.
            if s.x.is_nan() {
                s.x = tx;
            } else {
                let dx = (tx - s.x) * glide;
                if dx.abs() > 0.02 {
                    s.x += dx;
                    moved = true;
                } else if (tx - s.x).abs() > f64::EPSILON {
                    s.x = tx;
                    moved = true;
                }
            }
        }
        // Retire dead words from the front only (the window fades
        // oldest-first; ordered removal keeps the diff aligned). Only
        // committed words leave — an overflowed in-flight word keeps its
        // slot and simply draws nothing at ~0 opacity.
        while let Some(s) = self.slots.first() {
            if s.committed && s.opacity < DEAD_OPACITY {
                self.slots.remove(0);
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

    /// Fixed-width measurer: every unit is 60pt, so the 400pt lane holds
    /// ~6 words before overflow.
    fn measure60(_: &str) -> f64 {
        60.0
    }

    /// Per-char measurer for elision tests: 10pt per char.
    fn measure_chars(s: &str) -> f64 {
        s.chars().count() as f64 * 10.0
    }

    fn settle(win: &mut TextWindow, now: f64) {
        for i in 0..240 {
            win.step(now + i as f64 / 60.0, 1.0 / 60.0, false);
        }
    }

    #[test]
    fn smoothstep_is_clamped_monotone_with_flat_ends() {
        assert_eq!(smoothstep(-1.0), 0.0);
        assert_eq!(smoothstep(2.0), 1.0);
        assert_eq!(smoothstep(0.5), 0.5);
        let mut prev = 0.0;
        for i in 0..=100 {
            let v = smoothstep(i as f64 / 100.0);
            assert!(v >= prev);
            prev = v;
        }
        // Flat ends: the first/last 1% of input moves the output far less
        // than the middle 1% does — that is the non-mechanical property.
        let edge = smoothstep(0.01) - smoothstep(0.0);
        let mid = smoothstep(0.505) - smoothstep(0.495);
        assert!(edge < mid / 4.0, "ends must ease: edge {edge} mid {mid}");
    }

    #[test]
    fn words_bloom_in_from_transparent_at_their_target() {
        let mut w = TextWindow::new();
        w.ingest("hello", 0.0, &mut measure60);
        assert_eq!(w.slots()[0].opacity, 0.0, "born transparent");
        assert!(w.slots()[0].x.is_nan(), "position set on first step");
        w.step(0.1, 0.1, false);
        assert!(w.slots()[0].opacity > 0.2, "eases up within one τ");
        assert_eq!(w.slots()[0].x, 0.0, "born at the glance anchor");
    }

    #[test]
    fn overflow_fades_the_oldest_while_still_speaking() {
        let mut w = TextWindow::new();
        let hyp = "w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11";
        w.ingest(hyp, 0.0, &mut measure60);
        settle(&mut w, 0.0);
        let slots = w.slots();
        assert!(slots.first().unwrap().opacity < 0.1, "oldest overflowed");
        assert!(slots.last().unwrap().opacity > 0.9, "newest fully visible");
        assert!(slots.iter().all(|s| !s.committed), "purely positional");
    }

    #[test]
    fn positions_glide_instead_of_jumping() {
        let mut w = TextWindow::new();
        w.ingest("alpha", 0.0, &mut measure60);
        settle(&mut w, 0.0);
        assert_eq!(w.slots()[0].x, 0.0);
        // A new word pushes "alpha" left — but only by easing, never in
        // one frame.
        w.ingest("alpha beta", 4.0, &mut measure60);
        w.step(4.0 + 1.0 / 60.0, 1.0 / 60.0, false);
        let x = w.slots()[0].x;
        let target = -(60.0 + WORD_GAP);
        assert!(x > target && x < 0.0, "must be mid-glide, got {x}");
        settle(&mut w, 4.1);
        assert_eq!(w.slots()[0].x, target, "glide converges to layout");
    }

    #[test]
    fn pause_holds_then_fades_committed_words_as_a_group() {
        let mut w = TextWindow::new();
        // Three distinct agreeing hypotheses commit the prefix.
        w.ingest("the dog", 0.0, &mut measure60);
        w.ingest("the dog is", 0.5, &mut measure60);
        w.ingest("the dog is brown", 1.0, &mut measure60);
        let committed: Vec<bool> = w.slots().iter().map(|s| s.committed).collect();
        assert_eq!(committed, [true, true, false, false]);
        // Hold: well into the pause but before STALE_AFTER, everything
        // stays at full opacity.
        settle(&mut w, 1.0 + STALE_AFTER - 1.5);
        assert!(w.slots().iter().all(|s| s.opacity > 0.9), "hold phase");
        // Mid-decay: every committed word carries the SAME fade factor —
        // that is the "as a group" property (the old model decayed them
        // one-by-one on per-word commit clocks).
        let mid = 1.0 + STALE_AFTER + STALE_FADE * 0.5;
        for i in 0..30 {
            w.step(mid + i as f64 / 60.0, 1.0 / 60.0, false);
        }
        let committed_ops: Vec<f64> = w
            .slots()
            .iter()
            .filter(|s| s.committed)
            .map(|s| s.opacity)
            .collect();
        assert!(committed_ops.len() >= 2);
        let spread = committed_ops
            .iter()
            .fold(0.0f64, |m, &o| m.max((o - committed_ops[0]).abs()));
        assert!(spread < 0.05, "group fade must move together: {committed_ops:?}");
        assert!(
            committed_ops.iter().all(|&o| o < 0.9),
            "decay must be underway: {committed_ops:?}"
        );
        // In-flight words never stale-decay.
        assert!(w
            .slots()
            .iter()
            .filter(|s| !s.committed)
            .all(|s| s.opacity > 0.9));
        // Fully drained after the ramp: committed words are gone.
        settle(&mut w, 1.0 + STALE_AFTER + STALE_FADE + 1.0);
        assert!(w.slots().iter().all(|s| !s.committed));
    }

    #[test]
    fn resuming_speech_rescues_a_mid_decay_group() {
        let mut w = TextWindow::new();
        w.ingest("a b", 0.0, &mut measure60);
        w.ingest("a b c", 0.2, &mut measure60);
        w.ingest("a b c d", 0.4, &mut measure60);
        assert!(w.slots()[0].committed);
        // Run into the decay, then resume speaking: the group must come
        // back rather than continuing to die.
        let mid = 0.4 + STALE_AFTER + STALE_FADE * 0.4;
        for i in 0..30 {
            w.step(mid + i as f64 / 60.0, 1.0 / 60.0, false);
        }
        assert!(w.slots()[0].opacity < 0.95, "decay underway");
        w.ingest("a b c d e", mid + 0.6, &mut measure60);
        settle(&mut w, mid + 0.7);
        assert!(
            w.slots()[0].opacity > 0.9,
            "resumed speech must rescue the group, got {}",
            w.slots()[0].opacity
        );
    }

    #[test]
    fn revision_only_churns_the_inflight_tail_and_keeps_motion_state() {
        let mut w = TextWindow::new();
        w.ingest("recognize speech", 0.0, &mut measure60);
        w.step(0.2, 0.2, false);
        let old_opacity = w.slots()[1].opacity;
        let old_x = w.slots()[1].x;
        assert!(old_opacity > 0.0);
        w.ingest("recognize speedy", 0.3, &mut measure60);
        assert_eq!(w.slots()[1].text, "speedy");
        assert_eq!(w.slots()[1].opacity, old_opacity, "no re-bloom");
        assert_eq!(w.slots()[1].x, old_x, "no position jump on revision");
    }

    #[test]
    fn finalize_commits_everything_and_rearms_the_stale_clock() {
        let mut w = TextWindow::new();
        w.ingest("done and dusted", 0.0, &mut measure60);
        w.finalize(10.0);
        assert!(w.slots().iter().all(|s| s.committed));
        // The finalized line holds for STALE_AFTER from finalize time,
        // not from when the words appeared.
        settle(&mut w, 10.0 + STALE_AFTER - 1.5);
        assert!(w.slots().iter().all(|s| s.opacity > 0.9));
    }

    #[test]
    fn continuous_speech_is_bounded_in_memory() {
        let mut w = TextWindow::new();
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
        assert!(w.slots().len() < 20, "bounded, got {}", w.slots().len());
    }

    #[test]
    fn reduce_motion_steps_instantly_and_snaps_positions() {
        let mut w = TextWindow::new();
        w.ingest("only", 0.0, &mut measure60);
        w.step(0.0, 1.0 / 60.0, true);
        assert_eq!(w.slots()[0].opacity, 1.0, "instant, no bloom");
        assert_eq!(w.slots()[0].x, 0.0);
        // The roll itself is preserved: stale drain still empties the
        // lane, just as a step.
        w.finalize(0.1);
        w.step(0.1 + STALE_AFTER + STALE_FADE + 1.0, 1.0 / 60.0, true);
        assert!(w.slots().is_empty(), "reduced motion still drains");
    }

    #[test]
    fn overwide_units_are_middle_elided_not_clipped() {
        let long = "supercalifragilisticexpialidocious.example.com/very/long/path";
        let (text, width) = elide_to_width(long, MAX_UNIT_WIDTH, &mut measure_chars);
        assert!(width <= MAX_UNIT_WIDTH, "must fit: {width}");
        assert!(text.contains('…'), "middle elision marker");
        assert!(text.starts_with("supercali"), "head preserved");
        assert!(text.ends_with("path"), "tail preserved");
        // Short units pass through untouched.
        let (t, _) = elide_to_width("ok", MAX_UNIT_WIDTH, &mut measure_chars);
        assert_eq!(t, "ok");
    }

    #[test]
    fn elided_unit_still_diffs_and_commits_by_its_raw_text() {
        // The displayed (elided) text must not confuse the hypothesis
        // diff: re-ingesting the same long unit is stability, not a
        // revision.
        let long = "a".repeat(60);
        let mut w = TextWindow::new();
        let hyp1 = long.clone();
        let hyp2 = format!("{long} next");
        let hyp3 = format!("{long} next more");
        w.ingest(&hyp1, 0.0, &mut measure_chars);
        w.ingest(&hyp2, 0.1, &mut measure_chars);
        w.ingest(&hyp3, 0.2, &mut measure_chars);
        assert!(w.slots()[0].committed, "stability must survive elision");
        assert!(w.slots()[0].text.contains('…'));
    }

    #[test]
    fn cjk_units_roll_through_the_same_lane() {
        // Point budget + per-syllable units from split_units: CJK needs
        // no special casing here, but prove it flows end to end.
        let mut w = TextWindow::new();
        w.ingest("今日は良い天気", 0.0, &mut |s: &str| {
            s.chars().count() as f64 * 17.0
        });
        assert_eq!(w.slots().len(), 7);
        settle(&mut w, 0.0);
        assert!(w.slots().iter().all(|s| s.opacity > 0.9), "all fit in lane");
    }

    #[test]
    fn newest_word_is_the_position_origin() {
        let mut w = TextWindow::new();
        w.ingest("a b c", 0.0, &mut measure60);
        settle(&mut w, 0.0);
        let xs: Vec<f64> = w.slots().iter().map(|s| s.x).collect();
        assert_eq!(*xs.last().unwrap(), 0.0);
        assert!(xs[0] < xs[1] && xs[1] < xs[2]);
    }
}
