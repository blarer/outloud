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

## Native applications (macOS)

| Destination | Best tier | Read | In-place write | Notes |
|---|---|---|---|---|
| TextEdit | AX | yes | yes | M0 verified. `AXSelectedText` settable, undo preserved |
| Notes / Mail | AX | yes | yes | Standard AppKit `AXTextArea`; unverified in M0 only for window-Space reasons |
| Safari (address bar) | AX | yes | yes | M0 verified, native chrome field |
| Safari (web content) | AX | yes | yes | M0 verified: page `AXTextArea` with live contents, writable |
| Pages / Keynote | AX | yes | partial | Complex text engines expose `AXValue` read; write support varies per container |
| Xcode | AX | yes | yes | `AXTextArea` per editor pane; large buffers make timeouts matter |

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
| Discord | AX | yes | yes | Same contenteditable shape as Slack |
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
