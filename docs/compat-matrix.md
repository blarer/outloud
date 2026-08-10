# Destination compatibility matrix

Which delivery tier reaches which destination, whether we can read the
existing text back (required for edit-by-voice, not just dictation), and
whether the write lands in place. Findings come from the M0 spike where a
row was tested, and from each destination's documented protocol surface
otherwise; untested rows say so in the notes.

Tier legend, preference order per environment (see `text-target/src/detect.rs`
for why terminals invert it):

| Tier | Mechanism |
|---|---|
| AX | Accessibility in-place edit (macOS AX, Windows UIA TextPattern, Linux AT-SPI2) |
| IME | Input-method injection (`zwp_input_method_v2`, TSF, InputMethodKit) |
| Keys | Synthetic keystrokes (CGEvent, SendInput, uinput) |
| Clip | Clipboard set + paste keystroke + restore |
| Term | Terminal-native (OSC 52, bracketed paste, multiplexer/emulator IPC, shell line editor) |
| HL | Headless daemon socket or stdio filter |

## Linux/Wayland delivery

The Keys tier on Linux/Wayland is `wtype`
(`text_target::targets::keys::WtypeTarget`), not uinput: `wtype` uploads a
throwaway xkb keymap per invocation so it types the literal characters in
the payload regardless of the destination's active keyboard layout, the
same problem raw uinput/`ydotool` scancode injection does not solve.
`wtype` also doubles as the paste-keystroke sender for the Clip tier
(`wtype -M ctrl v -m ctrl`), since Linux has no `osascript`/System
Events-equivalent scripting target.

**Requires the compositor to implement `zwp_virtual_keyboard_manager_v1`.**
Hyprland, Sway, and other wlroots-family compositors do. GNOME (Mutter) and
KDE (KWin) deliberately do **not** expose it to arbitrary clients, for the
same reason a browser refuses an unprivileged
`document.execCommand('paste')`: an unauthenticated virtual keyboard is a
keylogger-adjacent capability. On those compositors `wtype` is installed
and runs, but every invocation fails with wtype's own "compositor does not
support the virtual keyboard protocol" message. `outloud doctor`'s
`display-server` check reports this distinction (PASS only means "wtype is
on PATH", not "the compositor accepts it" -- there is no way to probe
compositor support without actually typing something).

| Destination | Best tier | Notes |
|---|---|---|
| Hyprland / Sway (any focused app) | Keys (wtype) | Primary path. Long text (over `WtypeTarget::WTYPE_MAX_CHARS`, 500 chars) falls back to Clip: `wtype` does one Wayland roundtrip per key edge, so a long payload would visibly stream |
| GNOME (Mutter) / KDE (KWin) | Clip | `wtype` refuses (no virtual-keyboard protocol); `wl-copy`/`wl-paste` + `wtype -M ctrl v -m ctrl` for the paste keystroke still work, since the clipboard and paste-keystroke synthesis are separate protocols from typing |
| X11 (any window manager) | Clip (`xclip`/`xsel`) | `wtype` is Wayland-only; an X11 Keys tier (XTEST) is not implemented in this crate yet, see `docs/debugging.md` |
| No `wtype`, no `wl-clipboard`/`xclip`/`xsel` | none | `outloud doctor`'s `display-server`/`clipboard` checks FAIL and name exactly which package to install (nixpkgs: `wtype`, `wl-clipboard`) |

Traps worth knowing before debugging a "nothing types" report on Linux:

- **Newlines and unicode**: `wtype` special-cases `'\n'` to the `Return`
  keysym and builds a one-off keymap entry per codepoint, so multi-line
  transcripts and non-Latin scripts are typed correctly with no escaping on
  this crate's side. This is one respect in which `wtype` beats the
  clipboard fallback: paste can be intercepted by an app's own paste
  handler (some terminals disable bracketed paste by default); a
  `wtype`-synthesized keystroke looks identical to a real one to every
  listener.
- **Clipboard restore**: the Clip tier saves the user's clipboard before
  writing the dictated text and restores it after a short settle delay
  (150ms), same as every other platform's Clip implementation. A crash
  between write and restore leaves the dictated text on the clipboard
  rather than the user's own content; `outloud::inject::drain_pending_restores`
  is the shutdown-path guard against the common case of this (process exit
  racing the restore thread), not against a hard crash.
- **Focus races**: `wtype`'s key events land wherever the compositor's
  keyboard focus is at the moment they're actually sent, which is AFTER
  hotkey release and STT latency (hundreds of ms to seconds). Alt-tabbing
  during that window sends the text to the new focus target silently; no
  Wayland protocol reports "who is focused now" back to the client. Same
  class of race every insert-only tier has on every platform (SendInput,
  CGEvent), worth naming here because focus-follows-mouse compositor
  configs make it easier to trigger.
- **Runtime dependency, not a build dependency**: `wtype` and
  `wl-copy`/`wl-paste` must be on `PATH` at runtime; this crate only shells
  out to them; nothing in `text-target`'s own build links against a
  Wayland client library. Not hardware-verified: this crate is developed on
  macOS, which cannot run a Wayland session, so nothing here has been
  exercised against a real `wtype` process. `text-target`'s unit tests
  cover the pure logic (length-threshold chunking, paste-capability
  selection) with the subprocess calls themselves left unexercised.

## Native applications (macOS)

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| TextEdit | AX | yes | yes | M0 verified. `AXSelectedText` settable, undo preserved |
| Notes / Mail | AX | yes | yes | Standard AppKit `AXTextArea`; unverified in M0 only for window-Space reasons |
| Messages (iMessage) | AX | yes | yes | Verified live. Compose field is an `AXTextField` with settable `AXValue` (11-30ms writes). Caveat: while EMPTY the field intermittently reports no `AXValue` at all, so delivery falls back to typing; that fallback must be the batched CGEvent path (~9ms), not per-character pacing (~40ms). See "Synthetic keystrokes: two speeds" below |
| Safari (address bar) | AX | yes | yes | M0 verified, native chrome field |
| Safari (web content) | AX | yes | yes | M0 verified: page `AXTextArea` with live contents, writable |
| Pages / Keynote | AX | yes | partial | Complex text engines expose `AXValue` read; write support varies per container |
| Xcode | AX | yes | yes | `AXTextArea` per editor pane; large buffers make timeouts matter |

### Synthetic keystrokes: two speeds (macOS)

The Keys tier has two deliberately different pacing modes, chosen per
destination by `text_target::targets::keys::typing_strategy_for` (pure and
unit-tested, keyed on the DESTINATION app, never on our own tty):

| Mode | Mechanism | Cost for a sentence | Used for |
|---|---|---|---|
| `synthetic-keys-batched` | multi-character `CGEventKeyboardSetUnicodeString` payloads, 20 UTF-16 units per event, no pacing | ~9ms | GUI apps whose AX write was refused (Messages with an empty compose field, secure fields, canvas editors) |
| `synthetic-keys-paced` | one character per event, ~0.7ms spin-paced | ~25-45ms | tty-backed apps (Terminal, iTerm2, WezTerm, kitty, Alacritty, Ghostty, ...), plus any field that reads but refuses writes, the scrollback signature of an unknown terminal |

The split exists because a tty samples key events instead of reading the
attached string: a 20-unit payload delivered "hello from cgevent" to
Terminal.app as "bat". Batched into a terminal corrupts; paced into a GUI
merely wastes 40ms, so unknown destinations default to paced.

## Browsers

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| Chrome (macOS) | AX | yes | yes | Chromium enables its AX tree when an assistive client connects; first query can be slow |
| Chrome (Windows) | AX (UIA) | yes | yes | TextPattern on web content; same lazy-enable behavior |
| Chrome (Linux) | AX (AT-SPI) | yes | partial | Needs screen-reader announcement or `--force-renderer-accessibility` |
| Firefox | AX | yes | yes | Mature AX tree on all three platforms |
| Google Docs in a browser | Keys/Clip | no | no | Canvas renderer: no per-paragraph AX text. Read requires DOM-level extension, out of scope |
| CodeMirror / Monaco in a browser | AX | partial | partial | contenteditable exposes the visible region only; virtualized lines are absent from AX |

## Electron applications

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| VS Code (editor) | AX | partial | partial | Monaco virtualizes: AX sees rendered lines only. Reliable for the current line, not the file |
| VS Code (integrated terminal) | Term (HL) | via shell | via shell | xterm.js pane, no writable AX field; shell integration or extension API is the real path |
| Slack | AX | yes | yes | M0 exit criterion row; Quill editor exposes contenteditable AX |
| Discord | synthetic keys | no | no | ACCEPTS an AXValue write and ignores it: React's model keeps the old value, so Enter breaks the line instead of sending. Diverted to typing (`keys::ignores_ax_value_writes`) |
| Obsidian | AX | partial | partial | CodeMirror 6 virtualization, same caveat as VS Code |
| Notion | AX | partial | partial | Block editor: focused block reads fine, cross-block edits need per-block traversal |

## Terminal emulators

The AX row for every terminal is the same: readable grid text at best, never
a writable field (M0 measured Terminal.app as paste-fallback). So the Term
tier is what differentiates them.

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| tmux (any emulator) | Term | yes (`capture-pane`) | line only (`C-u` + `paste-buffer -p`) | Implemented. Works over SSH and headless; bracketed paste via `-p` |
| GNU screen | Term | yes (`hardcopy`) | line only | Stub: temp-file based `readbuf`/`paste .` |
| WezTerm | Term | yes (`cli get-text`) | line only | Implemented. `cli send-text` pastes bracketed by default; mux socket works over SSH domains |
| kitty | Term | yes (`kitten @ get-text`) | line only | Stub: requires `allow_remote_control yes`; refuses everything otherwise |
| iTerm2 | Term | yes (Python API) | line only | Stub: API is an authenticated websocket the user enables; OSC 1337 covers write-only extras |
| Terminal.app | Clip/Keys | no | no | No IPC, no writable AX field (M0). OSC 52 write works; paste is the input path |
| Alacritty | Clip/Keys | no | no | Deliberately no IPC beyond `msg create-window`; keystrokes or paste only |
| Ghostty | Term (partial) | no | no | Speaks OSC 52 and bracketed paste; no remote-control read API yet |
| GNOME Terminal / Konsole | Clip/Keys | no | no | VTE/Konsole expose no buffer-read IPC; AT-SPI shows the grid read-only |
| xterm | Clip/Keys | no | no | OSC 52 works (allowWindowOps permitting); nothing readable |
| Windows Terminal (ConPTY) | Term | no | no | Implemented for an *owned* pseudoconsole: bracketed paste into the input pipe (PSReadLine consumes it atomically). Foreign console needs `AttachConsole`+`ReadConsoleOutput` in a helper process (follow-up); UIA shows the grid read-only |

## Shell line editors (inside any terminal above)

The only true in-place, undo-preserving path a shell has. Each needs a
one-time rc snippet speaking the daemon protocol in `targets/headless.rs`.

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| bash (readline) | HL | yes (`READLINE_LINE`) | yes | `bind -x` widget reads and assigns `READLINE_LINE`/`READLINE_POINT` |
| zsh (zle) | HL | yes (`$BUFFER`) | yes | zle widget; external trigger via `TRAPUSR1` calling `zle` on the active widget |
| fish | HL | yes (`commandline`) | yes | `commandline -r` replaces, `commandline -f repaint` redraws |
| readline apps generally (psql, python) | HL | partial | partial | Same `bind -x` mechanism where the app loads inputrc; many embed readline without it |
| PowerShell (PSReadLine) | HL | yes | yes | `[Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState` / `Replace` from a key handler |

## Remote and headless

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| SSH session (plain) | Term | no | no | OSC 52 write-back to the *local* clipboard works with zero setup; reading needs tmux or a shell widget on the remote end |
| SSH + tmux on remote | Term | yes | line only | Full tmux row, unchanged by the network hop; this is the recommended remote setup |
| CI / no tty | HL | yes | yes | Stdio filter mode: buffer in, rewrite out. How this crate tests itself |
| Editor plugin (any) | HL | yes | yes | Daemon socket protocol: HELLO/BUFFER/READ/REPLACE, ~10 lines per client |
| Linux console (no X/Wayland) | Keys (uinput) | no | no | uinput types into the console; nothing reads it back short of `/dev/vcs` |

## Editors (native, non-terminal)

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| Neovim | HL | yes | yes | `nvim --remote-expr 'getline(".")'` / `nvim_buf_set_text` over `$NVIM` socket; strictly better than any generic tier |
| Emacs | HL | yes | yes | `emacsclient --eval`: full buffer read/write with undo intact |
| Sublime Text | AX | partial | partial | Custom text widget exposes a flat AX value; plugin API is the reliable path |
| JetBrains IDEs | AX | partial | partial | Java AX bridge is lossy; the built-in HTTP REST API or a plugin is the real integration |

## Reading the line buffer back: summary

Edit-by-voice needs read. Ranked by setup cost:

1. **tmux `capture-pane`**: zero setup if the user already lives in tmux. Reads the pane, not the logical line; the line must be located under the cursor row.
2. **WezTerm `cli get-text` / kitty `@ get-text` / iTerm2 API**: zero to one config line, same pane-not-line caveat.
3. **Shell widgets** (bash `READLINE_LINE`, zsh `$BUFFER`, fish `commandline`): one rc snippet, and the *only* option that reads the logical line with cursor position and writes back through the editor's own state, preserving its undo. This is the quality bar; everything else approximates it.
4. **OSC 52 read**: specified, but disabled by default nearly everywhere for exfiltration reasons, and reads the clipboard rather than the line. Not viable.

## Windows platform traps (cross-cutting)

These affect every tier on Windows and are documented once here rather than
per-row:

- **UIPI / elevation.** User Interface Privilege Isolation blocks a
  medium-integrity process from observing or injecting input into a
  high-integrity (elevated) window, and UIA patterns refuse likewise.
  SendInput *reports success* while the input is discarded, so this is not
  even detectable at the call site. Symptom: dictation and hotkeys go dead
  exactly while an elevated app (admin terminal, installer, regedit) has
  focus, and recover when focus moves. Fixes are all product-level: run
  elevated (bad default), or ship a signed uiAccess=true binary installed
  under Program Files. The spike documents the symptom instead.
- **DPI virtualization.** Without per-monitor-v2 DPI awareness
  (`SetProcessDpiAwarenessContext`, declared by the overlay at
  construction), Windows lies about coordinates on scaled monitors and
  overlay positioning lands offset by the scale factor. Any future code
  touching screen coordinates must live under the same awareness context.
- **AVX2 static-initializer crash class.** Already covered in
  docs/build-and-release.md: Windows builds must respect the baseline
  policy or pre-Haswell machines crash before main.

## Electron apps expose their editor only while it is focused

Measured on Discord, and it changes how these apps must be diagnosed.

Two probes of the same running app, seconds apart:

```
$ spike-cli probe          # reads the FOCUSED element
role:      AXTextArea
app:       Discord
writable:  value=true selectedText=true

$ spike-cli inspect Discord   # walks the app's whole tree
Discord: 1 window(s), no text fields exposed
```

The tree walk already sets `AXManualAccessibility` and waits 400ms for the
tree to build, so this is not the Chromium opt-in being missed. The message
box simply is not in the tree until it holds focus, which is consistent with
Electron building accessibility nodes lazily for the focused subtree.

**Consequence for diagnosis.** "No text fields exposed" from `inspect` is not
evidence that an app cannot be written to. For Electron destinations it is the
expected answer, and only a focused `probe` says anything real. A conclusion
drawn from the scan alone would be wrong in exactly the apps people use most.

**Consequence for the product.** None directly: injection always runs against
the focused element, which is the case that works. It matters because it makes
these apps hard to diagnose without a human at the keyboard, and because the
same lazy-tree behaviour is the likeliest reason a write that succeeds once
can fail afterwards.

## Discord discards typed text about a second after it lands

Measured, and it is not the doubling bug fixed in `inject.rs`.

Polling the focused field once a second while one utterance is dictated:

```
t+1s   " The dog is brown and has a lot of fun running through the yard..."
t+2s   "\u{feff}\n"
t+3s   "\u{feff}\n"
```

The text arrives complete and correct, then the field returns to Discord's
empty state (a zero-width no-break space and a newline) roughly a second
later. Four consecutive dictations all reported `synthetic-keys-paced`
success and all ended with an empty field.

**We do not send Return.** There is no Enter synthesis anywhere in the
injection path, so this is not an accidental submit. Discord is discarding
the content on its own.

The likeliest reading, consistent with why this app is on
`AX_VALUE_IGNORED_APPS` in the first place, is that its React editor
reconciles against its own model shortly after the synthetic events land,
finds a model that never recorded the keystrokes, and rewrites the DOM back
to that model. Synthetic CGEvents reach the field but apparently not the
state the component trusts.

**What this changes.** Discord was moved off the AXValue tier onto synthetic
typing precisely because AXValue writes were ignored. This measurement says
typing is *also* not sufficient there, so the app needs a different transport
again, and the clipboard-paste path is the obvious candidate: a real paste is
delivered through the same event stream as a human's Cmd-V and is far harder
for an editor to distinguish.

Not yet attempted, because the fix should be measured the same way this was
rather than assumed.

## Messages: text lands and stays, but the field is AXValue-only

Measured, and materially different from Discord despite the same reported
symptom ("dictation does not work here").

The compose field, probed while focused:

```
role:      AXTextField
app:       Messages
writable:  value=true selectedText=false
strategy:  set-value
```

`selectedText=false` is the interesting part. TextEdit offers both, so the
injection layer can use `AXSelectedText`, which goes through the app's own
text system and preserves its undo. Messages offers only `AXValue`, so a
write there replaces the entire field and resets the app's undo stack.

A dictation delivered via `synthetic-keys-batched` and the text stayed in the
field afterwards, unlike Discord, which clears it about a second later. So
whatever is wrong in Messages is not the transport dropping text.

Also unlike Discord, an unfocused tree scan finds it fine: 7 writable text
fields, including the message bubbles of the open conversation. That is
consistent with a native AppKit app building its accessibility tree eagerly,
where Electron does not.

**Still unexplained, and one hypothesis is now ruled out.** The original
report was that dictation "doesn't work" in iMessage. Measurement shows text
arriving via `set-value` and persisting, so the transport is not dropping it.

Attempting to test the "second consecutive utterance fails" hypothesis
produced a useful negative result of a different kind: Discord repeatedly
took focus back mid-test, so two dictations aimed at Messages landed in
Discord instead. That is worth recording, because it is also the likeliest
explanation of the original report. A dictation goes wherever focus is at
key-down, and on a machine where a chat app can raise itself, "it did not
appear in iMessage" and "it appeared somewhere else" are the same event.

Remaining candidates, narrowest first:

1. Focus moved between key-down and commit, so the text went elsewhere.
   Consistent with what was observed here, and invisible to the user unless
   they happen to look at the other app.
2. Text lands but Return does not send it, so it looks like nothing
   happened. Untested.
3. Something specific to a second consecutive utterance. Untested, because
   the focus problem above prevented a clean run.

Diagnosing this needs a reproduction from someone who can keep the app
focused, and the first thing worth checking is whether the missing sentence
turned up in a different window.
