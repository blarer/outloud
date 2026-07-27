# Global hotkeys: platform traps and capabilities

The hotkey is the product's front door: hold to speak, release to commit,
tap to latch (`crates/hotkey`). Every trap below was either hit while
building the crate or is a documented failure mode of shipping dictation
tools. The theme throughout: **a silently dead hotkey is indistinguishable
from a broken product**, so every mechanism here is chosen to fail loudly.

## How the macOS backend works, and why

`HotkeyManager::bind` compiles the chord into a matcher, runs conflict
detection, then starts a **CGEventTap** at `kCGHIDEventTap` on a dedicated
thread, listen-only. Down/up edges feed a pure tap-vs-hold state machine
(default threshold 300ms, configurable via `Timing`), which emits
`Pressed` / `Released` / `Latched` / `Unlatched` over a channel.

**Why CGEventTap and not `NSEvent.addGlobalMonitorForEvents`:** push-to-talk
needs both edges of arbitrary keys, including bare modifiers and Fn. The
NSEvent global monitor cannot see the Fn key as a key at all and gives no
reliable device-level (left/right) modifier information. The event tap sees
the raw stream: every `keyDown`, `keyUp`, and `flagsChanged`, with full
`CGEventFlags`. Its cost, Accessibility permission, is one this product
already pays for text editing (`docs/macos-permissions.md`), so the tap is
free here.

## Trap list

### 1. The Fn/Globe key is not a key

Aqua Voice's default binding, and the most trap-dense key on the board:

- It never produces `keyDown`/`keyUp`. It arrives as `flagsChanged` with
  keycode 63 and the `NX_SECONDARYFNMASK` bit (0x800000) indicating
  direction. Push-to-talk edges must be reconstructed from flag transitions.
- **Matching on the flag alone is wrong**: arrow keys, F-keys, and the nav
  cluster all carry the fn bit in their own events (observed: synthetic F13
  arrives with flags `0x20800000`). The matcher requires keycode 63 for the
  fn binding, and *masks the fn bit out* when matching F-key chords, or
  every `f13` binding would be unmatchable.
- **The system claims the press.** System Settings > Keyboard > "Press 🌐
  key to" defaults to Change Input Source / Emoji / Dictation. A tap-latch
  gesture on Fn will also trigger that action. Conflict detection flags a
  bare-fn binding unconditionally and names the setting to change.
- **Hardware remapping.** On newer hardware the Fn key can be remapped
  (to Control, or swapped) in System Settings and via
  `hidutil`/`com.apple.HIToolbox` properties; keyboards without a Globe key
  (most third-party externals) send nothing at all for it. This is why our
  default is right Option, not Fn (`docs/ux/02-core-interaction.md`).

### 2. Left/right modifier discrimination needs the device bits

The generic `CGEventFlags` option bit stays set while *either* Option key is
down. A `right-option` binding that watches only the generic bit misses the
release when left Option is also held, leaving the mic hot. The matcher uses
the NX device-specific bits (`NX_DEVICERALTKEYMASK` = 0x40 etc.) plus the
per-key keycode in the `flagsChanged` event.

### 3. `kCGEventTapDisabledByTimeout`

macOS disables an event tap whose callback is too slow, and also fires this
around sleep/wake and system stalls through no fault of ours. Without
handling, the hotkey dies silently and permanently. The callback:

1. receives `kCGEventTapDisabledByTimeout` / `...ByUserInput` *as an event*,
2. immediately calls `CGEventTapEnable` again,
3. resets the matcher and state machine (a key-up may have been swallowed
   while dead; a machine stuck in "pressed" keeps the mic hot forever, the
   worst trust failure available), emitting `Released` if capture was live,
4. emits `TapRecovered` so the UI can flip the tray warning glyph.

Corollary: **never block the callback.** It runs on the window server's
event-dispatch path. Ours reads two integers, runs two pure state machines,
and pushes to an unbounded channel. No locks shared with slow code, no IO,
no allocation-heavy work, no per-event env lookups.

### 4. Permission dependency

Creating even a listen-only tap requires Accessibility (or Input Monitoring)
trust. All of `docs/macos-permissions.md` applies, especially the
responsible-process trap: run from a terminal, it is the **terminal's**
grant being checked. `CGEventTapCreate` returning null is mapped to
`HotkeyError::PermissionDenied` with the fix in the message.

### 5. Conflict sources

A listen-only tap always "works", so binding Cmd+Space produces *both*
Spotlight and dictation on every press. There is no registration step to
fail. Detection is therefore explicit, before binding, and advisory:

- **`com.apple.symbolichotkeys`**: every system shortcut with keycode, NX
  modifier bits, and enabled flag. Read via `defaults export`, parsed, and
  compared (same keycode table, same bit layout as the tap events). Disabled
  entries are reported at lower severity, since they flip back on.
- **Static table** for chords that never appear there: Cmd+Tab, Cmd+`,
  Cmd+Alt+Esc, and the Globe-key action for bare fn.
- **Not knowable**: chords held by other running apps via Carbon
  `RegisterEventHotKey`. macOS has no public enumeration API. We do not
  pretend otherwise; runtime liveness monitoring (the tray warning on tap
  death) is the mitigation.

### 6. Keycodes are positional

Virtual keycodes name positions, not characters. A `cmd+a` binding checked
against an AZERTY layout is at the physical Q position. The default bindings
avoid character keys entirely, and the planned press-to-set picker captures
keycodes directly, bypassing the ANSI-US table used for parsing configs.

### 7. Wayland has no global hotkey protocol

By design: a client observing keys typed into other clients is the exact
keylogger capability Wayland exists to prevent. There is no equivalent of
XGrabKey. The options all move the binding out of process:

- **GlobalShortcuts XDG portal** (`org.freedesktop.portal.GlobalShortcuts`):
  the compositor owns the binding and delivers Activated/**Deactivated**
  signals over DBus, so push-to-talk works. KDE and GNOME ≥ 45; not
  universal on wlroots compositors.
- **Compositor config**: sway/Hyprland users bind keys to `exec` a small
  IPC trigger we ship.
- **evdev read access** (`input` group): works everywhere, but is literally
  system-wide key visibility and must be an informed opt-in.

## Per-platform capability table

| Capability | macOS (CGEventTap) | Windows (planned: WH_KEYBOARD_LL + RegisterHotKey probe) | Linux X11 (planned: XGrabKey/XI2) | Linux Wayland (portal) |
|---|---|---|---|---|
| Status | **implemented** | stub | stub | stub |
| Global key down + up | yes | yes (hook) | yes | via portal Activated/Deactivated |
| Bare modifier binding (right Option) | yes (flagsChanged + device bits) | yes (VK_RMENU etc.) | XI2 raw events, not XGrabKey | compositor-dependent |
| Fn/Globe key | yes, with traps above | no standard Fn visibility (vendor drivers) | keyboard-dependent (XF86Fn rarely delivered) | no |
| Conflict detection | symbolichotkeys + static table; other apps invisible | RegisterHotKey fails on collision (reliable) | XGrabKey BadAccess on collision (reliable) | compositor UI owns conflicts |
| Silent-death mode | tap disabled on timeout: **handled, re-enabled** | hook silently removed on timeout: needs watchdog | X server grab persists | portal session can be revoked |
| Permission required | Accessibility (already required) | none for hook (but AV heuristics flag it) | none | user consent dialog |

## Verifying on a real machine

```bash
cargo run -p hotkey --bin hotkey-demo                     # right-option default
cargo run -p hotkey --bin hotkey-demo -- fn               # the Aqua binding
cargo run -p hotkey --bin hotkey-demo -- cmd+shift+space 250
cargo run -p hotkey --bin hotkey-demo -- --selftest       # synthetic HID events
HOTKEY_DEBUG=1 ... # print every keyboard event the tap sees
```

The `--selftest` mode posts synthetic F13 events at the HID tap point, so
the real tap, matcher, and timing machine run exactly as for physical keys,
and asserts the Pressed/Released/Latched/Unlatched sequences. It cannot
synthesize the Fn flags change; Fn needs fingers (or `CGEventPost` of
keycode 63, which the window server does translate, see the demo notes).
