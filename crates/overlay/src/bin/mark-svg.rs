//! Emit the brand mark as an SVG, from the same geometry the menu bar draws.
//!
//! The logo and the menu-bar glyph were separate hand-authored shapes: an SVG
//! written by hand, and a Rust path in `mark.rs`. That is two sources for one
//! identity, and they drifted the moment the glyph became a skull -- the app
//! icon, the Dock icon and the README stayed a megaphone.
//!
//! Generating one from the other makes drift impossible: `mark::unit_mark()`
//! is the single definition, and this scales it into the 128-box the icon
//! pipeline expects.
//!
//! Usage: cargo run -p overlay --bin mark-svg > docs/assets/logo.svg

fn main() {
    const BOX: f64 = 128.0;
    // The mark sits inside the rounded field with a margin, the way the
    // megaphone did: a glyph touching the corner radius looks cramped at
    // Dock size.
    const INSET: f64 = 16.0;
    let span = BOX - INSET * 2.0;

    let m = overlay::mark::unit_mark();
    let place = |p: &overlay::layout::Point| (INSET + p.x * span, INSET + p.y * span);

    let poly = |pts: &[overlay::layout::Point]| -> String {
        let mut d = String::new();
        for (i, p) in pts.iter().enumerate() {
            let (x, y) = place(p);
            d.push_str(&format!("{}{x:.1} {y:.1}", if i == 0 { "M" } else { "L" }));
            d.push(' ');
        }
        d.push('Z');
        d
    };

    // One path, outline plus sockets, filled even-odd so the sockets are
    // holes. Same rule as the AppKit and Win32 backends, for the same
    // reason: a socket filled with a colour stops being a hole the moment
    // the background is not what you assumed.
    let mut d = poly(&m.outline);
    for hole in &m.holes {
        d.push(' ');
        d.push_str(&poly(hole));
    }

    print!(
        r##"<!--
  OutLoud mark: the skull, which is also the dictation overlay's mascot and
  the menu-bar glyph.

  GENERATED. Do not hand-edit: run
      cargo run -p overlay --bin mark-svg > docs/assets/logo.svg
  The shape comes from `crates/overlay/src/mark.rs`, which is the single
  definition the menu bar, the Windows tray and this file all share. It used
  to be hand-authored separately, and drifted the moment the glyph changed:
  the menu bar became a skull while the app icon, Dock icon and README
  stayed a megaphone.

  WHY an SVG rather than a PNG: it is text, so it diffs in review, scales to
  any README width or app icon size without a second asset, and carries no
  binary weight in git. The repo already learned that lesson the expensive
  way when a rename committed two 98KB binaries nobody noticed.

  WHY currentColor is NOT used: GitHub renders README images in both light
  and dark themes, and an unresolved currentColor renders black-on-black in
  dark mode. This project has already shipped that exact bug once, so every
  colour here is explicit.
-->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128"
     role="img" aria-label="OutLoud: a skull">
  <defs>
    <linearGradient id="body" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#e2e8f0"/>
      <stop offset="100%" stop-color="#94a3b8"/>
    </linearGradient>
  </defs>

  <!-- Rounded field. Sized so the mark still reads at 16px in a menu bar. -->
  <rect x="4" y="4" width="120" height="120" rx="28" fill="#0f172a"/>

  <!-- Skull: outline plus eye sockets, one path, even-odd so the sockets
       are cut rather than painted. -->
  <path d="{d}"
        fill="url(#body)" fill-rule="evenodd"
        stroke="#f8fafc" stroke-width="2" stroke-linejoin="round"/>
</svg>
"##
    );
}
