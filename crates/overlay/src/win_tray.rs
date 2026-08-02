//! The Windows notification-area presence: a `Shell_NotifyIcon` icon whose
//! click opens an `HMENU` tree rebuilt from a [`MenuModel`], the Win32
//! sibling of `status_item.rs`'s `MacStatusItem`.
//!
//! Why this exists at all: without it, a running `outloud.exe` has no way
//! to be paused or quit except Task Manager — the same "invisible and
//! unquittable" problem the macOS status item was built to fix. See
//! `docs/plans/windows-tray.md` for the full design and the staging this
//! module follows (Stage 0: message window + icon + fixed menu; Stage 1,
//! folded in here since the tree-walker is not meaningfully more code than
//! a hardcoded two-row menu: the *actual* `menubar::build()` tree, drawn
//! once in a neutral colour rather than a full per-state icon set).
//!
//! Three properties mirrored from the macOS backend, because they are the
//! same correctness requirements on either platform:
//!
//! 1. **The window never activates.** A message-only window (`HWND_MESSAGE`
//!    parent) has no visible surface at all, so there is nothing to steal
//!    focus from the field the user is dictating into. Opening the popup
//!    menu is the one moment Windows legitimately takes input focus
//!    (`SetForegroundWindow`, required so `TrackPopupMenu` dismisses on an
//!    outside click), and it is released the instant the menu closes.
//! 2. **Clicks are queued, never executed here.** `wnd_proc` runs on
//!    whatever thread pumps this window's message loop; it records the
//!    clicked [`MenuId`] and returns immediately so a slow host handler can
//!    never stall menu tracking or the tray icon's message delivery.
//! 3. **The model is diffed, not rebuilt every frame.** Same reasoning as
//!    `MacStatusItem::apply`: `MenuModel: PartialEq` (`menu.rs`) is already
//!    the "did anything change" check on macOS; reused verbatim here so
//!    Windows never destroys and reopens a menu the user might have open.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject, ExtCreatePen,
    Polygon, Polyline, SelectObject, BS_SOLID, HBITMAP, HGDIOBJ, LOGBRUSH, PS_ENDCAP_ROUND,
    PS_GEOMETRIC,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, GetCursorPos, GetSystemMetrics, PostMessageW, RegisterClassW,
    SetForegroundWindow, TrackPopupMenu, GWLP_USERDATA, HICON, HMENU, HWND_MESSAGE, MF_CHECKED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, SM_CXSMICON, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_APP, WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

use crate::mark;
use crate::menu::{MenuId, MenuItem, MenuModel};
use crate::theme::Color;

/// The tray icon's callback message. `WM_APP` (0x8000) is the first id an
/// application may define without colliding with a system message.
const WM_TRAYICON: u32 = WM_APP + 1;

/// Fixed application-defined identifier for the one tray icon this process
/// ever creates. `Shell_NotifyIcon` keys an icon by `(hWnd, uID)`, and one
/// icon is all this daemon has, so there is nothing to disambiguate.
const TRAY_ICON_ID: u32 = 1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copy `text` into a fixed-size `u16` tooltip buffer, truncating rather
/// than overflowing. `NOTIFYICONDATAW::szTip` is a 128-`WCHAR` buffer
/// (127 chars + NUL); `Status::tooltip`/`status_line()` on the host side
/// build arbitrary-length strings (permission text, mic name, config
/// errors concatenated in principle), and macOS has no such limit
/// (`NSString` is unbounded) so this is a real platform divergence, not a
/// copy-paste risk to silently drop.
fn copy_truncated(dst: &mut [u16; 128], text: &str) {
    *dst = [0u16; 128];
    let encoded: Vec<u16> = text.encode_utf16().collect();
    let n = encoded.len().min(dst.len() - 1); // room for the NUL
    dst[..n].copy_from_slice(&encoded[..n]);
}

/// The window procedure for the message-only tray window. Unlike the
/// overlay's `wnd_proc` (`windows.rs`, `DefWindowProcW`-only: that window
/// takes no input), this one *does* handle messages: the tray callback and
/// nothing else. Everything else falls through to `DefWindowProcW`.
///
/// The click queue lives behind `GWLP_USERDATA` as a raw pointer to the
/// `RefCell<Vec<MenuId>>`, rather than a global, so a second `WinTray` in
/// the same process (tests, `status-demo`-style tools) does not share
/// state with the first — the same reasoning `StatusTarget` on macOS
/// gets for free from being a per-instance Objective-C object.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if msg == WM_TRAYICON {
            // NOTIFYICON_VERSION_4 packs the mouse/keyboard event into the
            // LOW word of lParam (WM_LBUTTONUP, WM_RBUTTONUP,
            // WM_CONTEXTMENU, ...); the icon id (always TRAY_ICON_ID here)
            // rides in the high word. See docs/plans/windows-tray.md §2.3.
            let event = (lparam.0 as usize & 0xFFFF) as u32;
            if matches!(event, WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU) {
                // macOS gives every click (left or right) the same menu —
                // NSStatusItem with a menu assigned always opens it, no
                // left/right distinction. Match that here rather than
                // inventing an interaction macOS never had.
                open_menu_and_dispatch(hwnd);
            }
            return LRESULT(0);
        }

        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

/// Fetch the click queue pointer stashed at `GWLP_USERDATA` and, if
/// present, build+show the menu, pushing the clicked id (if any) onto it.
unsafe fn open_menu_and_dispatch(hwnd: HWND) {
    unsafe {
        let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if ptr == 0 {
            return; // WM_CREATE has not run yet, or the tray was torn down.
        }
        let state = &*(ptr as *const TrayState);

        let Some(model) = state.model.borrow().clone() else {
            return; // No model applied yet: nothing to show.
        };

        let Ok(menu) = build_menu(&model.items) else {
            return;
        };

        // 1. Required or the menu will not dismiss on an outside click —
        //    TrackPopupMenu's own documented requirement, and the first
        //    thing every Win32 tray sample gets wrong if it is skipped.
        let _ = SetForegroundWindow(hwnd);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // 2. TPM_RETURNCMD: the clicked command id comes back directly, no
        //    separate WM_COMMAND round trip needed.
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        // 3. Required after TrackPopupMenu returns, per MSDN's own tray
        //    sample, to work around a documented repaint/focus glitch.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        // Menus are NOT auto-freed; leaking one HMENU per click would be a
        // slow-motion handle leak that shows up only after days of uptime.
        let _ = DestroyMenu(menu);

        if cmd.0 != 0 {
            state.clicks.borrow_mut().push(id_for_tag(cmd.0 as u32));
        }
    }
}

/// `MenuId` -> Win32 menu command id, saturating rather than wrapping so an
/// absurd id can never collide with a real one. Mirrors
/// `status_item.rs::tag_for`, except Win32 command ids are `u32` (`UINT`)
/// rather than `isize`, so the saturation ceiling is `u32::MAX`.
fn tag_for(id: MenuId) -> u32 {
    u32::try_from(id.0).unwrap_or(u32::MAX)
}

/// Inverse of [`tag_for`]. `0` is not a valid command id (`TrackPopupMenu`
/// returns 0 for "no selection" and menus never assign it), so a real id of
/// exactly `u32::MAX` collides with the saturation ceiling in the same
/// (harmless, absurd-input-only) way the macOS side accepts.
fn id_for_tag(tag: u32) -> MenuId {
    MenuId(tag as u64)
}

/// Recursively translate an `overlay::menu::MenuItem` tree into an `HMENU`,
/// per the table in docs/plans/windows-tray.md §2.4.
fn build_menu(items: &[MenuItem]) -> windows::core::Result<HMENU> {
    unsafe {
        let menu = CreatePopupMenu()?;
        for item in items {
            append_item(menu, item)?;
        }
        Ok(menu)
    }
}

unsafe fn append_item(menu: HMENU, item: &MenuItem) -> windows::core::Result<()> {
    unsafe {
        match item {
            MenuItem::Separator => {
                AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
            }
            MenuItem::Label(text) => {
                // Disabled: it is information (the status line, a config
                // error), not a control that does nothing when clicked —
                // matches macOS's `it.setEnabled(false)` for labels.
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(wide(text).as_ptr()))?;
            }
            MenuItem::Item {
                title,
                id,
                checked,
                enabled,
            } => {
                let mut flags = MF_STRING;
                if *checked {
                    flags |= MF_CHECKED;
                }
                if !*enabled {
                    flags |= MF_GRAYED;
                }
                AppendMenuW(
                    menu,
                    flags,
                    tag_for(*id) as usize,
                    PCWSTR(wide(title).as_ptr()),
                )?;
            }
            MenuItem::Submenu { title, items } => {
                let child = build_menu(items)?;
                AppendMenuW(
                    menu,
                    MF_POPUP,
                    child.0 as usize,
                    PCWSTR(wide(title).as_ptr()),
                )?;
            }
        }
        Ok(())
    }
}

/// State shared between the tray's public handle and its `wnd_proc`,
/// reached through `GWLP_USERDATA`. `RefCell`, not a `Mutex`: everything
/// that touches this runs on the single thread that owns the message
/// pump, the same single-threaded discipline `MainThreadMarker` enforces
/// on the macOS side.
struct TrayState {
    model: RefCell<Option<MenuModel>>,
    clicks: RefCell<Vec<MenuId>>,
}

/// The live tray icon. Dropping it removes the icon from the notification
/// area and destroys the message window.
pub struct WinTray {
    hwnd: HWND,
    /// Kept alive for the lifetime of the tray: `Box::into_raw` handed the
    /// pointer to `GWLP_USERDATA`, and this is what reclaims it on drop.
    state: *mut TrayState,
    /// Last model applied, for the same reason `MacStatusItem::applied`
    /// exists: an unchanged model must cost one comparison, not an icon
    /// rebuild, and re-adding an unchanged tray icon would flicker it.
    applied: Option<MenuModel>,
    /// The rendered icon, rebuilt only when it actually needs to change
    /// (currently: never, past the first draw — Stage 0/1 draws one
    /// neutral-colour icon rather than the full per-state set in
    /// docs/plans/windows-tray.md §3, which is explicitly deferred).
    icon: Option<HICON>,
}

/// A process-unique class name. Registering the same class name twice in
/// one process is a benign no-op (mirrors `windows.rs`'s own
/// `RegisterClassW` comment), so a counter here only matters for the
/// unlikely case of two `WinTray`s coexisting (tests), where distinct
/// classes keep their `wnd_proc`/userdata wiring from colliding.
static CLASS_COUNTER: AtomicU64 = AtomicU64::new(0);

impl WinTray {
    /// Create the message-only window and register the tray icon. Must run
    /// on the thread that will pump this window's messages: message
    /// delivery for a custom window class is per-thread-queue, the same
    /// constraint `crates/hotkey/src/backend/windows.rs` already documents
    /// for the keyboard hook.
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            let n = CLASS_COUNTER.fetch_add(1, Ordering::Relaxed);
            let class_name = wide(&format!("OutLoudTrayClass{n}"));
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };
            // Re-registration failure (second tray in one process, same
            // counter value reused after a very long run) is benign; the
            // CreateWindowExW error below is the one that matters.
            let _ = RegisterClassW(&wc);

            let state = Box::into_raw(Box::new(TrayState {
                model: RefCell::new(None),
                clicks: RefCell::new(Vec::new()),
            }));

            // HWND_MESSAGE as parent: a genuine message-only window
            // (Vista+), never shown, no taskbar entry — not a 0x0 visible
            // window, which could still theoretically activate or appear
            // in some enumeration tool.
            let hwnd = match CreateWindowExW(
                Default::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(wide("OutLoud Tray").as_ptr()),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                Some(state as *const core::ffi::c_void),
            ) {
                Ok(hwnd) => hwnd,
                Err(e) => {
                    // Reclaim before returning: nothing else will.
                    drop(Box::from_raw(state));
                    return Err(e.into());
                }
            };
            windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                state as isize,
            );

            let mut tray = WinTray {
                hwnd,
                state,
                applied: None,
                icon: None,
            };
            tray.register_icon()?;
            Ok(tray)
        }
    }

    fn state(&self) -> &TrayState {
        unsafe { &*self.state }
    }

    fn register_icon(&mut self) -> anyhow::Result<()> {
        let icon = draw_mark_icon(None)?;
        self.icon = Some(icon);
        let mut data = notify_icon_data(self.hwnd, icon, "OutLoud");
        unsafe {
            if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                anyhow::bail!("Shell_NotifyIconW(NIM_ADD) failed");
            }
            // Skipping this is a classic silent bug: pre-v4 behavior sends
            // WM_RBUTTONUP etc. with lParam as raw cursor coordinates; v4
            // packs the event and icon id instead. Getting the version
            // wrong produces a tray icon that shows but never opens a
            // menu.
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
        Ok(())
    }

    /// Push a model to the tray. Cheap and idempotent: an identical model
    /// is ignored, so the host can call this every frame exactly as it
    /// does for `MacStatusItem::apply`.
    pub fn apply(&mut self, model: &MenuModel) {
        if self.applied.as_ref() == Some(model) {
            return;
        }
        // Update the tooltip live; the menu itself is rebuilt lazily, at
        // click time, from whatever is in `state().model` — rebuilding an
        // HMENU here would be wasted work on every unchanged-menu frame.
        let data = notify_icon_data(self.hwnd, self.current_icon(), &model.tooltip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY_COMPAT, &data);
        }
        *self.state().model.borrow_mut() = Some(model.clone());
        self.applied = Some(model.clone());
    }

    fn current_icon(&self) -> HICON {
        self.icon.unwrap_or_default()
    }

    /// Take everything the user clicked since the last call. The host maps
    /// ids to actions; this crate never does — same contract as
    /// `MacStatusItem::drain_clicks`.
    pub fn drain_clicks(&self) -> Vec<MenuId> {
        std::mem::take(&mut *self.state().clicks.borrow_mut())
    }

    /// The window handle, for pumping its message queue
    /// (`GetMessageW`/`DispatchMessageW`) from `main.rs`.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

// NIM_MODIFY isn't re-exported with that exact name from every windows-rs
// version's prelude path used above; alias it locally so the call site
// above reads by intent (this IS NIM_MODIFY, imported under its real name
// below) rather than leaving a mystery constant in `apply`.
use windows::Win32::UI::Shell::NIM_MODIFY as NIM_MODIFY_COMPAT;

fn notify_icon_data(hwnd: HWND, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: icon,
        ..Default::default()
    };
    copy_truncated(&mut data.szTip, tip);
    data
}

/// Draw `mark::mark_in()` (the shared megaphone geometry, see `mark.rs`)
/// into an `HICON` sized for the tray (`SM_CXSMICON`, DPI-aware — the tray
/// genuinely scales on high-DPI displays, unlike a fixed toolbar bitmap).
///
/// `tint`: `None` draws a neutral light-grey mark, matching the "quiet
/// monochrome" untinted states on macOS closely enough for Stage 0/1 (the
/// full per-state, per-theme icon cache from docs/plans/windows-tray.md §3
/// is explicitly deferred — this always draws one icon, once).
fn draw_mark_icon(tint: Option<Color>) -> anyhow::Result<HICON> {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);

        let screen_dc = windows::Win32::Graphics::Gdi::GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let color_bmp: HBITMAP = create_compatible_bitmap_rgb(mem_dc, size, size)?;
        let old = SelectObject(mem_dc, HGDIOBJ(color_bmp.0));

        // Fully transparent background: the mask bitmap below is what
        // actually controls visibility, but painting the background with
        // a distinct, never-drawn colour first avoids sampling garbage
        // stack memory through GDI's uninitialized-bitmap behaviour.
        let bg = CreateSolidBrush(COLORREF(0));
        windows::Win32::Graphics::Gdi::FillRect(
            mem_dc,
            &windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: size,
                bottom: size,
            },
            bg,
        );
        let _ = DeleteObject(HGDIOBJ(bg.0));

        let (r, g, b) = match tint {
            Some(c) => (
                (c.r * 255.0) as u8,
                (c.g * 255.0) as u8,
                (c.b * 255.0) as u8,
            ),
            // Near-white: legible against the near-universally dark
            // Windows 10/11 default taskbar. Full per-theme detection
            // (light vs dark taskbar via the registry) is Stage 1 work
            // noted in docs/plans/windows-tray.md §3, not done here.
            None => (235, 235, 235),
        };
        let colorref = COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32);

        let m = mark::mark_in(size as f64);

        // The horn: filled solid polygon. A stroked outline collapses into
        // scribble at tray-icon size (see mark.rs's module doc).
        let brush = CreateSolidBrush(colorref);
        let old_brush = SelectObject(mem_dc, HGDIOBJ(brush.0));
        let pen = create_solid_pen_compat(colorref);
        let old_pen = SelectObject(mem_dc, HGDIOBJ(pen.0));
        let horn_pts: Vec<POINT> = m
            .horn
            .iter()
            .map(|p| POINT {
                x: p.x.round() as i32,
                y: p.y.round() as i32,
            })
            .collect();
        let _ = Polygon(mem_dc, &horn_pts);
        SelectObject(mem_dc, old_brush);
        SelectObject(mem_dc, old_pen);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = DeleteObject(HGDIOBJ(pen.0));

        // The wave arcs: stroked polylines with round caps (`ExtCreatePen`,
        // since plain `CreatePen` has no cap-style control), matching the
        // AppKit path's `NSLineCapStyle::Round`.
        let logbrush = LOGBRUSH {
            lbStyle: BS_SOLID,
            lbColor: colorref,
            lbHatch: 0,
        };
        let arc_pen = ExtCreatePen(PS_GEOMETRIC | PS_ENDCAP_ROUND, 2, &logbrush, None);
        let old_arc_pen = SelectObject(mem_dc, HGDIOBJ(arc_pen.0));
        for wave in &m.waves {
            let pts: Vec<POINT> = wave
                .iter()
                .map(|p| POINT {
                    x: p.x.round() as i32,
                    y: p.y.round() as i32,
                })
                .collect();
            let _ = Polyline(mem_dc, &pts);
        }
        SelectObject(mem_dc, old_arc_pen);
        let _ = DeleteObject(HGDIOBJ(arc_pen.0));

        SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
        windows::Win32::Graphics::Gdi::ReleaseDC(None, screen_dc);

        // CreateIconIndirect requires SOME mask bitmap even though modern
        // Windows honours the color bitmap's real content for a 32bpp
        // color-only icon; an all-zero 1bpp bitmap of the right size is
        // the documented minimum.
        let mask_bits = vec![0u8; ((size + 7) / 8 * size) as usize];
        let mask_bmp = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const _));

        let icon_info = windows::Win32::UI::WindowsAndMessaging::ICONINFO {
            fIcon: windows::core::BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bmp,
            hbmColor: color_bmp,
        };
        let icon = windows::Win32::UI::WindowsAndMessaging::CreateIconIndirect(&icon_info)?;

        let _ = DeleteObject(HGDIOBJ(color_bmp.0));
        let _ = DeleteObject(HGDIOBJ(mask_bmp.0));

        Ok(icon)
    }
}

/// `CreateCompatibleBitmap` wants a 1bpp-or-DC-depth bitmap; the tray icon
/// needs real 32bpp color so `CreateIconIndirect`'s `hbmColor` carries an
/// actual RGB image rather than a monochrome one, hence a plain
/// `CreateBitmap` at depth 32 instead.
unsafe fn create_compatible_bitmap_rgb(
    _dc: windows::Win32::Graphics::Gdi::HDC,
    w: i32,
    h: i32,
) -> windows::core::Result<HBITMAP> {
    Ok(unsafe { CreateBitmap(w, h, 1, 32, None) })
}

/// A 1px solid pen for the polygon outline (belt-and-suspenders: the fill
/// already covers the shape; a matching-colour outline just avoids a
/// 1px unfilled seam some GDI implementations leave at a polygon's edge).
unsafe fn create_solid_pen_compat(color: COLORREF) -> windows::Win32::Graphics::Gdi::HPEN {
    unsafe {
        windows::Win32::Graphics::Gdi::CreatePen(windows::Win32::Graphics::Gdi::PS_SOLID, 1, color)
    }
}

impl Drop for WinTray {
    fn drop(&mut self) {
        unsafe {
            let mut data = notify_icon_data(self.hwnd, HICON::default(), "");
            data.uFlags = Default::default();
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            if let Some(icon) = self.icon.take() {
                let _ = DestroyIcon(icon);
            }
            let _ = DestroyWindow(self.hwnd);
            // Reclaim the userdata box now that wnd_proc can no longer be
            // invoked for this window.
            drop(Box::from_raw(self.state));
        }
    }
}
