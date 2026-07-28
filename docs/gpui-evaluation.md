# GPUI / gpui-component Evaluation for the Outloud Overlay

**Date:** 2026-07-28
**Spike:** `/tmp/gpui-spike` (gpui 0.2.2 from crates.io, `WindowKind::PopUp`)
**Status:** Complete. This closes the investigation started earlier; the previous
agent built the spike but never wrote a verdict.

## Recommendation: NO — do not adopt GPUI for the overlay

GPUI *passes* the decisive non-activating test: `WindowKind::PopUp` creates a
genuine `NSPanel` with `NSWindowStyleMaskNonactivatingPanel`, and empirically the
spike window does not steal focus while the user types into another app. So GPUI
is not disqualified on the core requirement. It is disqualified on everything
else that matters *now*: it demands ownership of `NSApplication` and the run
loop (no embed path for a windowed app), it force-sets
`NSApplicationActivationPolicyRegular` at launch (a Dock icon, which our
menu-bar-only Accessory app must not have), and it adds ~600 dependencies,
gigabytes of target size, and a second UI paradigm to a codebase whose overlay
redesign is already shipping in objc2 + CoreAnimation. There is not enough left
on the table to justify replacing something that already works. Revisit only if
we ever build a *separate*, GPUI-owned windowed app (e.g. a settings or history
window as its own process).

---

## 1. The decisive question: non-activating window

### Source evidence (gpui 0.2.2, `src/platform/mac/window.rs`)

All paths below are in
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-0.2.2/`.

- Line 63: `const NSWindowStyleMaskNonactivatingPanel: NSWindowStyleMask = ...`
- Line 109: `PANEL_CLASS = build_window_class("GPUIPanel", class!(NSPanel));`
  — PopUp windows are a real `NSPanel` subclass, not an `NSWindow`.
- Lines 621–625: window allocation branches on kind:

  ```rust
  let native_window: id = match kind {
      WindowKind::Normal | WindowKind::Floating => msg_send![WINDOW_CLASS, alloc],
      WindowKind::PopUp => {
          style_mask |= NSWindowStyleMaskNonactivatingPanel;
          msg_send![PANEL_CLASS, alloc]
      }
  };
  ```

- Lines 790–810 (PopUp branch): sets `NSPopUpWindowLevel` (101), an
  always-active tracking area for mouse-moved events without focus, and
  collection behavior `CanJoinAllSpaces | FullScreenAuxiliary` — the same
  recipe as our hand-rolled panel in `crates/overlay/src/macos.rs`.
- Lines 849–852: with `focus: false, show: true` in `WindowOptions`, GPUI calls
  `orderFront:` (not `makeKeyAndOrderFront:`), so showing the window does not
  key it.

One caveat found in source: line 291 — the shared window/panel class answers
YES to `canBecomeKeyWindow`. Our own panel hard-returns NO. With the
nonactivating style mask and `orderFront:` this doesn't matter for *display*
(verified below), but it means a stray click on the GPUI overlay could make the
panel key within our app, whereas our panel refuses key status entirely. That
is a small but real behavioral difference in GPUI's favor of interactivity and
against our "never interferes" guarantee.

### Empirical proof

Procedure: opened TextEdit with an empty document, launched the spike (its
translucent PopUp window visible on screen at all times), then sent keystrokes
via System Events and read the document back, all while `pgrep` confirmed the
spike was running.

- Frontmost process after spike launch: **TextEdit** (never gpui-spike).
- Typed "second try" while the spike window was visible → TextEdit document
  contained exactly `second try`. Keystrokes were not swallowed.
- Repeated with a fresh document and a longer sentence: same result, keystrokes
  landed in TextEdit; frontmost app never changed to the spike.
- Killing the spike changed nothing about where text landed, confirming the
  spike was not intercepting input while alive.

**Verdict on the decisive question: PASS.** GPUI's PopUp window does not
activate, does not take focus, and does not swallow keystrokes destined for the
frontmost app. If this had failed, nothing else would matter. It didn't, so the
decision rests on the items below.

## 2. Coexistence with our existing NSApplication + NSStatusItem: FAIL

This is the actual disqualifier. From `src/platform/mac/platform.rs`:

- `MacPlatform::run()` (line 474) calls `[NSApplication sharedApplication]`,
  installs **its own app delegate** (`app.setDelegate_(app_delegate)`), and
  calls `app.run()`. GPUI owns the run loop and the delegate, full stop.
- `did_finish_launching` (line 1387) unconditionally calls
  `app.setActivationPolicy_(NSApplicationActivationPolicyRegular)`. Our app is
  an Accessory (menu-bar-only) app — `crates/overlay` sets
  `NSApplicationActivationPolicy::Accessory` in every binary. GPUI would give
  us a Dock icon and a Cmd-Tab entry with no supported way to opt out.
- The only alternative entry point is `App::headless()` (`src/app.rs` line
  146), which by its own doc comment "prevents opening windows" — useless for
  an overlay.

There is no embed path: no "attach GPUI to an existing NSApp/run loop" API in
0.2.2. Adopting GPUI for the overlay means either (a) letting GPUI own the app
and re-implementing our status item, hotkey handling, and activation policy
inside its world (its `status_item.rs` exists but is Zed-shaped), or (b)
running the overlay as a separate helper process with IPC. Both are large
architectural changes to buy a window we already have.

## 3. Licences: OK

- `gpui` 0.2.2: **Apache-2.0** (`license = "Apache-2.0"` in its Cargo.toml,
  LICENSE-APACHE shipped in the crate). Note the *Zed application* is
  AGPL/GPL-licensed, but the gpui crate itself is Apache-2.0 — the framework
  was deliberately carved out.
- `gpui-component` 0.5.1: **Apache-2.0** (per crates.io metadata).
- Our `deny.toml` licence allow-list already includes Apache-2.0. Static
  linking Apache-2.0 into an MIT binary is fine (keep NOTICE/attribution).
  **No conflict.**

## 4. crates.io vs git: OK (recently)

`gpui` is now published on crates.io (0.2.2, resolved from
`registry+https://github.com/rust-lang/crates.io-index` in the spike's
Cargo.lock), as is `gpui-component` (0.5.1). Until mid-2025 gpui was git-only,
which would have violated our `unknown-git = "deny"` policy outright; that
conflict no longer exists. Two of its transitive deps are Zed-forks published
as crates (`zed-xim`, `zed-scap`) — on-registry, so policy-clean, but a hint
that the dependency tree is Zed's world, not a neutral library ecosystem.

## 5. Build cost: heavy

- **Dependencies:** the spike's Cargo.lock contains **704 packages** for a
  40-line hello-world overlay. Includes wgpu-adjacent GPU stack, cosmic-text,
  wayland/x11 code, zbus, screencapturekit, etc.
- **Target size:** spike `target/` is **2.5 GB** (2.2 GB in debug) for that one
  window. Our workspace target already hit 11 GB before cleaning; this adds
  multiple GB on top and a large cold-build (the original spike build was a
  full from-scratch compile of those 704 crates).
- **Incremental:** touching the spike's `main.rs` rebuilds in ~2.0 s wall.
  Tolerable, but roughly double our current ~1 s incremental loop, in the best
  case where nothing in the 704-crate graph changed.
- **Binary:** spike debug binary is 23 MB before linking into anything.

## 6. Weighing it against what already exists

The honest frame, per the current state of the repo: the question is no longer
"what should we build the overlay in" but "is there enough left on the table to
justify replacing something that already works". While this evaluation sat
open, the team designed the overlay redesign (`docs/overlay-redesign.md`),
prototyped it in `crates/overlay/src/bin/overlay-proto.rs` in objc2 +
CoreAnimation, and a sibling swarm is shipping it now. Our hand-rolled panel
already does exactly what GPUI's PopUp does (same style mask, same window-level
idea, stricter `canBecomeKey = NO`), with zero framework dependencies, an
Accessory activation policy GPUI cannot express, and full ownership of our own
run loop.

What GPUI would genuinely buy us — a retained-mode element tree, flexbox
layout, GPU text — is nice-to-have polish for an overlay measured in one small
panel, purchased at the price of an app-architecture rewrite, ~700 deps,
gigabytes of build artifacts, and throwing away work that is mid-ship. That
trade fails.

### When to reopen

- We build a standalone windowed surface (settings, history browser) that can
  be its own process and own its own NSApp — GPUI is a strong fit there.
- GPUI grows a documented embed/attach API for host-owned NSApplications and a
  way to keep Accessory activation policy.
