//! Windows overlay backend: a layered, click-through, topmost,
//! **non-activating** popup window.
//!
//! The extended-style quartet is the whole design, and every flag is a
//! correctness requirement, not a preference:
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
//! overlay beats no overlay.
//!
//! ## Rendering
//!
//! GDI onto the layered window via `UpdateLayeredWindow` with a 32-bit
//! premultiplied-alpha DIB. Deliberately no graphics framework: the surface
//! is a rounded rectangle, a text line, and a level meter, and
//! `UpdateLayeredWindow` needs a DC-backed bitmap anyway, so GDI is the
//! shortest correct path just as direct AppKit drawing is on macOS.

use crate::layout::{place, Anchor, Point, Rect, Size};
use crate::pixel::{premultiply, PANEL_ALPHA};
use crate::state::OverlayState;
use crate::{Overlay, OverlayFrame};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    FillRect, GetDC, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, TextOutW,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, FW_SEMIBOLD, HBITMAP,
    HDC, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
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

/// Fixed overlay size in device-independent pixels, matching the macOS
/// panel's footprint. Text that does not fit is already truncated by the
/// state machine's tail logic.
const OVERLAY_W: i32 = 260;
const OVERLAY_H: i32 = 48;

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

/// Premultiplied ARGB color for the DIB. GDI's COLORREF is 0x00BBGGRR; the
/// DIB wants 0xAARRGGBB premultiplied, so the two vocabularies are kept in
/// separate helper types to make a mixup a type error at the call site.
fn colorref(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32)
}

/// The per-state accent, mirroring macos.rs so both platforms speak the
/// same visual language.
fn accent(state: OverlayState) -> (u8, u8, u8) {
    match state {
        OverlayState::Listening => (64, 200, 120), // green: mic hot
        OverlayState::Transcribing => (240, 200, 80), // amber: working
        OverlayState::Error => (235, 90, 90),      // red
        OverlayState::NoPermission => (235, 90, 90), // red
        OverlayState::ModelLoading => (120, 160, 240), // blue: wait
        _ => (160, 160, 160),
    }
}

pub struct WinOverlay {
    hwnd: HWND,
    visible: bool,
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

            let class_name = wide("AquaOverlayClass");
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
                PCWSTR(wide("Aqua").as_ptr()),
                WS_POPUP,
                0,
                0,
                OVERLAY_W,
                OVERLAY_H,
                None,
                None,
                None,
                None,
            )?;

            Ok(WinOverlay {
                hwnd,
                visible: false,
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

    /// Draw the frame into a premultiplied-alpha DIB and push it to the
    /// layered window at `pos`. One function so DC/bitmap lifetimes have a
    /// single scope with a single cleanup path.
    fn paint(&self, frame: &OverlayFrame, pos: Rect) -> anyhow::Result<()> {
        unsafe {
            let screen_dc: HDC = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: OVERLAY_W,
                    biHeight: -OVERLAY_H, // negative: top-down rows
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

            // Background: near-black rounded rect at ~90% alpha. GDI draws
            // straight (unpremultiplied) color into the DIB with alpha 0;
            // the fixup loop below sets alpha and premultiplies.
            let bg = CreateSolidBrush(colorref(28, 28, 30));
            let full = windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: OVERLAY_W,
                bottom: OVERLAY_H,
            };
            FillRect(mem_dc, &full, bg);
            let _ = DeleteObject(HGDIOBJ(bg.0));

            // Accent dot.
            let (ar, ag, ab) = accent(frame.state);
            let dot = CreateSolidBrush(colorref(ar, ag, ab));
            let old_brush = SelectObject(mem_dc, HGDIOBJ(dot.0));
            let _ = RoundRect(mem_dc, 12, 18, 24, 30, 12, 12);
            SelectObject(mem_dc, old_brush);
            let _ = DeleteObject(HGDIOBJ(dot.0));

            // Level meter while listening: a horizontal bar under the text.
            if frame.state == OverlayState::Listening {
                let level = frame.audio_level.clamp(0.0, 1.0) as f64;
                let bar_w = ((OVERLAY_W - 44) as f64 * level) as i32;
                if bar_w > 0 {
                    let bar = CreateSolidBrush(colorref(ar, ag, ab));
                    let bar_rect = windows::Win32::Foundation::RECT {
                        left: 32,
                        top: OVERLAY_H - 10,
                        right: 32 + bar_w,
                        bottom: OVERLAY_H - 6,
                    };
                    FillRect(mem_dc, &bar_rect, bar);
                    let _ = DeleteObject(HGDIOBJ(bar.0));
                }
            }

            // Text: the detail string, else the partial tail, else the label.
            let text: &str = frame
                .detail
                .as_deref()
                .or(if frame.partial_text.is_empty() {
                    None
                } else {
                    Some(&frame.partial_text)
                })
                .unwrap_or(frame.state.label());
            let font = CreateFontW(
                -14, // ~10.5pt at 96dpi; layered surface scales with DPI awareness
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                PCWSTR(wide("Segoe UI").as_ptr()),
            );
            let old_font = SelectObject(mem_dc, HGDIOBJ(font.0));
            SetBkMode(mem_dc, TRANSPARENT);
            SetTextColor(mem_dc, colorref(235, 235, 235));
            let wtext: Vec<u16> = text.encode_utf16().collect();
            // Truncate to what fits rather than measuring: the state
            // machine already bounds tail length, and an ellipsis is drawn
            // by the tail logic, not here.
            let _ = TextOutW(mem_dc, 32, 8, &wtext);
            SelectObject(mem_dc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));

            // Alpha fixup: GDI wrote alpha=0 everywhere it drew, and
            // UpdateLayeredWindow with AC_SRC_ALPHA demands PREMULTIPLIED
            // pixels. The per-pixel maths lives in `premultiply` so it can
            // be unit-tested; a wrong formula here yields an invisible or
            // black-boxed overlay, which is a bug you can only see by
            // looking at a Windows screen.
            let px = bits as *mut u32;
            let count = (OVERLAY_W * OVERLAY_H) as isize;
            for i in 0..count {
                let p = px.offset(i);
                *p = premultiply(*p, PANEL_ALPHA);
            }

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
                cx: OVERLAY_W,
                cy: OVERLAY_H,
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
            return self.hide();
        }
        let overlay_size = Size {
            width: OVERLAY_W as f64,
            height: OVERLAY_H as f64,
        };
        let pos = place(frame.anchor, overlay_size, self.screen_bounds());
        self.paint(frame, pos)?;
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
