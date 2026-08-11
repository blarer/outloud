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

- **Compositor config** (implemented, `backend/linux.rs`): sway/Hyprland
  users bind a key in their compositor config to `exec` a tiny CLI we
  ship (`outloud trigger press` / `outloud trigger release`), which speaks
  one line over a unix-domain socket to whatever `outloud` daemon is
  running. The compositor already owns every keybinding on the system and
  is trusted with arbitrary `exec`, so this needs no portal negotiation, no
  DBus session, and works identically on every wlroots compositor that can
  `exec` on a key edge. Chosen as the primary path because it is the ONLY
  one of the three that is available today on the actual deployment target
  (Hyprland), with zero new system dependencies, on a machine whose
  compositor config the user controls entirely.
- **GlobalShortcuts XDG portal** (`org.freedesktop.portal.GlobalShortcuts`,
  detection-only stub in `backend/linux.rs`): the compositor owns the
  binding and delivers Activated/**Deactivated** signals over DBus, so
  push-to-talk works. KDE and GNOME ≥ 45; not implemented by Hyprland or
  most wlroots compositors as of this writing, so building the full
  session/consent/signal-listening integration now would spend the budget
  on a path this project's own target machine cannot exercise. Worth
  finishing for a future KDE/GNOME contributor; not blocking the Hyprland
  path per the task that added it.
- **evdev read access** (`input` group): works everywhere, but is literally
  system-wide key visibility and must be an informed opt-in. Not attempted.

See "Verifying on a real machine (Linux Wayland)" below for the exact
compositor config snippet and what still needs a human on real hardware.

### 8. Windows: RegisterHotKey cannot do push-to-talk

The Windows backend (implemented, `backend/windows.rs`) uses a low-level
keyboard hook (`SetWindowsHookExW` with `WH_KEYBOARD_LL`) on a dedicated
message-pump thread, with `RegisterHotKey` demoted to a bind-time conflict
probe. The reasoning, since RegisterHotKey looks like the obvious API:

- **No key-up.** RegisterHotKey delivers `WM_HOTKEY` on the down edge
  only. Push-to-talk is defined by the release; there is no release
  notification, and polling `GetAsyncKeyState` to fake one adds latency
  and misses fast taps.
- **No bare modifiers.** The API requires a non-modifier VK, so right-Alt
  (the nearest equivalent of our right-Option default) cannot be bound.
- What it IS good at: registration fails with
  `ERROR_HOTKEY_ALREADY_REGISTERED` when any other process holds the
  chord, which is more reliable conflict detection than macOS offers. So
  we register, record the result, and unregister immediately. The finding
  flows through `conflict::check_chord` into `HotkeyManager::conflicts()`
  like every other conflict source, so callers surface it the same way.

The hook's own traps:

- **The silent timeout unhook.** A hook callback that exceeds
  `LowLevelHooksTimeout` (default 300ms, `HKCU\Control Panel\Desktop`) is
  removed by the OS with no notification, the exact analogue of
  `kCGEventTapDisabledByTimeout` except *without the courtesy event*. The
  callback is therefore allocation-free and never blocks, and a watchdog on
  the pump thread verifies liveness every 2s. Detecting removal is itself a
  trap: no API answers "is my hook installed", so the watchdog uses the one
  observable side effect, that unhooking an already-removed hook FAILS. On
  detection it reinstalls and RESETS the matcher and state machine, because
  a key-up swallowed while dead would otherwise leave the machine stuck in
  "pressed" with the microphone hot forever, and emits `TapRecovered` so the
  UI can flip its warning glyph, exactly as the macOS tap does.
- **UIPI (the elevation trap).** When an elevated (admin) window has
  focus, a non-elevated process's hook does not see its keys, and
  SendInput into it is silently discarded: User Interface Privilege
  Isolation blocks both observation and injection upward across integrity
  levels. Symptom: the hotkey (and text delivery) simply dies while an
  elevated app is focused and comes back when focus moves. The only real
  outs are running elevated ourselves (unacceptable default) or a signed
  uiAccess=true binary installed under Program Files (a shipping-product
  decision, not a spike one). We document the symptom instead.
- **Fn does not exist.** PC keyboard firmware handles Fn internally; no
  VK arrives. `fn`-containing chords are refused at bind time with a
  message, rather than binding a key that can never fire.
- **Typematic repeat.** Holding a key delivers repeated `WM_KEYDOWN`; the
  matcher collapses them to one Down edge or tap/hold would re-trigger.

## Per-platform capability table

| Capability | macOS (CGEventTap) | Windows (WH_KEYBOARD_LL + RegisterHotKey probe) | Linux X11 (planned: XGrabKey/XI2) | Linux Wayland (compositor-exec, `backend/linux.rs`) |
|---|---|---|---|---|
| Status | **implemented** | **implemented** (compiled in CI; untested on real hardware) | stub | **implemented** (compile-checked and cross-linked for `x86_64-unknown-linux-gnu`; **not run on real Wayland/Hyprland hardware**, see below) |
| Global key down + up | yes | yes (hook) | yes | yes (compositor's `bind`/`bindr` execs `outloud trigger press`/`release`) |
| Bare modifier binding (right Option) | yes (flagsChanged + device bits) | yes (VK_RMENU etc.) | XI2 raw events, not XGrabKey | whatever the compositor can bind and `exec` on; no keycode/flags matching needed here since the compositor already decided |
| Fn/Globe key | yes, with traps above | no standard Fn visibility (vendor drivers) | keyboard-dependent (XF86Fn rarely delivered) | whatever the compositor exposes as a bindable key |
| Conflict detection | symbolichotkeys + static table; other apps invisible | RegisterHotKey fails on collision (reliable) | XGrabKey BadAccess on collision (reliable) | none from this crate: the compositor's own keybind table is the source of truth and already prevents two `bind`s on one key |
| Silent-death mode | tap disabled on timeout: **handled, re-enabled** | hook silently removed on timeout: **handled, 2s watchdog reinstalls + resets** | X server grab persists | lost RELEASE trigger (no OS signal that one is missing): **handled, watchdog polls and resets after `OUTLOUD_HOTKEY_TRIGGER_WATCHDOG_MS` (default 120s)**, daemon-not-running: **handled, `outloud trigger` names the daemon as the problem instead of failing silently** |
| Permission required | Accessibility (already required) | none for hook (but AV heuristics flag it) | none | none: unix socket, 0700/0600 perms + `SO_PEERCRED` own-uid check (same defense as `shell-bridge`) |

## Verifying on a real machine (Windows)

```powershell
cargo run -p hotkey --bin hotkey-demo                     # right Alt (default)
cargo run -p hotkey --bin hotkey-demo -- ctrl+shift+space 250
cargo run -p overlay --bin overlay-demo                   # focus/click-through check
```

What to check with your own eyes, since CI can compile this but never run it:

1. **Both edges fire.** Hold the chord: `Pressed` on the way down,
   `Released` on the way up. A tap should give `Latched`, and the next tap
   `Unlatched`.
2. **Focus is never stolen.** With `overlay-demo` running, type into
   Notepad. Every keystroke must land in Notepad, and clicks where the
   overlay sits must reach the window underneath.
3. **The UIPI wall.** Open an *administrator* PowerShell, focus it, and hold
   the chord: nothing fires, by design. Move focus to a normal window and it
   works again. If that surprises you, re-read the UIPI trap above.
4. **The watchdog.** Suspending the machine or a long debugger stall can get
   the hook removed; within ~2s the log should say it was reinstalled rather
   than going quiet forever.

`--selftest` is macOS-only: it posts synthetic HID events through the real
tap, and the Windows equivalent (SendInput feeding the hook that observes
it) is a loop worth building deliberately rather than as a demo side effect.

## Verifying on a real machine (macOS)

```bash
cargo run -p hotkey --bin hotkey-demo                     # right-option default
cargo run -p hotkey --bin hotkey-demo -- fn               # the OutLoud binding
cargo run -p hotkey --bin hotkey-demo -- cmd+shift+space 250
cargo run -p hotkey --bin hotkey-demo -- --selftest       # synthetic HID events
HOTKEY_DEBUG=1 ... # print every keyboard event the tap sees
```

The `--selftest` mode posts synthetic F13 events at the HID tap point, so
the real tap, matcher, and timing machine run exactly as for physical keys,
and asserts the Pressed/Released/Latched/Unlatched sequences. It cannot
synthesize the Fn flags change; Fn needs fingers (or `CGEventPost` of
keycode 63, which the window server does translate, see the demo notes).

## Verifying on a real machine (Linux Wayland / Hyprland)

**Everything in this section was compile-checked and cross-linked for
`x86_64-unknown-linux-gnu` from macOS (typecheck, clippy, and a full test
binary link, using `cargo check`/`clippy`/`test --no-run` against the
`x86_64-unknown-linux-gnu` target with a cross C toolchain). None of it has
been executed on a real Linux/Wayland/Hyprland machine.** The unit tests in
`crates/hotkey/src/backend/linux.rs` (real unix-domain socket, real threads,
real watchdog timing via an env override) all pass when run natively on
macOS's own unix-socket stack, which exercises the same std APIs Linux
provides, but that is not the same claim as "verified on Hyprland" and this
file will not pretend otherwise.

### 1. Start the daemon

```bash
cargo build -p outloud --no-default-features   # headless; add --features display for the GUI/tray
./target/debug/outloud --chord f13              # any bindable key; F13 avoids clashing with anything
```

### 2. Add the compositor keybind

Hyprland (`~/.config/hypr/hyprland.conf`, or the equivalent block in a NixOS
flake's `wayland.windowManager.hyprland.settings.bind`):

```
bind  = , F13, exec, outloud trigger press
bindr = , F13, exec, outloud trigger release
```

`bind` fires on the key going down, `bindr` on release; Hyprland supports
both natively, matching the tap-hold machine's need for both edges (`docs/
hotkeys.md` intro). Substitute whatever key `--chord` above was given; it
does not need to match a real `hotkey::Chord` string on the Linux path
since the compositor, not this crate's matcher, decides which physical key
fires — see the module doc in `backend/linux.rs` for why no keycode
matching happens here at all.

sway (`~/.config/sway/config`):

```
bindsym F13 exec outloud trigger press
bindsym --release F13 exec outloud trigger release
```

Reload the compositor (`hyprctl reload`, or restart sway) after editing.

#### Picking a key that can actually fire

Two traps, both of which look like the software is broken. Verified on real
hardware (Hyprland 0.56, NixOS, a Wooting 80HE):

- **Do not bind a bare modifier.** `bind = , Alt_R, ...` registers happily
  and never fires. `hyprctl binds` lists it twice, press and release, and
  the physical key does nothing, while `outloud trigger press` by hand
  drives the daemon to `state listening`. A modifier press is not a keybind
  event: the compositor is deciding whether it begins a chord. Same applies
  to Super, Ctrl, Shift and to `caps:super` / `caps:hyper`, which keep Caps
  Lock a modifier.

- **Check that an XKB option exists before trusting it.** `caps:f13` looks
  obvious and is not real. xkbcommon and Hyprland both accept an unknown
  option silently -- `hyprctl getoption input:kb_options` will even read it
  back to you as if set -- and simply do nothing, so Caps Lock stays Caps
  Lock and the bind can never fire. The real list:

  ```
  grep 'caps:' "$(nix eval --raw nixpkgs#xkeyboard_config)"/share/X11/xkb/rules/base.lst
  ```

  On this machine the working choice was `caps:menu`: Menu is an ordinary
  non-modifier key that essentially nothing else binds, and Caps Lock is
  under the left pinky.

### 3. What to check with your own eyes

CI and this development machine can compile and cross-link this backend but
never run it; only a human on the target hardware can confirm these:

1. **Both edges fire.** Hold the key: `outloud: state listening` should
   appear (or `--no-overlay`'s stderr log line) on press, and commit on
   release. A quick tap should latch (state stays listening after
   release), a second tap should unlatch — the exact behaviour documented
   for the macOS/Windows backends, driven here by the trigger socket
   instead of an OS-level tap or hook.
2. **`outloud trigger ping` reports liveness honestly.** With the daemon
   stopped, `outloud trigger press` must fail with a message naming
   "is `outloud` running?" (`hotkey::backend::linux::send_trigger`'s doc),
   not a bare `ENOENT` or silent success. With it running, `outloud
   --permissions` should show `hotkey: trigger daemon reachable at ...`,
   and `doctor` (`crates/diag`) should report the `linux-hotkey-trigger`
   check as PASS.
3. **A lost RELEASE actually recovers.** Bind only the `bind` (press) line,
   deliberately omit `bindr`, hold the key once, then release it (nothing
   fires). Set `OUTLOUD_HOTKEY_TRIGGER_WATCHDOG_MS=5000` before starting
   the daemon to shrink the wait, and confirm the log shows the watchdog
   forcing the microphone closed within a few seconds rather than staying
   hot until the process is killed. This is the one failure mode unique to
   this backend (the others all react to an OS-delivered "your tap/hook
   died" signal; this one has none, see `crate::trigger`'s module doc) and
   is the single most important thing to confirm on real hardware before
   trusting the backend.
4. **Compositor reload does not wedge the socket.** Reload Hyprland/sway
   config a few times while the daemon keeps running; `outloud trigger
   ping` should keep succeeding throughout (the socket is independent of
   the compositor process).

### What is NOT covered here

- The XDG GlobalShortcuts portal path (KDE/GNOME) is detection-only
  (`hotkey::backend::linux::globalshortcuts_portal_available`) and has no
  binding logic; see its doc comment in `backend/linux.rs` for the full
  scope of what a real implementation would still need.
- X11 (XGrabKey/XI2) remains an unimplemented stub, unchanged by this work.
