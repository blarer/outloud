//! The overlay's visual language as pure data.
//!
//! Every colour, radius, size, opacity and animation duration the overlay
//! draws with lives here as plain numbers. Nothing in this module touches
//! AppKit, Win32, or any windowing type, for two reasons:
//!
//! 1. **The headless gate.** `cargo check -p overlay --no-default-features`
//!    must keep working, so the theme has to compile on a machine with no
//!    GUI stack at all. Platform backends convert these values into
//!    `NSColor` / GDI brushes at the call site; the values themselves are
//!    just `f64`.
//! 2. **Visual decisions become reviewable.** When the palette is smeared
//!    through `drawRect:` as literal `0.96, 0.26, 0.21` triples, nobody can
//!    diff a design change. Here a colour has a name, a hex comment, and a
//!    reason, so "why is listening blue?" is answerable in code review
//!    instead of in a design meeting.
//!
//! ## Where the values come from
//!
//! The target is deliberate visual similarity to Aqua Voice, measured from
//! their own shipped assets rather than from memory:
//!
//! * The palette names and hexes are the design tokens published in
//!   withaqua.com's stylesheet (`--bg-ink`, `.bg-cobalt`, `.bg-aqua`, …).
//! * Aqua's own FAQ calls their overlay "the black floating bar at the
//!   bottom", which is why the card is a dark, near-opaque surface rather
//!   than a light one.
//! * Their "ink" is `#292C3D`: a blue-tinted charcoal, not neutral black.
//!   Copying it exactly is most of why a screenshot reads as "that app".
//!
//! Full derivation, side-by-side screenshots, and the deltas this module
//! closes: `docs/ux/visual-parity.md`.
//!
//! ## Where we deliberately differ
//!
//! Aqua pins its bar to the bottom of the screen. We anchor near the caret
//! (`docs/ux/02-core-interaction.md`), because feedback outside the user's
//! locus of attention is feedback they have to go looking for. The *look* is
//! borrowed; the *placement* is not.

/// A colour in sRGB with straight (non-premultiplied) alpha, components in
/// `0.0..=1.0`.
///
/// sRGB specifically, not "device RGB": AppKit's `deviceRGB` follows the
/// display profile, so the same triple renders differently on a P3 laptop
/// and an sRGB external monitor. Naming the space here is what makes the
/// hex values in the doc comments true on both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    /// Build a colour from 8-bit sRGB components, the way a design token is
    /// written. Keeping the source form as hex means the constants below can
    /// be checked against the reference stylesheet by eye.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a: 1.0,
        }
    }

    /// The same colour at a different opacity. Used for the dimmed partial
    /// tail and the pulsing loading state, so the palette stays one list of
    /// hues instead of one list per opacity.
    pub const fn alpha(self, a: f64) -> Self {
        Color { a, ..self }
    }

    /// Pack to `0xRRGGBB`. Exists so tests can assert against the published
    /// token values in the same notation the design system uses, and so the
    /// Windows backend (which wants a `COLORREF`) has one conversion point.
    pub fn hex(self) -> u32 {
        let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        (q(self.r) << 16) | (q(self.g) << 8) | q(self.b)
    }
}

/// Aqua's published brand palette, token names preserved.
///
/// Preserving *their* names (cobalt, ink, mist) rather than renaming to
/// semantic ones keeps the mapping to the reference auditable. Semantic
/// meaning is assigned below in [`accent`] and the surface constants.
pub mod palette {
    use super::Color;

    /// `#4288FF` — the primary action colour across Aqua's product and site.
    /// This is the hue a user recognises as "OutLoud is doing something".
    pub const COBALT: Color = Color::rgb(0x42, 0x88, 0xFF);
    /// `#67BEFF` — the lighter brand blue.
    pub const AQUA: Color = Color::rgb(0x67, 0xBE, 0xFF);
    /// `#219AF7` — the saturated blue used for emphasis on light surfaces.
    pub const AQUA_DEEP: Color = Color::rgb(0x21, 0x9A, 0xF7);
    /// `#A7DAFC` — the palest blue; on a dark card it reads as "waiting".
    pub const AQUA_PALE: Color = Color::rgb(0xA7, 0xDA, 0xFC);
    /// `#292C3D` — Aqua's "black". Blue-tinted charcoal; the single most
    /// identity-carrying value in the palette.
    pub const INK: Color = Color::rgb(0x29, 0x2C, 0x3D);
    /// `#3E4150` — one step up from ink, for hairlines and inset fills.
    pub const INK_SOFT: Color = Color::rgb(0x3E, 0x41, 0x50);
    /// `#F4F5F7` — light surface.
    pub const MIST: Color = Color::rgb(0xF4, 0xF5, 0xF7);
    /// `#FAFAFA` — lightest surface.
    pub const FROST: Color = Color::rgb(0xFA, 0xFA, 0xFA);

    // Status hues. Aqua publishes no error/warning tokens (their marketing
    // site has no failure states to colour), so these are ours: chosen to
    // sit next to the blues without clashing, and to stay distinguishable
    // for the most common colour-vision deficiencies, where blue-vs-amber is
    // reliable and red-vs-green is not. Every state that uses them also
    // carries text (UX principle 4), so colour is never the only signal.
    /// `#FF4A3D` — failure.
    pub const EMBER: Color = Color::rgb(0xFF, 0x4A, 0x3D);
    /// `#FFA033` — needs a human decision (permissions).
    pub const AMBER: Color = Color::rgb(0xFF, 0xA0, 0x33);
    /// `#F2F3F7` — primary text on the dark card. Not pure white: pure white
    /// on a near-black card at small sizes shimmers on subpixel-antialiased
    /// displays.
    pub const PAPER: Color = Color::rgb(0xF2, 0xF3, 0xF7);

    // The cat mascot's coat, sampled from the owner's reference photos of
    // one specific dilute calico domestic longhair rather than named from
    // memory — "grey cat" would have produced a neutral grey, and hers is
    // warm. Sampled values were averaged over the relevant fur regions in
    // the indoor (colour-neutral) reference.
    /// `#F5F1EA` — her white: the chest ruff and muzzle. Warm off-white,
    /// not PAPER, which is cool.
    pub const CAT_WHITE: Color = Color::rgb(0xF5, 0xF1, 0xEA);
    /// `#B4A99C` — the dilute grey ("blue") of her back, ears and temple
    /// patch. Warm grey-taupe: dilute black, not slate.
    pub const CAT_GREY: Color = Color::rgb(0xB4, 0xA9, 0x9C);
    /// `#E8D3BC` — the dilute cream of her forehead patch. Muted buff, the
    /// dilute of red.
    pub const CAT_CREAM: Color = Color::rgb(0xE8, 0xD3, 0xBC);
    /// `#C89093` — her nose and inner ears: dusty pink.
    pub const CAT_PINK: Color = Color::rgb(0xC8, 0x90, 0x93);
    /// `#9BA86A` — her eyes: moss green with a yellow core.
    pub const CAT_MOSS: Color = Color::rgb(0x9B, 0xA8, 0x6A);
}

use crate::state::OverlayState;
use palette::*;

/// The overlay card's fill. Alpha under 1.0 so a hint of the content behind
/// shows through and the card reads as an overlay rather than a window.
pub const CARD_BG: Color = INK.alpha(0.92);

/// A hairline inside the card's edge. Aqua's surfaces all carry a 1px
/// light inner ring (`ring-black/[0.05]` on light, its inverse on dark);
/// without it a dark card over dark content loses its silhouette entirely.
pub const CARD_BORDER: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.10,
};

/// Card corner radius in points.
///
/// 18 rather than a full pill (height/2 = 32): Aqua's *controls* are pills,
/// but their bar is a rounded card, and a full pill wastes horizontal room
/// at both ends of a text-bearing surface.
pub const CARD_RADIUS: f64 = 18.0;

/// Card border width in points. Hairline on 1x, still hairline on Retina.
pub const CARD_BORDER_WIDTH: f64 = 1.0;

/// Padding from the card edge to any content, in points.
pub const CARD_PADDING: f64 = 16.0;

/// Primary label size/weight. Semibold because the label is one or two
/// words read at a glance from the corner of the eye, not body copy.
pub const LABEL_FONT_SIZE: f64 = 13.0;
/// AppKit `NSFontWeightSemibold` numeric value, passed through as data so
/// this module needs no AppKit import.
pub const LABEL_FONT_WEIGHT: f64 = 0.3;
/// Primary label colour.
pub const LABEL_COLOR: Color = PAPER;

/// Partial-transcription tail: dimmer and lighter-weight than the label,
/// because it is provisional text. The visual hierarchy *is* the promise
/// that this text is not committed yet.
pub const TAIL_FONT_SIZE: f64 = 12.5;
/// `NSFontWeightRegular`.
pub const TAIL_FONT_WEIGHT: f64 = 0.0;
pub const TAIL_COLOR: Color = Color { a: 0.62, ..PAPER };

/// Whether the partial tail is drawn in a monospaced font.
///
/// `false`, deliberately, and this is a change from the first
/// implementation: Aqua shows dictated prose as prose. Monospace makes
/// partials look like console output, which is exactly the "developer tool"
/// register we do not want for text destined for an email.
pub const TAIL_MONOSPACE: bool = false;

/// State dot diameter in points.
pub const DOT_SIZE: f64 = 8.0;

/// Level-meter geometry. A bar is a rounded 3pt column; `MIN_HEIGHT` is
/// nonzero so bars never vanish — a meter that empties to nothing is
/// indistinguishable from a meter that has crashed, and the meter's whole
/// job is proving the microphone is live (UX principle 3).
pub const METER_BAR_WIDTH: f64 = 3.0;
pub const METER_BAR_GAP: f64 = 3.0;
pub const METER_MAX_HEIGHT: f64 = 14.0;
pub const METER_MIN_HEIGHT: f64 = 3.0;
/// Corner radius of a meter bar: half its width, i.e. fully rounded caps.
pub const METER_BAR_RADIUS: f64 = METER_BAR_WIDTH / 2.0;

/// Fraction of a bar's height that comes from the shaped level, with the
/// remainder from the per-bar phase wobble. Some wobble is what makes the
/// meter read as a live waveform instead of a progress bar.
pub const METER_WOBBLE: f64 = 0.45;

/// How fast the loading state's opacity breathes, in full cycles per second.
/// ~0.6 Hz: slow enough to read as "working", fast enough not to look stuck.
pub const PULSE_HZ: f64 = 0.6;
/// The opacity range the pulse sweeps.
pub const PULSE_MIN_ALPHA: f64 = 0.35;
pub const PULSE_MAX_ALPHA: f64 = 1.0;

/// Fade-in duration for the card, in milliseconds.
///
/// 90ms is a hard ceiling, not a taste choice: UX principle 2 requires the
/// first visible feedback within ~100ms of key-down, and an animation that
/// has not finished by then *is* the latency the user feels. Aqua's bar
/// appears effectively instantly; anything slower reads as a laggy hotkey.
pub const FADE_IN_MS: u64 = 90;

/// Fade-out duration in milliseconds. Longer than fade-in on purpose:
/// disappearance is not on the critical path, and an abrupt vanish makes
/// the user wonder whether it crashed or completed.
pub const FADE_OUT_MS: u64 = 140;

/// Per-state accent colour: the dot, the meter bars, and the static strip.
///
/// The blues carry the "working normally" arc (listening → transcribing →
/// loading) so a state change inside the happy path never flashes a warning
/// hue at someone who is mid-sentence. Amber and red are reserved for the
/// two states that actually want a human. This is the single biggest
/// departure from our first pass, which painted `Listening` recording-red.
pub fn accent(state: OverlayState) -> Color {
    match state {
        // The mic is hot: the brand's primary blue, at full strength.
        OverlayState::Listening => COBALT,
        // Still the machine's turn, one step lighter, so progress reads as
        // progress rather than as a new condition.
        OverlayState::Transcribing => AQUA,
        // Palest blue; combined with the pulse it reads as "not ready yet".
        OverlayState::ModelLoading => AQUA_PALE,
        OverlayState::Error => EMBER,
        OverlayState::NoPermission => AMBER,
        // Invisible states still answer, so surfaces that *do* render in
        // them (tray glyph, terminal indicator) have a defined colour and no
        // caller needs a fallback branch.
        OverlayState::Idle | OverlayState::Injecting | OverlayState::DegradedOffline => INK_SOFT,
    }
}

/// The accent with the loading pulse applied, given seconds since the state
/// was entered.
///
/// Time-based rather than frame-count-based: a frame counter makes the pulse
/// speed depend on redraw rate, so the same state breathes at different
/// speeds on a 60Hz and a 120Hz display.
pub fn pulsed_accent(state: OverlayState, elapsed_secs: f64) -> Color {
    let base = accent(state);
    if state != OverlayState::ModelLoading {
        return base;
    }
    let phase = (elapsed_secs * PULSE_HZ * std::f64::consts::TAU).sin() * 0.5 + 0.5;
    base.alpha(PULSE_MIN_ALPHA + (PULSE_MAX_ALPHA - PULSE_MIN_ALPHA) * phase)
}

/// Height of a meter bar in points for a shaped level and a wobble phase in
/// `0..=1`.
///
/// Kept here rather than in the backend so the meter's *shape* is testable
/// without a display, and so the Windows backend cannot drift from macOS.
pub fn meter_bar_height(shaped_level: f64, phase: f64) -> f64 {
    let scale = (1.0 - METER_WOBBLE) + METER_WOBBLE * phase.clamp(0.0, 1.0);
    let h = METER_MIN_HEIGHT
        + (METER_MAX_HEIGHT - METER_MIN_HEIGHT) * shaped_level.clamp(0.0, 1.0) * scale;
    h.clamp(METER_MIN_HEIGHT, METER_MAX_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance, the cheap sRGB approximation (no gamma
    /// linearisation). Good enough for the contrast tripwires below, and
    /// `const` so those checks can run at compile time.
    const fn luminance(c: Color) -> f64 {
        0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
    }

    #[test]
    fn palette_matches_the_published_design_tokens() {
        // These hexes are transcribed from withaqua.com's stylesheet. If a
        // refactor perturbs one, this test is the tripwire — the whole point
        // of the module is that these exact values are the reference.
        assert_eq!(COBALT.hex(), 0x4288FF);
        assert_eq!(AQUA.hex(), 0x67BEFF);
        assert_eq!(AQUA_DEEP.hex(), 0x219AF7);
        assert_eq!(AQUA_PALE.hex(), 0xA7DAFC);
        assert_eq!(INK.hex(), 0x292C3D);
        assert_eq!(INK_SOFT.hex(), 0x3E4150);
        assert_eq!(MIST.hex(), 0xF4F5F7);
        assert_eq!(FROST.hex(), 0xFAFAFA);
    }

    #[test]
    fn card_is_translucent_ink() {
        assert_eq!(CARD_BG.hex(), 0x292C3D, "the card must be OutLoud's ink");
        // A const block, so a bad edit fails to compile rather than failing
        // a test run someone might not have started.
        const {
            assert!(
                CARD_BG.a > 0.85 && CARD_BG.a < 1.0,
                "opaque enough to read over any content, translucent enough to read as an overlay"
            );
        }
    }

    #[test]
    fn alpha_preserves_the_hue() {
        let c = COBALT.alpha(0.3);
        assert_eq!(c.hex(), COBALT.hex());
        assert_eq!(c.a, 0.3);
    }

    #[test]
    fn every_state_has_an_accent() {
        // Including the invisible ones: non-overlay surfaces render them.
        for s in OverlayState::ALL {
            let c = accent(s);
            assert!(c.a > 0.0, "{s} has a fully transparent accent");
        }
    }

    #[test]
    fn the_working_states_are_all_brand_blue() {
        // The regression this guards: painting `Listening` recording-red,
        // which reads as a screen recorder rather than as dictation, and
        // makes a routine state look like an alert.
        for s in [
            OverlayState::Listening,
            OverlayState::Transcribing,
            OverlayState::ModelLoading,
        ] {
            let c = accent(s);
            assert!(
                c.b > c.r && c.b > c.g,
                "{s} must be blue-dominant, got {c:?}"
            );
        }
    }

    #[test]
    fn attention_states_are_not_blue() {
        // Conversely, the two states that want a human must not blend into
        // the happy-path blues.
        for s in [OverlayState::Error, OverlayState::NoPermission] {
            let c = accent(s);
            assert!(c.r > c.b, "{s} must read warm, got {c:?}");
        }
    }

    #[test]
    fn only_loading_pulses() {
        // A pulse means "wait"; if any other state breathed, the signal
        // would stop meaning anything.
        for s in OverlayState::ALL {
            // Quarter period, not half: the sine returns to its starting
            // value at the half-way point, so a half-period sample would
            // compare the pulse against itself and pass vacuously.
            let a = pulsed_accent(s, 0.0);
            let b = pulsed_accent(s, 1.0 / (4.0 * PULSE_HZ));
            if s == OverlayState::ModelLoading {
                assert_ne!(a.a, b.a, "loading must visibly breathe");
            } else {
                assert_eq!(a.a, b.a, "{s} must not animate");
            }
        }
    }

    #[test]
    fn pulse_stays_inside_its_declared_range() {
        // Sample a full period densely: an out-of-range alpha would either
        // clip to invisible or flash brighter than the label.
        for i in 0..400 {
            let t = i as f64 / 100.0;
            let a = pulsed_accent(OverlayState::ModelLoading, t).a;
            assert!(
                (PULSE_MIN_ALPHA - 1e-9..=PULSE_MAX_ALPHA + 1e-9).contains(&a),
                "alpha {a} out of range at t={t}"
            );
        }
    }

    #[test]
    fn meter_bars_never_vanish_and_never_overflow() {
        for level in [0.0, 0.01, 0.5, 1.0] {
            for phase in [0.0, 0.5, 1.0] {
                let h = meter_bar_height(level, phase);
                assert!(
                    (METER_MIN_HEIGHT..=METER_MAX_HEIGHT).contains(&h),
                    "level={level} phase={phase} gave {h}"
                );
            }
        }
        // Out-of-contract inputs must clamp rather than produce a bar that
        // paints outside the card.
        assert_eq!(meter_bar_height(-5.0, 0.5), METER_MIN_HEIGHT);
        assert_eq!(meter_bar_height(5.0, 1.0), METER_MAX_HEIGHT);
    }

    #[test]
    fn meter_height_is_monotone_in_level() {
        let mut prev = 0.0;
        for i in 0..=100 {
            let h = meter_bar_height(i as f64 / 100.0, 1.0);
            assert!(h >= prev, "meter must not shrink as the voice gets louder");
            prev = h;
        }
    }

    #[test]
    fn first_feedback_animation_fits_the_latency_budget() {
        // UX principle 2: visible feedback within ~100ms of key-down. An
        // animation slower than that budget *is* perceived latency.
        const {
            assert!(FADE_IN_MS <= 100, "fade-in eats the 100ms feedback budget");
            assert!(
                FADE_OUT_MS >= FADE_IN_MS,
                "appearing must be snappier than disappearing"
            );
        }
    }

    #[test]
    fn text_contrasts_with_the_card() {
        // Cheap luminance check, not a full WCAG computation: the point is
        // to catch someone darkening the label or lightening the card into
        // an unreadable pair, which review by screenshot routinely misses.
        const {
            assert!(
                luminance(LABEL_COLOR) - luminance(CARD_BG) > 0.6,
                "label/card contrast collapsed"
            );
            // The tail is dimmer on purpose, but must stay legible.
            assert!(TAIL_COLOR.a >= 0.55, "provisional text must stay readable");
            assert!(
                TAIL_COLOR.a < 1.0,
                "provisional text must be visibly weaker than committed text"
            );
        }
    }

    #[test]
    fn type_scale_keeps_the_label_dominant() {
        const {
            assert!(
                LABEL_FONT_SIZE > TAIL_FONT_SIZE,
                "the state label is the thing read at a glance"
            );
            assert!(
                LABEL_FONT_WEIGHT > TAIL_FONT_WEIGHT,
                "weight, not just size, carries the hierarchy"
            );
            assert!(
                !TAIL_MONOSPACE,
                "dictated prose is prose, not console output"
            );
        }
    }
}
