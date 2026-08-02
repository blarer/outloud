# Windows overlay: bringing it to macOS visual parity

Status: plan only, nothing implemented. Written against `crates/overlay` as of
this commit. All line numbers below were read directly from the files named,
not estimated.

## 0. The one-sentence summary

Almost all of the macOS overlay's *content* — skull geometry, animation,
theme, text-lane model, positioning math — is already platform-neutral Rust
in `crates/overlay/src/{skull,layout,theme,text_window,mark}.rs`. `macos.rs`
(886 lines) is a thin rendering adapter over that content using AppKit/GDI-
equivalent calls. `windows.rs` (404 lines) implements the *window plumbing*
correctly (click-through, non-activating, layered, topmost) but never calls
into `skull`, `text_window`, or `theme::accent` for anything beyond one dot
color — it draws a flat rounded-rect card with GDI primitives instead. The
work is a **from-scratch Windows renderer that consumes the existing shared
model**, not a redesign, and not a port of `macos.rs`'s AppKit calls (those
don't exist on Windows; Direct2D/DirectWrite are the correct analog and are
already available in the `windows` crate this project depends on).

---

## 1. What macOS actually draws, feature by feature, and where it lives

Reading `macos.rs` top to bottom against `skull.rs`/`layout.rs`/`theme.rs`/
`text_window.rs`:

| Feature | Computed in (shared, no AppKit) | Drawn in `macos.rs` (AppKit-specific) |
|---|---|---|
| Skull silhouette (cranium, jaw, eye sockets, eye glow, nose, teeth) as posed polygons | `skull::posed_geometry` (skull.rs:228-288), driven by `SkullAnimator::step` (skull.rs:380-465) | `draw_skull` (macos.rs:496-578): maps unit-square points into the 42pt `SKULL_*` box (`poly_path`, macos.rs:300-314) and fills with `NSBezierPath` |
| Depth / one light source (lit-top, shaded-underside gradient per bone piece; drop shadow on the whole skull; inset shadow on eye sockets) | Nothing — `LIGHT_ELEVATION` (macos.rs:291) and the whole depth model are AppKit-only | `fill_poly_lit` (macos.rs:334-353, `NSGradient` at 90°×`LIGHT_ELEVATION`), `with_drop_shadow` (macos.rs:361-375, `NSShadow`) |
| Eye glow / gaze aura (20-ring Gaussian falloff behind the skull, radius and alpha tied to `pose.eye_glow`) | `SkullPose.eye_glow` value (skull.rs:186-187, computed in `eye_glow()` skull.rs:478-500) | The 20 concentric `NSBezierPath` ovals themselves (macos.rs:503-517) — pure AppKit, no shared geometry exists for "a ring of circles" |
| Per-word birth stagger (55ms cascade) | `TextWindow::ingest` (`bloom_at = now + born * BIRTH_STAGGER`, text_window.rs:234), `BIRTH_STAGGER = 0.055` (layout.rs:229) | Nothing platform-specific — macos.rs just calls `model.words.ingest` (macos.rs:833) and reads `.opacity`/`.x` off already-staggered slots (`draw_words`, macos.rs:583-604) |
| Partial text (rolling lane, commit-horizon white/tinted split, position glide, stale group-decay) | Entirely in `text_window.rs` (`TextWindow`, `TextSlot`) | `draw_words` (macos.rs:583-604): `NSString::drawAtPoint_withAttributes`, `NSFont::systemFontOfSize`, color chosen from `w.committed` |
| State colors (accent per `OverlayState`) | `theme::accent` (theme.rs:229-245) — plain `Color` struct, no AppKit | `ns_color()` (macos.rs:277-279) converts `theme::Color` → `NSColor` at every call site |
| Drop shadow (of the whole skull silhouette, cast once not per-polygon) | Nothing — shadow itself has no shared representation, only the *idea* is documented (macos.rs:536-543) | `with_drop_shadow` (macos.rs:361-375), `NSShadow` |
| Entry animation (skull scales 0.72→1.0 + fades in over 150ms on hotkey-down) | `SkullAnimator::trigger_entry` / `entry_phase` (skull.rs:363-376, 437-455) | Nothing platform-specific — the pose's `scale`/`opacity` fields already carry it; macos.rs just applies them (macos.rs:527, 531-534, 570) |
| Settle pop (damped-spring bounce on utterance commit) | `SkullAnimator::trigger_settle`, damped cosine spring (skull.rs:359-361, 424-435) | Nothing platform-specific, same as above |
| Blink | `blink_openness` (skull.rs:505-517), deterministic hashed schedule | Nothing platform-specific |
| Breathing / idle sway | `step()` (skull.rs:425, 462) | Nothing platform-specific |
| Reduce Motion | `SkullAnimator::step(reduce_motion: bool)` branch (skull.rs:395-421), `TextWindow::step(reduce_motion)` branch (text_window.rs:332-352) | `reduce_motion()` (macos.rs:413-418) queries `NSWorkspace.accessibilityDisplayShouldReduceMotion` via `msg_send!` — **this one query is the only OS-specific input the shared logic needs**, everything downstream of the bool is shared |
| Animation clock (drives `step()` every frame, invalidated when hidden) | Nothing — pure host-loop concern | `CADisplayLink` w/ `NSTimer` fallback (macos.rs:626-782); ties to real vsync |
| Panel placement (bottom-center of active screen, clears the Dock) | `layout::place_bottom_center` exists and is shared (layout.rs:173-184) but **macos.rs does not call it** — it inlines the same math directly against `NSScreen.visibleFrame` (macos.rs:708-715) | `place_panel` (macos.rs:705-715) |
| Panel geometry constants (760×230pt, skull box, glow box, anchor X) | Nothing shared — `PANEL_SIZE`, `SKULL_X/Y`, `GLOW_CX/CY/R`, `ANCHOR_X`, `LANE_Y` are all local `const`s in macos.rs (macos.rs:80-112) | Same |
| Text measurement (`sizeWithAttributes:`) | `TextWindow` takes measurement as an injected `&mut dyn FnMut(&str) -> f64` closure (text_window.rs:189) — **already backend-agnostic by design** | `measure()` (macos.rs:380-389) is the AppKit implementation of that closure |

**Bottom line on "what's shared vs. what's macOS-only":** skull *pose* math,
text-lane *model*, theme *values*, and layout *placement policy* are 100%
shared. The things with zero shared representation are: the depth/lighting
recipe (gradient direction + shadow blur/offset/alpha as a set of numbers),
the panel's own geometry constants (`PANEL_SIZE` etc., currently duplicated
as private `const`s rather than living in `layout.rs` next to `OVERLAY_SIZE`),
and the glow-ring rendering technique itself (though its *inputs* — radius,
alpha, gain — are derivable from `pose.eye_glow` and `theme::accent`).

Windows' current file draws **none** of this: `windows.rs` calls
`theme::accent` equivalent logic by hand-duplicating a `fn accent()`
(windows.rs:98-107, note this literally re-implements `theme::accent` with
different colors — Listening is green here, `COBALT` blue in theme.rs — a
drift bug already present today, not hypothetical) and otherwise draws a
flat rounded rect (windows.rs:220-231), one accent dot via `RoundRect`
(windows.rs:233-239), a level-meter bar (windows.rs:241-256), and GDI
`TextOutW` text (windows.rs:258-293). No skull, no words, no theme reuse.

---

## 2. What Windows needs, API by API

### 2.1 Rendering: replace GDI with Direct2D + DirectWrite

GDI (`RoundRect`, `FillRect`, `TextOutW`) cannot do: alpha-blended polygon
fills with per-vertex color (needed for `fill_poly_lit`'s gradient), radial
gradients (glow rings), soft/blurred shadows, or antialiased curve fills at
the quality the skull needs at 42pt. Direct2D (`ID2D1Factory`,
`ID2D1DCRenderTarget` or `ID2D1HwndRenderTarget`) is the direct Win32 analog
of what AppKit's `NSBezierPath`/`NSGradient`/`NSShadow`/`CGContext` give
macOS, and DirectWrite (`IDWriteFactory`, `IDWriteTextLayout`) is the analog
of `NSString`/`NSFont` for measuring and drawing the text lane.

Concretely, from the `windows` crate (v0.62, already a workspace dependency,
confirmed present via `cargo check --target x86_64-pc-windows-msvc`):

- `windows::Win32::Graphics::Direct2D` — feature flag `Win32_Graphics_Direct2D`
  (confirmed present in the crate's `Cargo.toml`, not yet enabled in
  `crates/overlay/Cargo.toml`'s Windows feature list).
- `windows::Win32::Graphics::Direct2D::Common` — feature
  `Win32_Graphics_Direct2D_Common` (for `D2D1_COLOR_F`, `D2D_POINT_2F`, etc).
- `windows::Win32::Graphics::DirectWrite` — feature `Win32_Graphics_DirectWrite`.
- `windows::Win32::Graphics::Dxgi::Common` — feature
  `Win32_Graphics_Dxgi_Common` (pixel format enums Direct2D wants).

None of these were checked before this plan; I confirmed by grepping the
vendored `windows-0.62.2` crate's `Cargo.toml` for the feature names, and
they exist verbatim.

**Why not stay on GDI and just draw more shapes with it?** `fill_poly` in
macos.rs is a filled arbitrary polygon (`NSBezierPath`, moveTo/lineTo/close)
— GDI's `Polygon()` function *can* do this, alpha-blended fills cannot
(GDI has no native alpha channel; that's why `windows.rs` currently
premultiplies manually in `pixel.rs` after the fact for the *whole layered
window*, not per-shape). A skull with translucent eye glow, gradient bone
shading, and a soft drop shadow needs per-primitive alpha and blur, which is
architecturally what Direct2D is for and what GDI is not. Doing it in GDI
would mean re-deriving Direct2D's blend/blur math by hand into a manually-
premultiplied DIB — possible (the level meter and rounded rect already prove
GDI-into-a-layered-window works) but it is reinventing a compositor.

**How rendering into the existing `UpdateLayeredWindow` pipeline changes:**
the current design paints into a GDI-backed DIB section
(`CreateDIBSection`, windows.rs:216-217) then calls `UpdateLayeredWindow`
with that DC. Direct2D has a render target built exactly for this case:
`ID2D1Factory::CreateDCRenderTarget` → `ID2D1DCRenderTarget::BindDC(hdc,
rect)`, bound to the *same* memory DC currently created by
`CreateCompatibleDC`/`CreateDIBSection`. So the window-plumbing half of
`windows.rs` (lines 109-347's DC/bitmap/`UpdateLayeredWindow` machinery)
barely changes — swap what draws *into* the DC from raw GDI calls to
`ID2D1DCRenderTarget::BeginDraw`/`FillGeometry`/`EndDraw`. The manual
`premultiply()` alpha-fixup loop (windows.rs:301-306, `pixel.rs`) very likely
still applies: Direct2D's DC render target target format is
`DXGI_FORMAT_B8G8R8A8_UNORM` with `D2D1_ALPHA_MODE_PREMULTIPLIED`, and it can
be told to render already-premultiplied, which would let this fixup step be
deleted — worth verifying with a spike rather than assuming (see Stage 2
below), since getting it wrong produces exactly the "invisible or
black-boxed overlay" failure mode `pixel.rs`'s own doc comment warns about.

### 2.2 Never stealing focus (the load-bearing constraint)

Nothing here changes. `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED
| WS_EX_TOPMOST | WS_EX_TOOLWINDOW` (windows.rs:141-145) plus
`SW_SHOWNOACTIVATE` (windows.rs:365) are exactly right and are already
implemented, tested by the user dictating into Discord today per the task
brief. Direct2D drawing happens entirely inside `paint()`'s existing
CPU-side DC — it has no interaction with window activation, message
handling, or input at all, so swapping the renderer cannot regress this.
The one thing to keep verifying by hand on real hardware after every stage:
that showing/updating the layered window surface never triggers
`WM_ACTIVATE`/`WM_SETFOCUS`. `UpdateLayeredWindow` and `ShowWindow(SW_SHOW-
NOACTIVATE)` are documented not to, and this project already validated it
once; a regression test is not mechanically possible here (Windows CI
cannot exercise real focus semantics, per README's own "compiles, not
exercised" caveat), so this stays a manual hardware check per stage.

### 2.3 Skull geometry → Direct2D path

`skull::posed_geometry` returns `Vec<Point>` per part in the unit box.
Direct2D's analog of `NSBezierPath::moveTo/lineTo/close` + `fill` is
`ID2D1PathGeometry` built via `ID2D1GeometrySink` (`BeginFigure`,
`AddLine`, `EndFigure(D2D1_FIGURE_END_CLOSED)`, `Close()`), then
`ID2D1RenderTarget::FillGeometry(&geometry, &brush)`. This is a direct,
mechanical translation of `poly_path`/`fill_poly` (macos.rs:300-327) — same
shape, same closed-polygon convention `skull.rs`'s doc comment already
promises ("closed; first point NOT repeated; backends close the path").

### 2.4 Depth / lighting → Direct2D gradient + shadow effects

- `fill_poly_lit`'s two-color vertical gradient → `ID2D1LinearGradientBrush`
  (`CreateLinearGradientBrush`, with a `D2D1_GRADIENT_STOP` array of the
  same two colors, axis rotated to match `LIGHT_ELEVATION`).
- `with_drop_shadow`'s soft shadow → Direct2D has no direct `NSShadow`
  equivalent on a DC render target; the idiomatic path is
  `ID2D1Effect` (`CLSID_D2D1Shadow`) which needs a `ID2D1DeviceContext`
  (not just `ID2D1RenderTarget`), which in turn needs a `ID2D1Device` built
  from a `ID3D11Device` — a materially heavier setup than the DC render
  target alone. **This is the single biggest scope decision in this plan**
  (see Stage 3 below): either (a) pull in the Direct3D/Direct2D device
  chain to get real blurred shadows, or (b) fake the shadow cheaply by
  filling the same polygon path offset by a few px in near-black at low
  alpha *underneath* the lit fill — visually close, no blur, no extra
  device, and a direct GDI-era trick. Given `docs/overlay-performance.md`'s
  own conclusion that draw cost has huge headroom (0.73ms of an 8.33ms
  budget on macOS; Windows starts from a much simpler flat-rect draw so has
  even more room), (a) is affordable, but (b) ships stage 3 sooner and (a)
  can follow as a visual-quality pass. Recommendation: ship (b) first,
  revisit (a) only if a real screenshot comparison shows the offset-fill
  shadow reads as noticeably worse than macOS's blurred one.

### 2.5 Eye glow / aura rings → Direct2D radial gradient brush

macOS fakes a radial gradient with 20 concentric circles (macos.rs's own
comment at overlay-redesign.md:171-178 admits this is a `CAGradientLayer`
workaround for a missing dependency). Direct2D *does* have
`ID2D1RadialGradientBrush` natively (`CreateRadialGradientBrush`), so
Windows can do this in one `FillEllipse` call with a real radial gradient
instead of 20 draws — better than parity, and simpler code, not a
compromise.

### 2.6 Text lane → DirectWrite

- Measurement: `IDWriteFactory::CreateTextLayout` + `GetMetrics()` gives
  `width`, the same role as macOS's `measure()` (macos.rs:380-389). This is
  literally the injected closure `TextWindow::ingest` already expects
  (text_window.rs:189) — no change to `text_window.rs` needed, only a new
  `fn measure(text: &str) -> f64` implementation using DirectWrite.
- Drawing: `ID2D1RenderTarget::DrawText` (given an `IDWriteTextFormat`) or
  `DrawTextLayout` (if reusing the layout already built for measurement,
  avoiding double-shaping the same string). Font: `Segoe UI` already used
  in `windows.rs`'s `CreateFontW` call (windows.rs:282) — keep it, it's the
  system UI font and the closest Windows analog to San Francisco.
- Color / opacity per word: `w.committed` branch (macos.rs:592-596) ports
  unchanged; `theme::palette::PAPER`/`AQUA` are plain `Color` structs, just
  need a `d2d1_color(c: theme::Color) -> D2D1_COLOR_F` conversion function,
  the Direct2D analog of `ns_color()` (macos.rs:277-279).

### 2.7 Reduce Motion → Windows accessibility API

macOS asks `NSWorkspace.accessibilityDisplayShouldReduceMotion` once per
frame (macos.rs:413-418). Windows' equivalent signal is
`SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, ...)` — `FALSE` is the
closest analog to "reduce motion" (Windows has no single dedicated "reduce
motion" toggle the way macOS does; `SPI_GETCLIENTAREAANIMATION` is what
Windows' own shell honors for comparable purposes, e.g. window animations).
This is one `unsafe extern "system"` FFI call, same shape as the existing
`GetCursorPos`/`GetSystemMetrics` calls already in `windows.rs`
(windows.rs:60-65) — trivial to add, not researched further here because it
gates on being confirmed against a real Windows accessibility settings pane
during Stage 4, which this plan explicitly defers to real hardware.

### 2.8 Animation clock → Windows equivalent of `CADisplayLink`

macOS prefers `CADisplayLink` (vsync-timed), falls back to a 60Hz
`NSTimer` (macos.rs:717-772). Windows has no direct vsync-callback API
reachable without pulling in DXGI's `IDXGIOutput::WaitForVBlank` or a swap
chain — disproportionate for an indicator overlay. The pragmatic Windows
equivalent, and what the existing `overlay_main` loop in `main.rs` already
does (main.rs:793-811: a 33ms `std::thread::sleep` loop), is to keep that
loop but drive `SkullAnimator::step`/`TextWindow::step` from it directly,
the same way the loop currently calls `ov.render(&frame)` once per host
poll. Concretely: `WinOverlay` gains its own `last_tick`/`epoch` bookkeeping
(mirroring `Model` in macos.rs:116-163) and a `tick()` method the render
loop calls every iteration, not just on new frames — this is a change to
`main.rs`'s Windows `overlay_main` (main.rs:776-812), not just to
`windows.rs`. `SetTimer`/`WM_TIMER` is the "real" Win32 idiom and would let
the window's own message loop drive it, but there currently *is* no Windows
message loop (`wnd_proc` at windows.rs:76-83 only exists to satisfy
`RegisterClassW`, nothing pumps `GetMessage`/`DispatchMessage` for it) —
adding one is more surface area for a focus-stealing mistake than reusing
the sleep loop that's already proven safe. Recommendation: reuse the sleep
loop, tighten it to ~16ms (60Hz) once animation is live, matching the
`FALLBACK_HZ` macOS uses in its non-displaylink path (macos.rs:112).

---

## 3. Staging: smallest thing that renders a skull, up to full parity

### Stage 0 — Cargo plumbing (no visible change)
Add the four Direct2D/DirectWrite/Dxgi feature flags to
`crates/overlay/Cargo.toml`'s `[target.'cfg(target_os = "windows")'.dependencies]`
block (currently only `Win32_Foundation`, `Win32_Graphics_Gdi`,
`Win32_UI_HiDpi`, `Win32_UI_WindowsAndMessaging` — Cargo.toml lines
~85-90). Verify with `cargo check -p overlay --target x86_64-pc-windows-msvc`
(confirmed working today for the existing feature set; re-run after adding
features). Effort: **~30 minutes**. Risk: near zero, purely additive.

### Stage 1 — Skull renders, flat, no depth, no aura, no words
Replace `windows.rs::paint`'s GDI card/dot/meter/text block (windows.rs:
219-293) with: create a `ID2D1DCRenderTarget` bound to the existing
`mem_dc`, hold a `SkullAnimator` + `SkullPose` on `WinOverlay` (mirroring
`Model` in macos.rs but far smaller — no `TextWindow`, no `FrameStats` yet),
call `skull::posed_geometry`, build one `ID2D1PathGeometry` per part, fill
each with a **flat** `ID2D1SolidColorBrush` (no gradient, no shadow —
defer 2.4). Map unit-square points into a fixed box the same way
`poly_path` does (macos.rs:300-314), sized to fit inside the current
`OVERLAY_W=260, OVERLAY_H=48` footprint or (more likely, given the skull's
own 42×42pt box) grow the window to something skull-shaped — this is the
first place the current 260×48 "info card" footprint and the macOS panel's
760×230 orb-plus-lane footprint have to be reconciled; recommend adopting
macOS's `SKULL_SIZE=42` constant directly and picking a Windows panel size
in Stage 3 once text is back in the picture, rather than guessing early.

This alone answers "no skull, isn't there at all" — the user gets a jaw
that opens with mic level and eyes that glow with state color, both driven
by the exact same `skull::posed_geometry`/`theme::accent` macOS uses, just
undepth-shaded. Reduce Motion, blink, breathing, entry/settle pop all come
free at this stage because they're computed in `skull.rs`, not drawn — the
only new code is the polygon→Direct2D translation and wiring `step()` into
the render loop's tick.

Effort: **1-2 days**. This is the single highest-value, lowest-risk stage:
it is almost entirely mechanical translation of already-tested, already-
pure logic into a new drawing backend, with zero design decisions except
"what size is the Windows panel." What could go wrong: (a) `BindDC` /
render-target-to-DC interop with the existing `UpdateLayeredWindow` alpha
path is the one genuinely unverified piece — Direct2D's premultiplied-alpha
contract needs to be confirmed against `pixel.rs`'s manual premultiply step
by an actual screenshot on real Windows hardware, not by reading docs; (b)
coordinate system mismatch — Direct2D's DC render target is top-left-origin
DIPs by default matching `layout.rs`'s stated convention (layout.rs:4-9), so
this should be a non-issue, but it's exactly the kind of thing that reads
fine in code and renders upside-down on a real screen.

### Stage 2 — Depth and light
Add `fill_poly_lit`'s two-tone gradient (§2.4, `ID2D1LinearGradientBrush`)
and the cheap offset-fill shadow approximation (§2.4 option b). Verify the
premultiply/`BindDC` alpha question from Stage 1 is actually settled before
this, since gradients and semi-transparent shadow fills are far more
sensitive to a wrong alpha mode than a flat fill was (a flat opaque fill
can look right even with subtly wrong alpha; a soft translucent shadow
cannot). Effort: **1 day**, contingent on Stage 1's alpha question being
resolved cleanly; add half a day if it wasn't.

### Stage 3 — Eye glow aura + text lane + panel resize
- Radial gradient aura (§2.5): genuinely simpler than macOS's 20-circle
  trick, since Direct2D has a native radial gradient brush. ~half a day.
- `TextWindow` + DirectWrite (§2.6): wire `text_window::TextWindow` the same
  way `macos.rs::draw_words` does (macos.rs:583-604) — `ingest` on frame
  updates, `step` on tick, draw each visible slot with `DrawTextLayout`.
  This is where `WinOverlay` needs to grow the full `Model`-equivalent
  state macOS carries (words, animator, pose, detail, now/last_tick,
  reduce_motion) — effectively porting `Model`'s shape (macos.rs:116-163)
  minus the AppKit-specific `FrameStats`/display-link parts. ~1-2 days.
- Resize the Windows panel to accommodate skull + lane, matching the
  proportions `layout::OVERLAY_SIZE` documents (380×64, "a wide, low bar",
  layout.rs:95-103) or macOS's panel (760×230) — this is a product/design
  call, not an engineering one; recommend matching `OVERLAY_SIZE` since
  that constant is already declared shared-and-authoritative in its own doc
  comment (layout.rs:90-94) even though neither backend currently reads it
  (macos.rs hardcodes 760×230, windows.rs hardcodes 260×48 — this drift is
  called out as a known problem in §4 below).

Effort: **2-3 days** total for this stage.

### Stage 4 — Polish and platform-specific correctness passes
- Reduce Motion via `SPI_GETCLIENTAREAANIMATION` (§2.7), verified against a
  real Windows accessibility toggle. ~2 hours engineering, but needs actual
  Windows hardware to confirm the setting maps the way assumed.
- `FrameStats`-equivalent instrumentation (`OVERLAY_FRAMESTATS`), porting
  `docs/overlay-performance.md`'s measurement discipline to Windows rather
  than assuming performance transfers. This project's own stated norm is
  "measure, don't estimate" (docs/latency.md convention, referenced
  throughout this codebase) — a Windows performance doc analogous to
  `overlay-performance.md` should exist before calling this done. ~half a
  day plus whatever the actual numbers demand.
- Tighten the render-loop tick rate from 33ms to something nearer 16ms
  once there's real animation to drive (§2.8). Trivial, but re-verify CPU
  idle cost afterward — macOS's whole clock-invalidation trick
  (macos.rs:774-782, "hidden = zero cost") has no Windows equivalent yet;
  the sleep loop in `main.rs`'s Windows `overlay_main` currently runs at a
  constant rate regardless of visibility (main.rs:793-811 has no early-out
  for hidden state the way `hide()` does on macOS) — this is worth fixing
  in the same pass, since an idle daemon burning a wakeup every 16ms all
  day is exactly the "battery bug" macOS's own doc comments warn against
  (macos.rs:38-40).

Effort: **1-2 days**, gated on hardware access this session explicitly does
not have.

### Total honest estimate
**6-9 engineering days** across stages 0-4, assuming no surprises in the
`BindDC`/premultiply interop (the one real unknown) and assuming access to
real Windows hardware for stages 2 onward (stage 1's skull-renders-flat
milestone can plausibly be sanity-checked purely by `cargo check` plus
reading the Direct2D calls against documentation, but *looks right* is not
the same bar as *is right* for a compositing/alpha bug — this plan does not
claim stage 1 is verifiable without a screen).

---

## 4. What must be shared, not duplicated — the actual risk in this plan

The task brief's framing is exactly right: *"A second 886-line platform
file that drifts from the first is a worse outcome than a plainer overlay."*
Concretely, watching for this means:

1. **`theme::accent` must be the only source of per-state color.**
   `windows.rs`'s current `fn accent()` (windows.rs:98-107) is *already* a
   drift bug today — Listening is green there, `COBALT` (blue) in
   `theme.rs`. Any new Windows renderer must delete that function and call
   `theme::accent` directly, converting `theme::Color` → `D2D1_COLOR_F` at
   the call site the same way `ns_color()` does for `NSColor`
   (macos.rs:277-279). This is a one-line fix that should land in Stage 0
   or 1, independent of everything else in this plan — worth doing
   immediately regardless of how the rest proceeds.
2. **Panel geometry constants should move into `layout.rs`, not be
   redeclared per-backend.** Today `PANEL_SIZE`/`SKULL_X`/`SKULL_Y`/
   `GLOW_CX`/`GLOW_CY`/`GLOW_R`/`ANCHOR_X`/`LANE_Y`/`WORD_FONT` are all
   private `const`s inside `macos.rs` (macos.rs:80-112), even though
   `layout.rs` already declares (and documents as authoritative)
   `OVERLAY_SIZE` (layout.rs:100-103) that neither backend actually reads.
   Before or during Stage 3, promote the panel-layout constants that both
   backends need identically (skull box position/size relative to panel,
   lane anchor, font sizes) into `layout.rs` or a new small module, and
   have both `macos.rs` and the new Windows renderer read the same
   constants. This is exactly the kind of fix the theme/skull/layout
   modules were already designed to prevent, and it's currently only
   half-applied.
3. **`SkullAnimator`, `SkullPose`, `TextWindow`, `TextSlot` must not grow
   Windows-specific fields or forks.** Both backends should hold one of
   each and call the same `step`/`ingest` methods; the *only* new code per
   backend should be geometry-to-drawing-API translation and the
   OS-specific "what is reduce motion" query. If a Windows-only field ever
   needs to live on `SkullPose` or `TextSlot`, that's a signal the
   abstraction boundary was drawn wrong and needs a second look before
   proceeding, not an invitation to special-case it.
4. **A single conversion function per value type, not scattered inline
   conversions.** macOS has exactly one `ns_color()` (macos.rs:277-279) as
   the sole `theme::Color → NSColor` conversion point. The Windows
   renderer should have exactly one `d2d1_color()` doing the equivalent,
   not `D2D1_COLOR_F { r: c.r as f32, ... }` written out at every call site
   — the existing `colorref()` in `windows.rs` (windows.rs:92-94) already
   demonstrates the right pattern, just for the wrong (GDI) color type;
   replace it with a Direct2D-flavored equivalent rather than keeping both.
5. **No new "is this the mac or the windows overlay" branches inside
   `skull.rs`/`theme.rs`/`text_window.rs`/`layout.rs`.** Their entire value
   is that they compile and are unit-tested with zero knowledge of which
   OS will render them (`lib.rs`'s own module docs are explicit about this,
   lib.rs:31-34, 40-49). Any change to those files driven by "but Windows
   needs X" should be interrogated hard: does macOS actually not need X
   too (in which case it belongs in a backend), or is it genuinely a new
   shared capability (in which case both backends should gain a test for
   it, the way `skull.rs`'s own test suite already covers reduce-motion,
   entry, settle, blink determinism, etc.)?

---

## 5. What could go wrong, beyond what's flagged inline above

- **Direct2D device-context requirements may force a heavier setup than
  hoped.** `ID2D1DCRenderTarget` (DC-bound, software/WARP-eligible, no HWND
  needed) should be sufficient for solid fills and linear/radial gradients
  — those don't need a full `ID2D1DeviceContext`/D3D device chain. Real
  shadow *effects* (`CLSID_D2D1Shadow`) do need that heavier chain, which
  is exactly why §2.4 recommends faking the shadow cheaply rather than
  pulling in Direct3D for one soft-shadow effect. If Stage 2 discovers the
  cheap fake looks noticeably worse than expected, the fallback is scoping
  down further (flat skull, no shadow at all) rather than escalating to a
  full D3D device — a shadow-less skull with correct color/motion is still
  a large improvement over "no skull at all," and matches the plan's own
  smallest-first philosophy.
- **DPI/DIP interactions between GDI's device-pixel world and Direct2D's
  DIP world.** `windows.rs` already handles DPI awareness for window
  *placement* (`SetProcessDpiAwarenessContext`, windows.rs:122-127) but the
  render target itself will need its own DPI set correctly
  (`ID2D1RenderTarget::SetDpi`) or geometry will be sized wrong on any
  monitor that isn't 100% scale — this is a real, previously-seen failure
  class in this exact codebase (the module doc for `windows.rs` calls out
  the DPI trap explicitly, windows.rs:23-33) and should get a specific
  manual test on a scaled monitor, not just an unscaled one, before calling
  any stage done.
- **No Windows hardware in this research session.** Every claim about
  Direct2D API shape above is from documentation and the vendored crate's
  type signatures, not from having compiled and run it on a Windows box.
  `cargo check --target x86_64-pc-windows-msvc` (confirmed working for the
  crate as it stands today) will catch type errors but nothing about
  whether the alpha, DPI, or `BindDC` behavior is actually correct on
  screen — that gap is real and this plan does not pretend otherwise. The
  README's own framing ("compiles in CI, unexercised on hardware") applies
  doubly to a new rendering path.
- **Panel size/position mismatch is a product decision hiding inside an
  engineering plan.** macOS's panel is a 760×230 orb-plus-lane pinned
  bottom-center; Windows' current panel is a 260×48 info card positioned by
  `layout::place` near the caret/cursor (windows.rs:358, using `Anchor`).
  These are two different placement *philosophies*
  (`layout.rs`'s own doc explains the tradeoff, layout.rs:158-172:
  bottom-center vs. near-caret). Stage 3's panel resize should not
  silently also change *where* the Windows overlay appears — that's a
  separate decision the user should confirm, not a side effect of adding a
  skull.

---

## 6. Immediate next step, if this plan is approved

Land the `theme::accent` deduplication (§4.1) on its own, first, regardless
of everything else — it is a real bug today, it's a five-line fix, and it
needs none of the Direct2D work to be worth doing.
