//! Windows overlay backend: a layered, click-through, topmost,
//! **non-activating** popup window, drawing the same animated skull as
//! macOS via Direct2D.
//!
//! The extended-style quartet is the whole window-plumbing design, and
//! every flag is a correctness requirement, not a preference:
//!
//! - `WS_EX_NOACTIVATE`: showing or updating the window never takes
//!   keyboard focus. This is THE requirement: the product edits the text
//!   field the user is focused on, and an overlay that activates would
//!   destroy that field's focus and with it the edit about to happen.
//!   Show uses `SW_SHOWNOACTIVATE` for the same reason; `ShowWindow(SW_SHOW)`
//!   activates even a WS_EX_NOACTIVATE window on some paths.
//! - `WS_EX_TRANSPARENT` + `WS_EX_LAYERED`: clicks pass through to whatever
//!   is underneath. The overlay is an indicator, not a control; stealing a
//!   click would be a lesser version of stealing focus.
//! - `WS_EX_TOPMOST`: above normal windows, where a status indicator must
//!   live. (Above the *taskbar* or fullscreen-exclusive games is not
//!   promised; borderless-fullscreen apps are covered.)
//! - `WS_EX_TOOLWINDOW`: keeps the overlay out of Alt-Tab and the taskbar,
//!   the Windows equivalent of the macOS panel's non-participating
//!   collection behavior.
//!
//! None of this changes when the renderer changes (see below): Direct2D
//! drawing happens entirely inside `paint()`'s existing CPU-side DC, with
//! no interaction with window activation, message handling, or input.
//!
//! ## DPI
//!
//! The process declares per-monitor-v2 DPI awareness before creating the
//! window (`SetProcessDpiAwarenessContext`). Without it, Windows lies about
//! coordinates on any monitor whose scale is not 100%: `GetCursorPos` and
//! monitor rects come back virtualized, and the overlay lands offset from
//! the caret by exactly the scale factor. This is called at overlay
//! construction rather than in a manifest because the crate cannot dictate
//! the host binary's manifest; failure (another component already set a
//! conflicting awareness) is non-fatal and logged, since a mispositioned
//! overlay beats no overlay. The render target's own DPI (`SetDpi`,
//! see `new()`) is a separate, *not yet handled*, scaled-monitor concern —
//! see `docs/plans/windows-overlay.md` §5.
//!
//! ## Rendering
//!
//! Direct2D (`ID2D1DCRenderTarget`) bound to the same GDI memory DC
//! `UpdateLayeredWindow` already required, drawing the platform-neutral
//! skull model from [`crate::skull`] the same way `macos.rs` does with
//! AppKit — same geometry, same theme, same animator, a different
//! rendering API translating it to pixels. See
//! `docs/plans/windows-overlay.md` for the full design and staging; this
//! file implements stages 0-2 (flat skull, then depth/lighting). The eye
//! glow aura and text lane (stage 3) are not wired in yet: the panel
//! currently shows the skull only, no partial-text tail.
//!
//! ### The one thing this file cannot verify
//!
//! The DC render target's pixel format below declares
//! `D2D1_ALPHA_MODE_PREMULTIPLIED`, which per Direct2D's documented
//! contract means Direct2D itself writes already-premultiplied pixels into
//! the bound DC — exactly the format `UpdateLayeredWindow`'s
//! `AC_SRC_ALPHA` blend wants. That is why the old manual `premultiply()`
//! fixup loop (still in `pixel.rs`, still tested, just unused here) is
//! **not** called in this file: applying it on top of already-premultiplied
//! Direct2D output would double-scale every channel and corrupt the image.
//! This is the single biggest unverified interop point in the whole plan
//! (`docs/plans/windows-overlay.md` §2.1, §5): it is the documented,
//! standard way `ID2D1DCRenderTarget` is used, but no Windows hardware
//! confirmed it in this session. If the overlay renders as fully
//! transparent, solid black, or fringed on real hardware, this premultiply
//! assumption is the first thing to check.

use crate::layout::{self, place, Anchor, Point, Rect, Size};
use crate::skull::{self, SkullAnimator, SkullPose};
use crate::state::OverlayState;
use crate::theme;
use crate::{Overlay, OverlayFrame};

use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_FILL_MODE_WINDING, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1PathGeometry, D2D1_ELLIPSE,
    D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics, RegisterClassW,
    ShowWindow, UpdateLayeredWindow, HWND_TOPMOST, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows_numerics::{Matrix3x2, Vector2};

/// Panel size in points (treated as device pixels; see the DPI note in the
/// module doc — this crate does not yet scale the render target for
/// non-100% monitors). Sized to comfortably hold the skull box, the
/// stage-2 shadow's offset, and the stage-3 glow aura's reach, with no
/// text lane yet.
const PANEL_W: i32 = 160;
const PANEL_H: i32 = 160;

/// The skull's bounding box inside the panel, in panel points. Same
/// `SKULL_SIZE=42` macOS uses (`macos.rs`'s `SKULL_SIZE` constant), per the
/// plan's recommendation to adopt it directly rather than guess a Windows
/// size independently (`docs/plans/windows-overlay.md` Stage 1).
const SKULL_SIZE: f64 = 42.0;
const SKULL_X: f64 = (PANEL_W as f64 - SKULL_SIZE) / 2.0;
const SKULL_Y: f64 = (PANEL_H as f64 - SKULL_SIZE) / 2.0;

/// The glow aura's centre (panel centre, matching the skull box's own
/// centre) and base radius, mirroring `macos.rs`'s `GLOW_CX`/`GLOW_CY`/
/// `GLOW_R` — same footprint policy, different rendering technique (one
/// native radial-gradient fill here vs. macOS's 20-ring fake, plan §2.5:
/// Direct2D has a real radial gradient brush, so this is simpler code, not
/// a compromise). Unlike macOS's ring loop, a single radial brush needs no
/// separate inner-floor radius (`ORB_R` there) — the gradient stops
/// themselves define where the fade starts.
const GLOW_CX: f64 = PANEL_W as f64 / 2.0;
const GLOW_CY: f64 = PANEL_H as f64 / 2.0;
const GLOW_R: f64 = 58.0;

/// Where the light comes from, mirroring `macos.rs`'s `LIGHT_ELEVATION`
/// doc: depth is every highlight and shadow agreeing about one light.
/// Direct2D's linear gradient brush takes explicit start/end points rather
/// than macOS's angle-from-horizontal, so this file approximates the same
/// "lit top, shaded bottom" cue with a plain vertical axis rather than
/// replicating the elevation angle's slight tilt — a simplification of an
/// already-cheap depth cue, not a new design decision.
const SHADOW_OFFSET_Y: f32 = 2.0;

/// Default window procedure only: the window takes no input (transparent,
/// no-activate) and paints exclusively via UpdateLayeredWindow, so there is
/// nothing to handle.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The single `theme::Color` → `D2D1_COLOR_F` conversion point, the
/// Direct2D analog of `macos.rs`'s `ns_color()`. `theme::Color` is already
/// straight (non-premultiplied) sRGB alpha (see `theme.rs`'s doc comment),
/// which is exactly what Direct2D brush colors want regardless of the
/// render target's own premultiplied pixel format — Direct2D premultiplies
/// on write, callers do not. `alpha_scale` composes an additional opacity
/// (the pose's fade, mostly) without needing a second color constant per
/// state, mirroring `ns_color`'s own second parameter.
fn d2d1_color(c: theme::Color, alpha_scale: f64) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r as f32,
        g: c.g as f32,
        b: c.b as f32,
        a: (c.a * alpha_scale) as f32,
    }
}

/// Map one unit-square skull point into panel-relative device points,
/// mirroring `macos.rs`'s `poly_path` mapping into its `SKULL_*` box.
fn map_point(p: &Point) -> Vector2 {
    Vector2 {
        X: (SKULL_X + p.x * SKULL_SIZE) as f32,
        Y: (SKULL_Y + p.y * SKULL_SIZE) as f32,
    }
}

/// Build a closed Direct2D path geometry for one unit-square polygon, or
/// `None` for a degenerate (<3 point) polygon — the same "too small to be a
/// shape" guard `macos.rs::poly_path` uses.
fn poly_geometry(
    factory: &ID2D1Factory,
    poly: &[Point],
) -> windows::core::Result<Option<ID2D1PathGeometry>> {
    if poly.len() < 3 {
        return Ok(None);
    }
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.SetFillMode(D2D1_FILL_MODE_WINDING);
        sink.BeginFigure(map_point(&poly[0]), D2D1_FIGURE_BEGIN_FILLED);
        for p in &poly[1..] {
            sink.AddLine(map_point(p));
        }
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(Some(geometry))
    }
}

/// The vertical gradient axis for `fill_poly_lit`: panel-space horizontal
/// midpoint of the polygon's bounding box, and its top/bottom device y.
fn gradient_axis(poly: &[Point]) -> (f32, f32, f32) {
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    for p in poly {
        let v = map_point(p);
        min_x = min_x.min(v.X);
        max_x = max_x.max(v.X);
        min_y = min_y.min(v.Y);
        max_y = max_y.max(v.Y);
    }
    ((min_x + max_x) / 2.0, min_y, max_y)
}

/// The Windows implementation of [`Overlay`]: layered/click-through window
/// plumbing (unchanged from the GDI-only version) plus a Direct2D renderer
/// consuming the shared skull/theme model.
pub struct WinOverlay {
    hwnd: HWND,
    visible: bool,
    /// Held once rather than recreated per frame: `ID2D1DCRenderTarget` is
    /// designed to be rebound to a fresh DC (`BindDC`) every frame, which
    /// is cheap, so there is no need to recreate the factory or the render
    /// target object itself on each `paint()` call.
    factory: ID2D1Factory,
    render_target: ID2D1DCRenderTarget,
    /// The skull's motion state and its latest pose (pure; see skull.rs) —
    /// mirrors `macos.rs`'s `Model`, minus the AppKit-specific
    /// `FrameStats`/display-link parts (docs/plans/windows-overlay.md
    /// §2.8: this crate has no Windows equivalent of `CADisplayLink` yet,
    /// so the animator is stepped once per `render()` call instead, which
    /// the host's `overlay_main` sleep loop already calls at ~33ms
    /// regardless of whether the frame's data changed).
    animator: SkullAnimator,
    pose: SkullPose,
    /// Displayed mic level, eased toward the host's raw level the same way
    /// `macos.rs`'s `Model::level` is, so the jaw does not read the 30Hz
    /// host push rate as jitter.
    level: f64,
    /// Seconds since this overlay was created; the animator's clock.
    epoch: Instant,
    last_tick: f64,
    /// State of the previous frame, to detect utterance boundaries the
    /// same way `MacOverlay` does (entry animation, settle pop).
    last_state: OverlayState,
}

// HWND is a handle usable from the owning thread; the Overlay trait is
// consumed single-threaded (the daemon's render loop), matching macOS's
// MainThreadMarker discipline.
impl WinOverlay {
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            // DPI first, before any window exists: awareness cannot be
            // raised for windows created under the old context. Failure is
            // survivable (see module docs).
            if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
                eprintln!(
                    "overlay: could not set per-monitor DPI awareness (already set \
                     elsewhere?); positions may be off on scaled monitors"
                );
            }

            let class_name = wide("OutLoudOverlayClass");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };
            // Re-registration (second overlay in one process) fails benignly;
            // CreateWindowExW is the call whose error matters.
            let _ = RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(wide("OutLoud").as_ptr()),
                WS_POPUP,
                0,
                0,
                PANEL_W,
                PANEL_H,
                None,
                None,
                None,
                None,
            )?;

            // Single-threaded: this overlay is driven from one render loop
            // (main.rs's `overlay_main`), never touched concurrently, so
            // there is no multithread-factory overhead to pay for.
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    // See the module doc's premultiply caveat: this is the
                    // one interop assumption this file cannot verify
                    // without Windows hardware.
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let render_target = factory.CreateDCRenderTarget(&rt_props)?;

            Ok(WinOverlay {
                hwnd,
                visible: false,
                factory,
                render_target,
                animator: SkullAnimator::new(),
                pose: SkullPose::at_rest(),
                level: 0.0,
                epoch: Instant::now(),
                last_tick: 0.0,
                last_state: OverlayState::Idle,
            })
        }
    }

    /// The screen's bounds in our top-left-origin convention. Multi-monitor
    /// refinement (MonitorFromPoint on the anchor) is noted follow-up; the
    /// primary monitor covers the spike.
    fn screen_bounds(&self) -> Rect {
        unsafe {
            Rect::new(
                0.0,
                0.0,
                GetSystemMetrics(SM_CXSCREEN) as f64,
                GetSystemMetrics(SM_CYSCREEN) as f64,
            )
        }
    }

    /// Where the mouse is, as a layout anchor. Public for parity with the
    /// macOS backend's `mouse_anchor`.
    pub fn mouse_anchor(&self) -> Anchor {
        let mut p = windows::Win32::Foundation::POINT::default();
        unsafe {
            if GetCursorPos(&mut p).is_ok() {
                return Anchor::Cursor(Point {
                    x: p.x as f64,
                    y: p.y as f64,
                });
            }
        }
        Anchor::Corner
    }

    /// Fill one skull polygon with a flat color. The Direct2D analog of
    /// `macos.rs::fill_poly`.
    fn fill_poly(&self, poly: &[Point], color: D2D1_COLOR_F) -> windows::core::Result<()> {
        let Some(geometry) = poly_geometry(&self.factory, poly)? else {
            return Ok(());
        };
        unsafe {
            let brush = self.render_target.CreateSolidColorBrush(&color, None)?;
            self.render_target.FillGeometry(&geometry, &brush, None);
        }
        Ok(())
    }

    /// Fill one skull polygon with a vertical two-tone gradient instead of
    /// one flat tone — the Direct2D analog of `macos.rs::fill_poly_lit`.
    /// A single flat fill is what makes a shape read as a sticker; a lit
    /// top meeting a shaded underside is the cheapest cue that it is a
    /// solid object.
    fn fill_poly_lit(
        &self,
        poly: &[Point],
        lit: D2D1_COLOR_F,
        shade: D2D1_COLOR_F,
    ) -> windows::core::Result<()> {
        let Some(geometry) = poly_geometry(&self.factory, poly)? else {
            return Ok(());
        };
        let (mid_x, min_y, max_y) = gradient_axis(poly);
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: shade,
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: lit,
                },
            ];
            let stop_collection = self.render_target.CreateGradientStopCollection(
                &stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            )?;
            let lin_props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                // Bottom of the shape gets `shade`, top gets `lit`: lit
                // top, shaded underside, agreeing with the module doc's
                // light-from-above convention.
                startPoint: Vector2 { X: mid_x, Y: max_y },
                endPoint: Vector2 { X: mid_x, Y: min_y },
            };
            let brush =
                self.render_target
                    .CreateLinearGradientBrush(&lin_props, None, &stop_collection)?;
            self.render_target.FillGeometry(&geometry, &brush, None);
        }
        Ok(())
    }

    /// The gaze aura behind the skull: one native radial-gradient fill,
    /// simpler than `macos.rs`'s 20-ring `NSGradient` workaround because
    /// Direct2D has a real `ID2D1RadialGradientBrush` (plan §2.5 — this is
    /// better than parity, not a compromise). Radius and alpha both track
    /// `pose.eye_glow` the same way macOS's `gain`/`reach` do, so the aura
    /// still breathes with the voice.
    fn draw_glow(
        &self,
        accent: theme::Color,
        eye_glow: f64,
        fade: f64,
    ) -> windows::core::Result<()> {
        let reach = GLOW_R * (0.72 + 0.28 * eye_glow);
        // Alpha at the centre; the stop collection fades it to fully
        // transparent by the outer edge, same shape as macOS's Gaussian
        // falloff approximated with a 3-stop curve instead of 20 discrete
        // rings.
        let inner_a = (0.16 * (0.55 + 0.45 * eye_glow) * fade).clamp(0.0, 1.0);
        let mid = D2D1_COLOR_F {
            r: accent.r as f32,
            g: accent.g as f32,
            b: accent.b as f32,
            a: inner_a as f32,
        };
        let outer = D2D1_COLOR_F {
            r: accent.r as f32,
            g: accent.g as f32,
            b: accent.b as f32,
            a: 0.0,
        };
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: mid,
                },
                D2D1_GRADIENT_STOP {
                    position: 0.45,
                    color: mid,
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: outer,
                },
            ];
            let stop_collection = self.render_target.CreateGradientStopCollection(
                &stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            )?;
            let radial_props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: Vector2 {
                    X: GLOW_CX as f32,
                    Y: GLOW_CY as f32,
                },
                gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 },
                radiusX: reach as f32,
                radiusY: reach as f32,
            };
            let brush = self.render_target.CreateRadialGradientBrush(
                &radial_props,
                None,
                &stop_collection,
            )?;
            let ellipse = D2D1_ELLIPSE {
                point: Vector2 {
                    X: GLOW_CX as f32,
                    Y: GLOW_CY as f32,
                },
                radiusX: reach as f32,
                radiusY: reach as f32,
            };
            self.render_target.FillEllipse(&ellipse, &brush);
        }
        Ok(())
    }

    /// The skull: pure posed geometry from [`crate::skull`], mapped into
    /// the panel's skull box and filled via Direct2D. Mirrors
    /// `macos.rs::draw_skull`: the gaze aura (native radial gradient
    /// instead of macOS's 20-ring fake, plan §2.5), the bone fills, and a
    /// cheap offset-fill shadow instead of a real blurred one (stage 2,
    /// plan §2.4 option b — see the module doc for why a DC render target
    /// cannot cheaply do a real blur). Text lane (stage 3b) is not wired
    /// in yet.
    fn draw_skull(
        &self,
        state: OverlayState,
        pose: &SkullPose,
        reduce_motion: bool,
    ) -> windows::core::Result<()> {
        let accent = theme::accent(state);
        let geo = skull::posed_geometry(pose);
        let fade = pose.opacity;

        // Aura first, so the skull draws over it. Reduced motion drops it
        // entirely: it exists to flicker with the voice, same rationale as
        // macos.rs::draw_skull's identical guard.
        if !reduce_motion {
            self.draw_glow(accent, pose.eye_glow, fade)?;
        }

        // Three bone tones instead of one, so a lit top can meet a shaded
        // underside — same rationale as macos.rs's identical comment.
        let bone_lit = d2d1_color(theme::palette::PAPER, 0.99 * fade);
        let bone = d2d1_color(theme::palette::PAPER.alpha(0.86), 0.96 * fade);
        let bone_shade = d2d1_color(theme::palette::PAPER.alpha(0.66), 0.96 * fade);
        let dark = d2d1_color(theme::palette::INK, 0.94 * fade);

        unsafe {
            // Cast the whole skull onto the desktop behind it: one shadow
            // pass for cranium+jaw together (not per sub-polygon), same
            // "one solid object" reasoning as macos.rs::with_drop_shadow.
            // No blur here (see module doc) — just a small downward offset
            // at low alpha, filled before the real lit passes.
            let shadow_color = d2d1_color(theme::palette::INK, 0.42 * fade);
            let shift = Matrix3x2::translation(0.0, SHADOW_OFFSET_Y);
            self.render_target.SetTransform(&shift);
            self.fill_poly(&geo.cranium, shadow_color)?;
            self.fill_poly(&geo.jaw, shadow_color)?;
            self.render_target.SetTransform(&Matrix3x2::identity());
        }

        self.fill_poly(&geo.mouth, dark)?;
        self.fill_poly_lit(&geo.cranium, bone_lit, bone_shade)?;
        // The jaw sits under the cranium, so it never catches the top
        // light: its own gradient runs darker at both ends, same as
        // macos.rs.
        self.fill_poly_lit(&geo.jaw, bone, bone_shade)?;

        for socket in &geo.sockets {
            self.fill_poly(socket, dark)?;
        }
        // Eye glow: the state's accent inside the sockets, alpha from the
        // pose (listening brightens with the voice, transcribing shimmers,
        // loading pulses, errors stare). This is the small in-socket glow
        // from `skull::posed_geometry`'s own `eyes` polygons, not the
        // 20-ring aura behind the skull (that is stage 3).
        let glow = d2d1_color(accent, (0.25 + 0.75 * pose.eye_glow) * fade);
        for eye in &geo.eyes {
            self.fill_poly(eye, glow)?;
        }
        self.fill_poly(&geo.nose, dark)?;
        for tooth in &geo.teeth {
            self.fill_poly(tooth, bone)?;
        }
        Ok(())
    }

    /// Draw one frame into the layered window's premultiplied-alpha DIB
    /// and push it to the screen at `pos`. One function so DC/bitmap
    /// lifetimes have a single scope with a single cleanup path — same
    /// shape as the old GDI-only `paint`, with GDI shape/text drawing
    /// replaced by a Direct2D pass bound to the same DC.
    fn paint(
        &self,
        state: OverlayState,
        pose: &SkullPose,
        reduce_motion: bool,
        pos: Rect,
    ) -> anyhow::Result<()> {
        unsafe {
            let screen_dc: HDC = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: PANEL_W,
                    biHeight: -PANEL_H, // negative: top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap: HBITMAP =
                CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old_bitmap = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

            let rect = windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: PANEL_W,
                bottom: PANEL_H,
            };
            self.render_target.BindDC(mem_dc, &rect)?;
            self.render_target.BeginDraw();
            // Fully transparent background: the panel itself is invisible,
            // only the skull's own pixels are, matching macOS's
            // `panel.setOpaque(false)` / clear background color.
            self.render_target.Clear(None);
            self.draw_skull(state, pose, reduce_motion)?;
            self.render_target.EndDraw(None, None)?;

            // No manual premultiply pass here — see the module doc's
            // "one thing this file cannot verify" section. Direct2D wrote
            // straight into `bits` through `mem_dc` already in the format
            // the pixel format above declared.
            let _ = bits;

            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255, // per-pixel alpha does the work
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let dst = windows::Win32::Foundation::POINT {
                x: pos.origin.x as i32,
                y: pos.origin.y as i32,
            };
            let size = windows::Win32::Foundation::SIZE {
                cx: PANEL_W,
                cy: PANEL_H,
            };
            let src = windows::Win32::Foundation::POINT { x: 0, y: 0 };
            let res = UpdateLayeredWindow(
                self.hwnd,
                Some(screen_dc),
                Some(&dst),
                Some(&size),
                Some(mem_dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );

            // Cleanup in reverse order of creation, before error handling,
            // so a failed UpdateLayeredWindow cannot leak GDI objects
            // (a classic slow-death bug: GDI handle exhaustion).
            SelectObject(mem_dc, old_bitmap);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);

            res?;
            Ok(())
        }
    }
}

impl Overlay for WinOverlay {
    fn render(&mut self, frame: &OverlayFrame) -> anyhow::Result<()> {
        if !frame.state.overlay_visible() {
            self.last_state = frame.state;
            return self.hide();
        }

        // No dedicated animation clock (see the struct doc's note on
        // `docs/plans/windows-overlay.md` §2.8): the host's `overlay_main`
        // sleep loop already calls `render()` every ~33ms regardless of
        // whether the frame's data changed, so stepping the animator here
        // is the "reuse the sleep loop" recommendation the plan makes.
        let now = self.epoch.elapsed().as_secs_f64();
        let dt = (now - self.last_tick).max(0.0);
        self.last_tick = now;

        // The entry gesture: fires once per appearance, same as
        // MacOverlay::render — keyed off `visible` before the show path
        // below sets it.
        if !self.visible {
            self.animator.trigger_entry(now);
        }
        // Key released: the recognizer is finalizing, so trigger the
        // skull's commit gesture (jaw shuts, one damped-spring pop), same
        // rule as MacOverlay::render.
        if frame.state == OverlayState::Transcribing && self.last_state == OverlayState::Listening {
            self.animator.trigger_settle(now);
        }

        // Ease the displayed level toward the host's raw level so the jaw
        // does not read the ~30Hz host push rate as jitter, mirroring
        // MacOverlay's identical easing of `Model::level`. Windows has no
        // per-band audio path yet (`set_audio_bands` stays the trait's
        // default no-op), so the jaw always follows the broadband level.
        let level_target = layout::shape_level(frame.audio_level) as f64;
        let ease = 1.0 - (-dt / layout::EASE_TAU).exp();
        self.level += (level_target - self.level) * ease;

        // Reduce Motion: not wired up yet. `SPI_GETCLIENTAREAANIMATION`
        // (plan §2.7) needs confirming against a real Windows accessibility
        // toggle on hardware this session does not have, so it stays
        // explicitly deferred to stage 4 rather than guessed at here.
        let reduce_motion = false;
        self.pose = self
            .animator
            .step(now, dt, frame.state, self.level, reduce_motion);

        let overlay_size = Size {
            width: PANEL_W as f64,
            height: PANEL_H as f64,
        };
        let pos = place(frame.anchor, overlay_size, self.screen_bounds());
        self.paint(frame.state, &self.pose, reduce_motion, pos)?;

        self.last_state = frame.state;

        if !self.visible {
            unsafe {
                // SW_SHOWNOACTIVATE, never SW_SHOW: see module docs. The
                // topmost z-order was set at creation via WS_EX_TOPMOST and
                // is re-asserted here in case a fullscreen app displaced us.
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                );
            }
            self.visible = true;
        }
        Ok(())
    }

    fn hide(&mut self) -> anyhow::Result<()> {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
        Ok(())
    }

    fn is_visible(&self) -> bool {
        self.visible
    }
}

impl Drop for WinOverlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
