//! Pixel maths for layered-surface compositing.
//!
//! Lives in the platform-neutral half of the crate, alongside [`layout`] and
//! [`state`], for exactly the same reason those do: it is pure arithmetic
//! whose failure modes are invisible to the compiler and visible only on a
//! screen nobody on this project currently has. Keeping it here means the
//! properties are asserted on every platform's CI instead of by eye on
//! Windows.
//!
//! [`layout`]: crate::layout
//! [`state`]: crate::state

/// The overlay panel's uniform opacity. ~90%: solid enough to read against
/// a busy desktop, translucent enough to read as an overlay rather than a
/// window that stole the screen.
pub const PANEL_ALPHA: u32 = 230;

/// Convert one straight (unpremultiplied) `0xAARRGGBB` pixel into the
/// premultiplied form Windows' `UpdateLayeredWindow` requires when the
/// blend uses `AC_SRC_ALPHA`.
///
/// Premultiplied means every colour channel is already scaled by alpha, so
/// the compositor blends with a plain add. This is a classic layered-window
/// bug site with two failure shapes, neither of which the compiler or a
/// clean build can catch:
///
/// * **Forgetting to scale the channels** leaves light pixels glowing and
///   every edge fringed against whatever is behind the overlay.
/// * **Leaving alpha at zero**, which is what GDI writes for every pixel it
///   draws, composites a fully transparent surface: the overlay simply
///   never appears, and no API reports an error.
///
/// The invariant that defines a valid premultiplied pixel, and the one the
/// tests pin hardest: no colour channel may exceed alpha.
pub fn premultiply(straight: u32, alpha: u32) -> u32 {
    debug_assert!(alpha <= 255, "alpha is an 8-bit channel");
    let r = (straight >> 16) & 0xFF;
    let g = (straight >> 8) & 0xFF;
    let b = straight & 0xFF;
    (alpha << 24) | ((r * alpha / 255) << 16) | ((g * alpha / 255) << 8) | (b * alpha / 255)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(px: u32) -> (u32, u32, u32, u32) {
        (
            (px >> 24) & 0xFF,
            (px >> 16) & 0xFF,
            (px >> 8) & 0xFF,
            px & 0xFF,
        )
    }

    #[test]
    fn every_pixel_gets_the_panel_alpha() {
        // GDI leaves alpha at 0. If the fixup misses it, the overlay is
        // composited fully transparent: never visible, no error anywhere.
        let (a, ..) = channels(premultiply(0x0012_3456, PANEL_ALPHA));
        assert_eq!(a, PANEL_ALPHA);
    }

    #[test]
    fn colour_channels_are_scaled_by_alpha() {
        // The definition of premultiplied. Unscaled channels make light
        // pixels glow and edges fringe against the desktop behind them.
        let (_, r, g, b) = channels(premultiply(0x00FF_FF80, PANEL_ALPHA));
        assert_eq!(r, 0xFF * PANEL_ALPHA / 255);
        assert_eq!(g, 0xFF * PANEL_ALPHA / 255);
        assert_eq!(b, 0x80 * PANEL_ALPHA / 255);
    }

    #[test]
    fn no_channel_may_ever_exceed_alpha() {
        // The invariant that defines a VALID premultiplied pixel. Violating
        // it is undefined for the compositor and renders as bright fringing.
        for v in [
            0x0000_0000,
            0x00FF_FFFF,
            0x0080_4020,
            0x0001_0203,
            0x00FF_0000,
            0x0000_FF00,
            0x0000_00FF,
        ] {
            let (a, r, g, b) = channels(premultiply(v, PANEL_ALPHA));
            assert!(
                r <= a && g <= a && b <= a,
                "channel exceeded alpha for {v:#010x}"
            );
        }
    }

    #[test]
    fn black_stays_black_and_white_reaches_full_alpha() {
        assert_eq!(
            channels(premultiply(0x0000_0000, PANEL_ALPHA)),
            (PANEL_ALPHA, 0, 0, 0)
        );
        let (a, r, g, b) = channels(premultiply(0x00FF_FFFF, PANEL_ALPHA));
        assert_eq!(
            (r, g, b),
            (a, a, a),
            "opaque white premultiplies to alpha in every channel"
        );
    }

    #[test]
    fn fully_transparent_alpha_zeroes_everything() {
        // Degenerate but worth pinning: alpha 0 must produce a wholly zero
        // pixel, not stale colour a compositor might sample.
        assert_eq!(premultiply(0x00FF_FFFF, 0), 0);
    }

    #[test]
    fn opaque_alpha_leaves_colour_untouched() {
        // With alpha 255 premultiplication is the identity on colour, so a
        // fully opaque panel must not be dimmed by this step.
        let (a, r, g, b) = channels(premultiply(0x0012_3456, 255));
        assert_eq!((a, r, g, b), (255, 0x12, 0x34, 0x56));
    }

    #[test]
    fn is_monotonic_in_alpha() {
        // Raising opacity must never darken a channel. Catches a swapped
        // multiply/divide, which passes the spot checks above by accident.
        let mut last = 0;
        for alpha in [0, 32, 64, 128, 200, 230, 255] {
            let r = (premultiply(0x00C0_C0C0, alpha) >> 16) & 0xFF;
            assert!(r >= last, "channel went backwards at alpha {alpha}");
            last = r;
        }
    }
}
