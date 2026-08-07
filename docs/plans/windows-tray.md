# Windows tray icon and menu: implementation plan

Goal: bring the Windows build to menu-bar parity with macOS — pause, quit,
settings, diagnostics, active-microphone visibility, and no console window
by default. This is a plan, not a patch; nothing outside this file was
edited.

## 0. Verified claims (read before trusting the rest of this doc)

- `crates/outloud/src/menubar.rs` (1018 lines, entire file) has **zero**
  `cfg(target_os)` and zero AppKit/objc2 imports. It is pure: `Status` +
  `Settings` in, `(MenuModel, Vec<Action>)` out. Confirmed by reading every
  line, not just the module doc's claim.
- `crates/overlay/src/menu.rs` (193 lines, entire file) — `MenuId`,
  `MenuItem`, `MenuModel`, `glyph_tint()` — is equally pure. No platform
  code anywhere in it.
- `crates/overlay/src/mark.rs` (293 lines) — the skull glyph as
  fractional-unit-square geometry (`Mark { horn: Vec<Point>, waves:
  Vec<Vec<Point>> }`) — is pure and its own module doc (lines 11-17)
  states it exists specifically so a Windows tray backend can render the
  identical shape. This is not aspirational; nothing about it depends on
  AppKit.
- `crates/outloud/src/menuhost.rs` (666 lines) is the I/O edge
  (config read/write, `open`/`cmd start`, diagnostics, quit) and is
  **already** cross-platform: `open_with()` at lines 462-492 has a working
  `#[cfg(target_os = "windows")]` arm (473-481) using `cmd /C start`.
  `MenuHost::new/reload/model/handle/write_setting/run_diagnostics` contain
  no AppKit calls. **This file needs no changes.**
- macOS-specific code begins at `crates/overlay/src/status_item.rs` line 1
  (the whole file: `objc2`, `objc2_app_kit`, `NSStatusItem`, `NSMenu`). It
  is gated in `crates/overlay/src/lib.rs` at
  `#[cfg(all(target_os = "macos", feature = "display"))]` (~line 54-58).
  This is the *only* file a Windows tray needs a sibling for.
- `crates/outloud/src/main.rs` lines 775-812: confirmed —
  `#[cfg(all(target_os = "windows", feature = "display"))] fn overlay_main`
  takes `_menu_host: Option<outloud::menuhost::MenuHost>` and never touches
  it. The render loop is a bare 33ms sleep with no message pump, no tray,
  no menu handling at all.
- `grep -rn "windows_subsystem"` across the repo: **zero matches**. The
  Windows binary is a console subsystem app today by default (no
  attribute overrides it), which is why a console window pops up on every
  launch — confirmed, not assumed.
- Baseline sanity: `cargo check --target x86_64-pc-windows-msvc -p outloud
  --features display` succeeds today (exit 0), so the plan below starts
  from a compiling tree.

## 1. Reusable vs. new, by file

| Layer | File | Status |
|---|---|---|
| Menu **model** (data) | `overlay::menu` (`menu.rs`) | Reuse as-is |
| Glyph **geometry** | `overlay::mark` (`mark.rs`) | Reuse as-is |
| Menu **policy** (what's in the menu, what a click does) | `outloud::menubar` (`menubar.rs`) | Reuse as-is |
| Menu **I/O** (config r/w, open, diagnostics, quit) | `outloud::menuhost` (`menuhost.rs`) | Reuse as-is |
| macOS status item | `overlay::status_item` | Reference implementation only |
| **Windows status item** | `overlay::win_tray` (new) | **Build this** |
| Wiring | `crates/outloud/src/main.rs:775-812` | **Rewrite this function** |

Net new code is one file (`overlay/src/win_tray.rs`, macOS's
`status_item.rs` is 357 lines — expect similar) plus a rewritten
`overlay_main` for Windows. Everything upstream of the status item is
already shared.

## 2. Shell_NotifyIcon design

### 2.1 The message-only window

A tray icon needs an `HWND` to receive its callback messages; it does not
need to be visible. Create one hidden window, owned by the same thread
that pumps its message loop (message delivery for a custom window class is
per-thread-queue, same constraint the hotkey hook already documents at
`crates/hotkey/src/backend/windows.rs:280-283`).

```rust
// Real APIs, windows crate 0.62 (already a workspace dependency):
use windows::Win32::UI::WindowsAndMessaging::{
    RegisterClassW, CreateWindowExW, DefWindowProcW, WNDCLASSW, WS_OVERLAPPED,
    HWND_MESSAGE, // parent = message-only window: never shown, no taskbar entry
};
```

Register a window class `"OutLoudTrayClass"` with a `wnd_proc` (mirrors
the pattern already in `crates/overlay/src/windows.rs:76-83`, which is
`DefWindowProcW`-only because that window takes no input). The tray
window's `wnd_proc` is different: it *does* handle messages.
`CreateWindowExW(0, class, title, WS_OVERLAPPED, 0,0,0,0, HWND_MESSAGE,
None, None, None)` — `HWND_MESSAGE` as parent makes it a message-only
window (Vista+), which is the correct primitive here, not a 0x0 visible
window (that would still theoretically activate/appear in some tools).

### 2.2 Registering the icon

```rust
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NOTIFYICONDATAW, NIM_ADD, NIM_MODIFY, NIM_DELETE,
    NIM_SETVERSION, NOTIFYICON_VERSION_4, NIF_ICON, NIF_MESSAGE, NIF_TIP,
};
```
(New Cargo feature needed: `Win32_UI_Shell`, not currently enabled on
either the `overlay` or `outloud` `windows` dependency.)

- `NOTIFYICONDATAW { cbSize, hWnd: tray_hwnd, uID: 1, uFlags: NIF_ICON |
  NIF_MESSAGE | NIF_TIP, uCallbackMessage: WM_TRAYICON, hIcon, szTip, ..
  }`, `Shell_NotifyIconW(NIM_ADD, &data)`.
- Immediately follow with `Shell_NotifyIconW(NIM_SETVERSION, &data)` with
  `data.Anonymous.uVersion = NOTIFYICON_VERSION_4`. Skipping this is a
  classic silent bug: pre-v4 behavior sends `WM_RBUTTONUP` etc. with
  `lParam` as raw cursor coordinates; v4 sends the same messages but with
  `lParam` as (x,y) packed **and** an extra `WM_CONTEXTMENU` message,
  which is the one every modern sample expects. Getting the version wrong
  produces a tray icon that *shows* but never opens a menu — a "the icon
  is there but nothing happens" bug indistinguishable from a click-routing
  bug.
- `szTip` is a **fixed 128-`WCHAR` buffer** (127 chars + NUL) in
  `NOTIFYICONDATAW`. `Status::tooltip` / `status_line()` in `menubar.rs`
  builds arbitrary-length strings (permission text, mic name, config
  errors concatenated in principle). Must truncate before copying in; the
  macOS side has no such limit (`NSString` is unbounded), so this is a
  real platform divergence to handle, not a copy-paste risk.

### 2.3 The callback message and click routing

Define `const WM_TRAYICON: u32 = WM_APP + 1;` (`WM_APP` is in
`Win32_UI_WindowsAndMessaging`, already enabled). In `wnd_proc`:

- `lparam` low word (with `NOTIFYICON_VERSION_4`) is the mouse/keyboard
  event: `WM_LBUTTONUP`, `WM_RBUTTONUP`, `WM_CONTEXTMENU`. macOS gives
  every click the same menu (no left/right distinction — `NSStatusItem`
  with a menu assigned always opens it). Match that here: open the same
  context menu on `WM_LBUTTONUP`, `WM_RBUTTONUP`, and `WM_CONTEXTMENU`,
  rather than reserving left-click for some other action. Keeps parity
  and avoids inventing an interaction macOS never had.

### 2.4 Building and showing the context menu

```rust
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, AppendMenuW, TrackPopupMenu, SetForegroundWindow,
    GetCursorPos, DestroyMenu, MF_STRING, MF_SEPARATOR, MF_GRAYED,
    MF_CHECKED, MF_POPUP, TPM_RIGHTBUTTON, TPM_RETURNCMD,
};
```

Walk `MenuModel.items` (from `menubar::build()`, unchanged) recursively,
translating `overlay::menu::MenuItem` 1:1:

| `MenuItem` variant | Win32 call |
|---|---|
| `Label(text)` | `AppendMenuW(menu, MF_STRING \| MF_GRAYED \| MF_DISABLED, 0, text)` — disabled, matches macOS's `it.setEnabled(false)` for labels (`status_item.rs:292-297`) |
| `Separator` | `AppendMenuW(menu, MF_SEPARATOR, 0, null)` |
| `Item { title, id, checked, enabled }` | `AppendMenuW(menu, MF_STRING \| (checked then MF_CHECKED) \| (!enabled then MF_GRAYED), tag_for(id), title)` |
| `Submenu { title, items }` | build a child `CreatePopupMenu()` recursively, `AppendMenuW(parent, MF_POPUP, child.0 as usize, title)` |

`tag_for(id)`: same saturating cast macOS uses at `status_item.rs:346-348`
(`MenuId(u64) -> isize`, `try_from().unwrap_or(MAX)`), except Win32 menu
command IDs are `u32` (`UINT`), so saturate to `u32::MAX` instead. Same
"an absurd id must not collide with a real one" reasoning applies
verbatim.

Showing it — **the three-line incantation every Win32 tray sample gets
wrong if you skip any line**, so name them explicitly:

```rust
SetForegroundWindow(tray_hwnd);           // 1. required or the menu won't dismiss on outside click
let mut pt = POINT::default();
GetCursorPos(&mut pt);
let cmd = TrackPopupMenu(
    menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, 0, tray_hwnd, None,
);                                          // 2. TPM_RETURNCMD: get the id back directly, no WM_COMMAND round trip needed
PostMessage(tray_hwnd, WM_NULL, ...);      // 3. required after TrackPopupMenu returns, per MSDN's own tray sample, to fix a documented Windows repaint/focus glitch
DestroyMenu(menu);                          // menus are NOT auto-freed; a leak here is one HMENU per click, forever
```

`TPM_RETURNCMD` means the clicked `MenuId` comes back as `cmd` directly —
no separate `WM_COMMAND` handler is needed for the menu path (though
`WM_COMMAND` still exists as the alternate path if `TrackPopupMenu` is
called without `TPM_RETURNCMD`; pick one, `TPM_RETURNCMD` is simpler here
since there's already an event loop iteration to hand the result to).

### 2.5 Routing into the existing `Action` enum

No new enum, no new routing logic needed. The `cmd`/`MenuId` from
`TrackPopupMenu` is exactly what `overlay::status_item::MacStatusItem::
drain_clicks()` produces on macOS, and exactly what `MenuHost::handle(id:
MenuId) -> bool` (`menuhost.rs:345-382`) already consumes. The Windows
tray module's public surface should mirror `MacStatusItem`'s:

```rust
impl WinTray {
    pub fn new(hwnd_owner: /* the tray window */) -> anyhow::Result<Self>;
    pub fn apply(&mut self, model: &MenuModel);      // diff against last-applied, like MacStatusItem::apply (status_item.rs:123-137)
    pub fn drain_clicks(&self) -> Vec<MenuId>;        // same contract
}
```

This means `main.rs`'s Windows `overlay_main` becomes a near-mirror of the
macOS one (`main.rs:604-768`): pump messages, call `tray.apply(host.model(
..))`, drain clicks into `host.handle(id)`, exit on `true`. The only
platform-specific piece is the message pump shape (`GetMessage`/
`DispatchMessage` loop instead of `nextEventMatchingMask`).

## 3. State-driven icon changes

macOS swaps the glyph's **tint**, never its shape (`overlay::menu::
glyph_tint`, `menu.rs:112-130`): `Listening`, `Transcribing`,
`ModelLoading`, `Error`, `NoPermission` get an explicit accent color;
`Idle`, `Injecting`, `DegradedOffline` stay untinted (monochrome,
"invisible by default" per the module doc). The shape is always the
`mark::mark_in()` horn+waves geometry.

Windows has no template-image auto-recoloring equivalent (`NSImage`
`setTemplate`/tint), so the Windows tray must **pre-render one `HICON`
per distinct color the state machine can request** and swap the whole
icon, rather than tint one image live. Concretely, from `OverlayState::
ALL` (8 states, `overlay/src/state.rs:41`) via `glyph_tint()`:

- 5 states get an explicit RGB accent (identical values to
  `overlay::windows::accent()` at `windows.rs:98-108`, which already
  defines this palette for the floating overlay — reuse that function,
  don't reinvent a second palette): `Listening` green, `Transcribing`
  amber, `Error`/`NoPermission` red, `ModelLoading` blue.
- 3 states (`Idle`, `Injecting`, `DegradedOffline`) render untinted.
  macOS resolves untinted color from the *live* menu-bar appearance
  (`is_dark_menu_bar()`, `status_item.rs:153-164`) precisely because a
  fixed color like `labelColor` silently rendered wrong after a
  light/dark switch — status_item.rs's own comments (222-224) call this
  out as a bug they already hit once ("same trap family as 64af502").
  **The Windows taskbar has the identical trap**: it can be light or
  dark independent of the app's own theme. Read
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\
  SystemUsesLightTheme` (`RegGetValueW`, needs the `Win32_System_
  Registry` feature) to pick white-on-dark vs near-black-on-light, exactly
  mirroring `mark_image()`'s color branch (`status_item.rs:224-230`).
  Re-check on `WM_SETTINGCHANGE`/`WM_THEMECHANGED` (the message, not a
  poll-per-frame — cheaper and matches how the message loop already
  receives window messages).

Total: up to 5 fixed-color icons + 2 theme-variant icons (untinted-light,
untinted-dark) = 7 `HICON`s, built once at tray startup and rebuilt only
on a theme-change message. Cache in a small `HashMap` or a fixed array
keyed by `(OverlayState, is_dark)`, mirroring `MacStatusItem`'s
`applied`/`applied_dark` change-detection fields (`status_item.rs:96-102`)
so `apply()` is a no-op on unchanged frames — the model equality check
already exists for the menu (`MenuModel: PartialEq`, `menu.rs:83`), reuse
it for the icon skip-redraw decision too.

**Where the icons come from**: rendered, not shipped as files (there is
no `.ico` in the repo — confirmed, `find . -iname '*.ico'` finds nothing).
Same GDI technique `overlay::windows::WinOverlay::paint()` already uses
(`windows.rs:198-346`): `CreateCompatibleDC` + `CreateDIBSection` (32bpp),
draw `mark::mark_in(size).horn` as a filled `Polygon()` and each `.waves`
entry as a stroked polyline with round caps (`ExtCreatePen` with
`PS_ENDCAP_ROUND`, since plain `CreatePen` has no cap-style control —
`windows.rs`'s `RoundRect`/`FillRect` calls don't need this, but the tray
icon's stroked arcs do, same requirement `status_item.rs:263`
(`NSLineCapStyle::Round`) states for the AppKit path). Icon size:
`GetSystemMetrics(SM_CXSMICON)` (usually 16px, DPI-aware) rather than a
hardcoded 16 — the tray genuinely does DPI-scale on high-DPI displays,
unlike a fixed toolbar bitmap. Wrap the finished DIB as an icon with
`CreateIconIndirect(&ICONINFO { fIcon: true, hbmColor: <the DIB>, hbmMask:
<a same-size all-black 1bpp mask> })` — modern Windows honors the 32bpp
DIB's real alpha channel for `hbmColor` regardless of the mask, but
`CreateIconIndirect` still requires *some* mask bitmap to be supplied.
`DestroyIcon` every icon on tray teardown and on rebuild — `HICON`s are
GDI objects with the same "leaked forever if you forget" property the
`WinOverlay::paint()` comments already warn about for DCs/bitmaps
(`windows.rs:335-341`).

## 4. Console removal sequencing

`#![windows_subsystem = "windows"]` is not safe to add today, for a
concrete and testable reason, not a vague caution:

**With no console attached, `println!`/`eprintln!` panic, they do not
silently no-op.** `GetStdHandle(STD_OUTPUT_HANDLE)` returns an invalid
handle when a GUI-subsystem process has no console; Rust's
`Stdout::write_fmt` unwraps that write, so the *first* `println!`/
`eprintln!` call anywhere in the process becomes an unhandled panic —
which, again with no console, is **also invisible** (no backtrace goes
anywhere a user can see). This is strictly worse than today's console
window, and it is exactly the failure mode the task description already
points at ("the console is currently the ONLY place a Windows user sees
errors") — removing it without a replacement doesn't just lose that
channel, it turns every remaining print into a silent crash.

Concrete count of what's reachable on the resident-daemon path today
(not exhaustive, but the shape of the audit): `main.rs` has print/eprintln
in the args parser (`--permissions`, `--version`), the single-instance
refusal path (`report_refusal_to_the_user`, called from `main.rs:344-367`
before any tray exists), and the overlay/pipeline error paths
(`main.rs:589-591`, `708-712`, etc). `menuhost.rs` has several
(`config could not save`, diagnostics write failure, `open` spawn
failure). Every one of these needs a decision before the console goes
away.

Required order:

1. **Build the tray (§2-3) first.** Most of `menubar::Status` (config
   problems, permission blocks, mic device, bound-hotkey failure) already
   has a surface — the menu itself. This is the majority of what stderr
   currently carries at runtime, and it needs no new plumbing once the
   tray exists.
2. **Add a log file** beside `config.toml` (same directory
   `run_diagnostics()` already writes `diagnostics.txt` into,
   `menuhost.rs:432-439`) and redirect every remaining `eprintln!` on the
   resident-daemon path to append there instead of (or in addition to)
   stderr. This covers errors that happen *before* the tray exists yet
   (single-instance refusal) or that aren't state the menu model
   represents (a spawn failure for `open`).
3. **Add a global panic hook** (`std::panic::set_hook`) that writes the
   panic message + location to that same log file. Without this, a panic
   after the console is gone is not just silent about its *message* — the
   whole process dies with zero trace, and "OutLoud just vanished" is a
   worse bug report than "there's an ugly console window."
4. **Dual-mode console attach for CLI flags.** `--once`, `--permissions`,
   `--version`/`-V` are utility invocations a user runs *from a shell* and
   expects to actually see output from, right now, in that shell — not in
   a log file discovered later. Before doing anything else in `main()`,
   detect those flags and call `AttachConsole(ATTACH_PARENT_PROCESS)`
   (recovers the invoking cmd/PowerShell's console when launched from
   one) and fall back to `AllocConsole()` if that fails (covers being
   double-clicked with a flag, unlikely but shouldn't eat the output).
   This needs the `Win32_System_Console` feature on the `windows` crate
   dependency (not currently enabled anywhere in the workspace).
5. **Only then add `#![windows_subsystem = "windows"]`.** Recommend
   gating it `#[cfg_attr(all(target_os = "windows", not(debug_assertions)),
   windows_subsystem = "windows")]` so `cargo run` during Windows
   development still gets a console by default without a flag — a
   deliberate ergonomics choice, not a requirement of the mechanism
   itself (a release-mode reader would want the flag unconditional; note
   the tradeoff rather than silently picking one).

Each of steps 2-4 is independently useful and independently testable
without touching subsystem at all, which is why they're staged before
step 5 rather than bundled with it.

## 5. Staging: smallest useful tray vs. full parity

| Stage | Scope | New/changed | Effort |
|---|---|---|---|
| **0 — Minimum viable tray** | Fixes the worst problem (no way to quit but Task Manager). One static icon (ignore state; reuse `mark::mark_in()` drawn once in a neutral color), a two-row menu: `Pause Dictation` / `Quit OutLoud`, wired straight into the existing `Action::Set`/`Action::Quit` via `MenuHost::handle`. No console change. | New `overlay/src/win_tray.rs` (message window + `Shell_NotifyIcon` add/delete + `TrackPopupMenu` for a *fixed* 2-item menu you build by hand, not yet from `menubar::build()`); rewrite `main.rs:775-812` to pump `GetMessage`/`DispatchMessage` and call `host.handle()`. | **~1-2 days.** No icon-set work, no theme detection, no menu-tree walker — just the Shell_NotifyIcon plumbing and one click path. |
| **1 — Full menu + state icons** | Everything `menubar::build()` produces (permission rows, settings submenu, sensitivity steps, hotkey presets, diagnostics, config file/vocab folder links) rendered as a real `HMENU` tree; all 8 `OverlayState`s reflected via the icon set in §3, including the theme-change watch. | Extend `win_tray.rs`: recursive `MenuItem` → `HMENU` builder (§2.4 table), icon cache + `RegGetValueW` theme read + `WM_SETTINGCHANGE` handling, tooltip truncation (§2.2), model-diff skip-redraw (mirroring `MacStatusItem::apply`). No changes needed to `menubar.rs` or `menuhost.rs` — they're already exercised as-is once this lands. | **~3-5 days.** Bulk of it is the icon rendering (GDI polygon/polyline + `CreateIconIndirect`) and getting `NOTIFYICON_VERSION_4` + the three-line `TrackPopupMenu` incantation right (§2.4) — both are "small code, easy to get subtly wrong" territory, worth budgeting real test time on real Windows hardware rather than assuming CI compilation proves correctness (per the README's own honesty about Windows being compiled-but-unexercised). |
| **2 — Console removal** | Everything in §4: log file, panic hook, dual-mode console attach for CLI flags, `windows_subsystem = "windows"`. | New small `outloud::winlog` (or similar) module; audit and redirect the `eprintln!`/`println!` call sites listed in §4; one crate-root attribute. Depends on Stage 1 existing as the replacement error surface — doing this before Stage 1 would make errors strictly harder to see, not easier. | **~2-3 days**, mostly the audit (finding every reachable print site) rather than the mechanism itself. |
| **3 — Parity extras (optional)** | `launch-at-login` is currently schema-valid but explicitly inert on every platform (`menuhost.rs:646-664` pins this via a doc-vs-code test). Windows' version of this is a two-line `HKCU\...\Run` registry write/delete — genuinely simpler than macOS's `SMAppService`, and worth doing here since the tray now exists to expose the toggle. Also: balloon notifications (`NIF_INFO`) for state changes, and a friendlier "already running" experience (toast instead of a bare refusal message) once the log-file/console work from Stage 2 is in. | Small, additive, no architecture changes. | **~1 day** for `launch-at-login`; the rest is nice-to-have and not blocking parity. |

Recommended order: **0 → 1 → 2**, with 3 picked up opportunistically.
Stage 0 alone converts "the app is unquittable and invisible" into "the
app has a real tray icon," which is the single highest-value fix in this
whole plan and ships in isolation without touching the console or the
icon-per-state work at all.

## Cargo changes needed (both stages 0-1)

`crates/overlay/Cargo.toml`, `[target.'cfg(target_os = "windows")'.
dependencies].windows` (currently `Win32_Foundation`,
`Win32_Graphics_Gdi`, `Win32_UI_HiDpi`, `Win32_UI_WindowsAndMessaging`):
add `Win32_UI_Shell` (Stage 0, for `Shell_NotifyIconW`/`NOTIFYICONDATAW`).
Add `Win32_System_Registry` (Stage 1, for the taskbar theme read). For
Stage 2, `crates/outloud/Cargo.toml`'s `windows` dependency (currently
`Win32_Foundation`, `Win32_System_Threading` only) needs
`Win32_System_Console` (`AttachConsole`/`AllocConsole`).
