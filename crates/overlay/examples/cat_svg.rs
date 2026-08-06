//! Headless render of the cat mascot to SVG, one file per interesting pose.
//!
//! Exists because the mascot's geometry is pure and therefore viewable
//! without a display: iterating on proportions against the reference photos
//! is a save-and-refresh loop here, versus a build-run-screenshot loop
//! through the real panel. The SVG painter mirrors `macos.rs::draw_cat`'s
//! order and palette so what this shows is what the panel draws (minus the
//! aura, shadows and gradients, which are AppKit's).
//!
//! Run: `cargo run -p overlay --example cat_svg` → `/tmp/cat_*.svg`.

use overlay::cat::{posed_geometry, CatPose};
use overlay::layout::Point;
use overlay::state::OverlayState;
use overlay::theme::{palette, Color};

fn hex(c: Color) -> String {
    format!("#{:06X}", c.hex())
}

fn poly(pts: &[Point], fill: &str, opacity: f64) -> String {
    let pts: Vec<String> = pts
        .iter()
        .map(|p| format!("{:.2},{:.2}", p.x * 420.0, p.y * 420.0))
        .collect();
    format!(
        "<polygon points=\"{}\" fill=\"{}\" fill-opacity=\"{opacity}\"/>\n",
        pts.join(" "),
        fill
    )
}

fn render(pose: &CatPose, state: OverlayState, name: &str) {
    let geo = posed_geometry(pose);
    let mut s = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"420\" height=\"420\" \
         viewBox=\"0 0 420 420\"><rect width=\"420\" height=\"420\" fill=\"#333\"/>\n",
    );
    let white = hex(palette::CAT_WHITE);
    let grey = hex(palette::CAT_GREY);
    let cream = hex(palette::CAT_CREAM);
    let pink = hex(palette::CAT_PINK);
    let moss = hex(palette::CAT_MOSS);
    let ink = hex(palette::INK);

    s += &poly(&geo.ruff, &white, 0.9);
    for (ear, inner) in geo.ears.iter().zip(&geo.ear_inners) {
        s += &poly(ear, &grey, 1.0);
        s += &poly(inner, &pink, 1.0);
    }
    s += &poly(&geo.head, &white, 1.0);
    s += &poly(&geo.patch_cream, &cream, 0.9);
    s += &poly(&geo.patch_grey, &grey, 1.0);
    s += &poly(&geo.smudge, &grey, 1.0);
    s += &poly(&geo.mouth, &ink, 0.94);
    for fang in &geo.fangs {
        s += &poly(fang, &white, 1.0);
    }
    s += &poly(&geo.nose, &pink, 1.0);
    for w in &geo.whiskers {
        s += &poly(w, &white, 0.55);
    }
    for eye in &geo.eyes {
        s += &poly(eye, &moss, 1.0);
    }
    for p in &geo.pupils {
        s += &poly(p, &ink, 1.0);
    }
    if let Some(glint) = overlay::theme::cat_glint(state) {
        for g in &geo.glints {
            s += &poly(g, &hex(glint), 0.9);
        }
    }
    s += "</svg>";
    let path = format!("/tmp/cat_{name}.svg");
    std::fs::write(&path, s).expect("write svg");
    println!("{path}");
}

fn main() {
    let rest = CatPose::at_rest();
    render(&rest, OverlayState::Idle, "idle");
    render(
        &CatPose {
            mouth_open: 0.9,
            pupil: 1.0,
            ear_perk: 1.0,
            eye_glow: 1.0,
            ..rest
        },
        OverlayState::Listening,
        "listening_loud",
    );
    render(
        &CatPose {
            pupil: 0.35,
            ear_perk: 0.85,
            ..rest
        },
        OverlayState::Transcribing,
        "transcribing",
    );
    render(
        &CatPose {
            ear_perk: 0.0,
            pupil: 1.0,
            ..rest
        },
        OverlayState::Error,
        "error",
    );
    render(
        &CatPose {
            ear_perk: 0.22,
            pupil: 0.8,
            ..rest
        },
        OverlayState::NoPermission,
        "no_permission",
    );
    render(
        &CatPose {
            ear_perk: 0.55,
            eye_glow: 0.5,
            ..rest
        },
        OverlayState::ModelLoading,
        "model_loading",
    );
    render(
        &CatPose {
            eye_open: 0.15,
            ..rest
        },
        OverlayState::Transcribing,
        "slow_blink",
    );
    render_glyph();
}

/// Render the menu-bar silhouette at glyph proportions, dark-bar white on
/// a dark swatch, so the 15pt legibility call is checkable by eye.
fn render_glyph() {
    let polys = overlay::cat::glyph_silhouette();
    let mut s = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"420\" height=\"420\" \
         viewBox=\"0 0 420 420\"><rect width=\"420\" height=\"420\" fill=\"#222\"/>\n",
    );
    for p in &polys {
        s += &poly(p, "#FFFFFF", 1.0);
    }
    // A 15pt-equivalent thumbnail in the corner: the size it actually ships.
    s += "<g transform=\"translate(370,10) scale(0.0714)\">";
    s += "<rect width=\"560\" height=\"560\" fill=\"#222\"/>";
    for p in &polys {
        s += &poly(p, "#FFFFFF", 1.0);
    }
    s += "</g></svg>";
    std::fs::write("/tmp/cat_glyph.svg", s).expect("write svg");
    println!("/tmp/cat_glyph.svg");
}
