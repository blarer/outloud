# Overlay redesign: the orb and the rolling window

Status: design + working prototype (`cargo run -p overlay --bin overlay-proto`).
Owner brief: replace the generic card with (1) a glowing orb pinned to the
bottom of the screen and (2) a *flowing rolling window* of transcribed text —
"by the time he reaches 'has a lot of fun', 'the dog is brown' should already
be fading away."

This document works the problem out; the prototype demonstrates the answers.
Nothing here changes `crates/overlay/src/{macos,layout,state,theme}.rs`; a
separate framework evaluation owns those files.

---

## 1. The fade model

Three candidate models were considered:

| Model | Rule | Failure mode |
|---|---|---|
| Age-only | opacity = f(seconds since word appeared) | Fast speech: words die while still the newest thing on screen. Slow speech: a 6-word line lives forever and becomes the "ever-growing block" we were told to kill. |
| Position-only | opacity = f(distance from right edge) | A long pause leaves stale text at full brightness indefinitely; nothing about the display says "this is old". |
| **Overflow + age (chosen)** | position decides *whether* a word must fade, age decides stale decay during pauses, and the commit horizon decides *how it is styled* | none observed in the prototype |

### The chosen model, precisely

The overlay keeps an ordered list of word slots. Each frame it computes a
**target opacity** per word and eases the displayed opacity toward it (see
§2 for the easing). Target opacity is the minimum of three terms:

1. **Overflow term (position).** Words are laid out right-to-left from the
   newest. The lane has a fixed width budget `LANE_WIDTH`. Any word whose
   layout position falls left of the lane start gets a target that ramps
   from 1 → 0 over one `FADE_RAMP` distance (48 pt) past the edge. This is
   what produces the brief's example exactly: as "has a lot of fun" pushes
   in from the right, "the dog is brown" is pushed across the left edge and
   ramps out — *while the user is still speaking*, not on a timer.
2. **Staleness term (age).** A **committed** word starts decaying
   `STALE_AFTER` (4 s) after commitment, reaching 0 over `STALE_FADE` (2 s).
   Committed text already lives in the target field — the field is the
   source of truth (UX doc 02) — so the overlay repeating it forever is
   pure noise. On a long pause the lane therefore empties itself back to
   just the orb, which is the correct "invisible by default" resting shape.
   **In-flight** words never stale-decay: text that might still change must
   stay visible until the horizon resolves it, or the user cannot see what
   the recognizer is still unsure about.
3. **Removal.** A word whose displayed opacity reaches ~0 is dropped from
   the model. The list is therefore bounded by the lane width, not by
   utterance length: 30 seconds of continuous speech holds a constant ~8-12
   visible words and constant memory.

### Committed vs in-flight styling (the free information)

`stream::CommitHorizon` splits every hypothesis into a **committed prefix**
(stable across N hypotheses, never retracted, already written to the field)
and an **in-flight tail** (may still be rewritten). The old overlay threw
this away and drew one dim string. The redesign makes it the primary visual
axis:

- **Committed words**: solid near-white (`PAPER` at 0.95), regular weight.
  They read as *settled type* — done, safe, boring.
- **In-flight words**: state-accent tint (aqua) at 0.60 base opacity, and
  they are the only words allowed to be *replaced in place* when the
  hypothesis revises. Revision therefore only ever churns the tinted zone;
  the white zone is visually guaranteed stable, which is the never-retract
  property (`stream::horizon`) made visible.
- The boundary between white and tinted text IS the commit horizon. A user
  who watches for two utterances learns, without being told, that tinted
  text is "still thinking" — genuinely novel feedback no shipping dictation
  overlay gives.

Model input is exactly `HorizonUpdate { newly_committed, tail }`: committed
words are appended once and never touched; the tail is diffed word-wise and
rewritten in place. The prototype scripts this feed; production wires
`CommitHorizon::update` straight in.

## 2. Timing: what makes it "flowy"

- **Exponential easing, not linear tweens.** Displayed opacity approaches
  its target as `o += (target − o) · (1 − e^(−dt/τ))` with `τ = 220 ms`.
  Exponential approach has no start/stop discontinuity, is frame-rate
  independent (uses real `dt`), and composes: if a word's target changes
  mid-fade (speech resumes during a stale decay) the motion bends smoothly
  instead of restarting. Linear tweens are what reads as "jerky".
- **Words are born transparent** and ease up to their target, so a new word
  *blooms in* over ~200 ms rather than popping. Cost: well under the 250 ms
  first-partial budget (UX principle 2) because the word is visible (>50%)
  within one τ.
- **Long pause**: staleness (§1.2) drains committed words after 4 s; the
  in-flight tail, if any, stays. The orb stays as the mic-truth indicator.
- **30 s of continuous speech**: overflow dominates, staleness never fires
  (words overflow long before 4 s), and the lane is a steady conveyor:
  bloom right, drift target left, ramp out. Bounded words, bounded cost.
- **Frame clock**: 60 Hz NSTimer while visible, 0 Hz while hidden. Every
  animation is a pure function of `(now, model)` — no per-frame mutation
  besides the easing — so a dropped frame degrades smoothness, never
  correctness.

## 3. Layout

- **Orb bottom-center** of the active display's `visibleFrame` (clear of
  the Dock), the placement the brief demands and `layout::
  place_bottom_center` already ships for Aqua migrants. Orb diameter 44 pt
  core, glow field ~120 pt.
- **Text in a single-line lane centered above the orb**, newest word
  right-anchored at the lane's right edge... rejected. Right-anchoring makes
  the whole line shuffle left on every word, which is exactly the jitter we
  are trying to kill. Chosen instead: **newest word anchored just right of
  lane center, older words extending left**. New words appear at a fixed
  screen position (glance target never moves), old words get pushed left
  toward the fade ramp. The eye rests in one place; motion is all leftward
  and slow.
- **Lane width 440 pt**, clamped to `screen.width − 2·margin` for small
  displays. 440 pt ≈ 8-10 English words ≈ 4-5 spoken seconds — roughly one
  phrase, matching the commit horizon's natural lag.
- **CJK**: the budget is *points, not characters*. Words come from UAX #29
  segmentation (`stream::diff::word_boundaries`), which yields per-syllable
  units for CJK; each unit is measured with `sizeWithAttributes:` and the
  same overflow rule applies. A CJK lane simply holds more, narrower units.
  No character-count constant exists anywhere in the design (the old
  `TAIL_CHARS = 44` is the anti-pattern this replaces).
- Text never wraps and never sits *on* the orb: the lane's bottom edge
  keeps an 18 pt gap above the orb's glow so glow bloom never reduces text
  contrast.

## 4. The orb, tied to the 8 states

One orb, no new states. Color comes from `theme::accent(state)`; behavior
per `docs/ux/05-settings-and-states.md`:

| State | Orb (full motion) | Orb (reduced motion) |
|---|---|---|
| `idle` | absent (invisible-by-default) | absent |
| `listening` | red-accented glow; **glow radius and brightness track shaped mic level** — the orb literally breathes with the voice, replacing the bar meter as the "is it hearing me?" surface | static red orb; a thin ring thickens with level (position change, not pulse) |
| `transcribing` | amber; level input gone, so a slow constant-rate shimmer (1.2 s period) says "machine working, not hung" | static amber, slightly brighter core |
| `injecting` | absent (~13-47 ms per M0) | absent |
| `error` | steady red, no animation — motion celebrates or soothes, an error should do neither; text lane shows the situation → action line | same |
| `no-permission` | steady gray | same |
| `model-loading` | amber slow pulse (the state table's "pulsing" glyph) | static amber at half opacity |
| `degraded-offline` | absent | absent |

The orb replaces both the state dot and the level meter of the old card:
one object carries state (color), liveness (motion), and level (size). The
lane carries only words.

## 5. Reduced motion

`NSWorkspace.accessibilityDisplayShouldReduceMotion`, checked at frame time
(cheap, and reacts to the setting changing mid-session). This tool *is*
assistive technology (docs/ux/06): the reduced variant is a design, not an
off-switch.

- No pulse, no shimmer, no level-driven glow swell. Level is shown by ring
  thickness (a static-per-frame quantity, not an oscillation).
- Word opacity becomes **3 discrete tiers** (visible / half / gone) with
  instant steps instead of continuous easing — the rolling window still
  rolls (information preserved), it just doesn't *flow*.
- Stale decay becomes a single step to gone at `STALE_AFTER + STALE_FADE`.

## 6. Prototype notes (what is faked, and why drawRect)

- The brief suggested `CALayer`/`CAGradientLayer`. Those types live in
  `objc2-quartz-core`, which is not a dependency of the overlay crate, and
  this task's file budget excludes `Cargo.toml`. The prototype therefore
  renders the glow in the crate's existing immediate-mode `drawRect:`
  pipeline: 20 concentric circles with a Gaussian alpha falloff, which is
  visually a radial gradient at this size and costs nothing measurable at
  60 Hz over a ~500×190 pt surface. A production pass can move to
  `CAGradientLayer(type: .radial)` to get the compositor to do this for
  free; the *model* (this document) is renderer-independent.
- The transcript feed is a scripted `(word, commit-at)` timeline standing
  in for `CommitHorizon::update`. The mic level is a synthetic speech
  envelope. Both arrive through the same two entry points a real host
  would use.
- Focus/click-through/Spaces properties are NOT faked: the prototype uses
  the same `NonactivatingPanel` + `canBecomeKeyWindow → false` +
  `orderFrontRegardless` + `ignoresMouseEvents` + all-Spaces collection
  behavior as `macos.rs`, verified by typing into another app while it ran
  (see commit message for the observation log).
