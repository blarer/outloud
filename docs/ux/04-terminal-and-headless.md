# Terminals and headless: the differentiator

Hexavoice works in "even a raw terminal" by pasting keystrokes. Nobody, closed or
open, treats the terminal as a first-class *editing* destination, and nobody
works at all when there is no display server: an SSH session into a build box,
a tmux session on a VPS, a TTY. This document designs that, because "works in
EVERY text destination" is the product promise and the terminal is where the
promise is hardest and most valuable (developers are the beachhead audience).

The M0 fact to design around: terminals expose **no writable AX text field**
(M0 matrix: "Terminal — no text field, uses paste fallback (expected)"). The
GUI strategy stack is unavailable. Everything below is about building an
equivalent capability out of what terminals *do* have: stdin, escape
sequences, the shell's own line editor, and multiplexer APIs.

## The capability ladder

The product probes the focused terminal and climbs as high as it can, exactly
mirroring the GUI strategy ladder. Each tier is strictly better than the one
below, and the active tier is visible in `hexavoice doctor`:

| Tier | Mechanism | Enables |
|---|---|---|
| T0 | synthesized keystrokes / bracketed paste | dictation only, append-at-cursor |
| T1 | bracketed paste + readline/ZLE knowledge | dictation with safe newline handling |
| T2 | **shell integration** (zsh/bash/fish plugin) | read + rewrite the current line buffer: real edit-by-voice |
| T3 | shell integration + OSC channel | T2 + in-terminal status indicator, works over SSH |
| T4 | tmux integration | T3 + status-right widget, works in detached/remote sessions |

T0 works on day one with zero setup in any terminal. T2 upward requires the
user to install a shell plugin, which is a one-liner and is where the
differentiator lives.

## Dictating into a terminal (T0/T1)

Plain dictation must be *safe* before it is smart:

- **All injected text goes through bracketed paste** when the terminal
  supports it (nearly all modern ones), so a dictated string containing a
  newline does not execute half a command. Outside bracketed paste, newlines
  in dictated text are stripped and the overlay says so.
- **Nothing we inject ever ends with Enter by itself.** Dictation composes
  the command line; the human presses Enter. The single exception is the
  explicit spoken command "run it" / "send it" (off by default, opt-in
  setting, mirroring Hexavoice's "Send It" but gated because terminals execute
  things). When enabled, "run it" strips the phrase, injects, and sends `\r`.
- **Terminal-profile formatting.** The destination profile
  (`05-settings-and-states.md`) for terminals defaults to: no auto-
  capitalization, no trailing period, no smart quotes (a smart quote in a
  shell command is a bug we cause), spoken-symbol vocabulary active ("dash
  dash force" → `--force`, "pipe" → `|`, "tilde slash" → `~/`). This profile
  activates automatically when the focused app is a known terminal or the
  active tier detected shell integration.

## Editing the current shell line (T2): the crown jewel

With the shell plugin installed, edit-by-voice works on the command line
exactly as it does in a GUI field. The plugin gives us a read/write interface
to the one text field a terminal really has: the shell's line buffer.

Mechanism: the plugin (zsh ZLE widget / bash readline binding / fish binding)
maintains a tiny local socket (`$XDG_RUNTIME_DIR/hexavoice/shell.sock`, or a
per-session file under `~/.cache` over SSH). On hotkey, the desktop app asks
the plugin for `(buffer, cursor)`; the edit pipeline runs the same
`EditIntent` parse/apply as everywhere else; the plugin writes the new buffer
back and repaints the line via the shell's own redraw. The shell's line
editor stays the owner of the buffer, so its own undo (`C-x C-u` in zsh,
`C-_` in readline) keeps working — the terminal equivalent of preferring
`AXSelectedText` because it preserves host undo.

```
$ kubectl get pods --namespace prod-web --output wide▂
        hold key: "change prod-web to staging-web"
$ kubectl get pods --namespace staging-web --output wide▂
```

No selection exists in a line editor, so targeting is always search-text or
scope words ("delete the last flag", "change the last word to json"). The
zero/one/many disambiguation rules from `03-edit-by-voice.md` apply
unchanged; numbered-highlight fallback renders as the candidates underlined
via the shell's region-highlight facility where available, or a numbered list
printed above the prompt and cleaned up after choice.

Freeform edits work too and are *scoped to the line buffer only*: "make this
a for loop over all yaml files in this directory" previews above the prompt
(see TUI preview below) before touching the buffer. The preview rule from
`03` is non-negotiable here: a model rewriting a shell command unreviewed is
a model that can type `rm`.

## What the overlay becomes without a compositor

The GUI overlay cannot exist over an SSH session or on a machine with no
display server. The indicator moves **into the terminal**, in three
independent, layerable forms:

### 1. OSC in-band indicator (T3)

The desktop app (or the headless daemon) emits escape sequences into the
session's tty, so the indicator travels through SSH and tmux for free,
because it is just bytes in the stream:

- **Cursor color** flips while capturing (`OSC 12`): the caret itself turns
  red while the mic is hot, restored on release. This is the perfect
  invisible-by-default indicator: zero characters of screen space, precisely
  located at the user's locus of attention.
- **Terminal title / progress** (`OSC 0` / ConEmu-style `OSC 9;4` where
  supported) carries the state word (`● listening`, `✎ transcribing`) for
  terminals and taskbars that surface it.
- Partial text during latched long-form dictation renders as ghost text at
  the cursor via the shell plugin (written into the buffer's postdisplay in
  zsh, not into the buffer itself), dim, exactly like the overlay's
  provisional tail.

### 2. Shell status line (T2/T3)

For shells with a right prompt or transient status (zsh `RPROMPT`, fish
right prompt, or a plugin-managed line above the prompt), the plugin exposes
`aqua_status` for the user's prompt framework (starship/p10k modules shipped
by us):

```
$ kubectl get pods --namespace staging-web▂          [● 0:03]
```

### 3. tmux status-right widget (T4)

A shipped tmux plugin (`hexavoice.tmux`, TPM-installable) adds a status segment
fed over the tmux control channel:

```
[0] 1:vim  2:logs* 3:ssh                 ● listening · 0:04 │ prod-vps │ 22:41
```

The widget also gives us presence in *detached-and-reattached* sessions and a
place to show which pane dictation is bound to. `tmux send-keys` and
`paste-buffer -p` (bracketed) are additionally used as the T4 injection path,
which is more reliable than synthetic keystrokes when the terminal emulator
is remote or unfocused.

## SSH and tmux sessions

The audio and the models are on the local machine; the text destination is
remote. Two supported topologies:

**A. Local capture, remote injection (default, zero remote install).**
The local app injects into the local terminal emulator (T0/T1); bytes flow
over SSH like typed keys. Works everywhere immediately. Edit-by-voice needs
the shell plugin *on the remote shell* to reach T2, installed by one command:
`hexavoice shell-install --ssh user@host` (appends the plugin line to the remote
rc, nothing else). The control channel between local app and remote plugin
multiplexes over the existing connection (SSH `RemoteForward` of the socket,
set up by our ssh config snippet, or degrades to an in-band OSC 52-style
handshake when forwarding is blocked).

**B. Fully headless (`hexad`).** On a machine with no GUI at all, dictating
*at* that machine's console: a user with a USB mic on a TTY, or audio
forwarded from a thin client. `hexad` is the same engine as the desktop app
minus every GUI dependency: hotkey via evdev, indicator via OSC, control via
the CLI and TUI below. This is also the accessibility story for GUI-less
users (`06-accessibility.md`): a motor-impaired sysadmin gets the full
product on a server.

tmux specifics worth pinning down:

- Injection targets the **active pane of the client the hotkey came from**,
  never a hardcoded pane. tmux control mode tells us which.
- Copy-mode is detected (pane in copy-mode ignores keys): dictating while a
  pane is in copy-mode surfaces "pane is in copy-mode, press q first" rather
  than silently vanishing text, per principle 4.

## The pure-TUI control surface

Everything the tray menu and settings window do must be reachable with no
display server. Two layers:

**CLI (scriptable truth):** `hexavoice status`, `hexavoice doctor`, `hexavoice listen`,
`hexavoice undo`, `hexavoice models`, `hexavoice set <key> <value>`, all with `--json`.
The CLI is the API; the TUI and GUI are both clients of it.

**TUI (`hexavoice tui`):** a full-screen ratatui-style surface for humans:

```
+ hexavoice ────────────────────────────────────────────────────────+
| state: ● listening (latched)          mic: USB Audio  ▁▂▅▂▁  |
| tier:  T3 (zsh plugin + OSC)          model: parakeet-tdt-v3 |
|                                                              |
|  partial: kubectl get pods --namespace stag…                 |
|                                                              |
|  last 3:                                                     |
|   22:40:12  edit   prod-web → staging-web        [u]ndo      |
|   22:39:50  dictate "kubectl get pods…"                      |
|   22:37:01  edit   3 × "teh" → "the"                         |
|                                                              |
|  [space] toggle mic  [u]ndo  [d]octor  [s]ettings  [q]uit    |
+──────────────────────────────────────────────────────────────+
```

The TUI doubles as the freeform-edit **preview surface** on headless systems:
the diff from `03-edit-by-voice.md` renders as a unified diff above the
prompt (or in a tmux popup, `display-popup`, when available), with the same
apply / retry / cancel choices, spoken or keyed.

## Honest limits

Stated in-product, not discovered by users:

- Inside a full-screen TUI app in the terminal (vim, htop), the shell plugin
  cannot see a line buffer. Dictation degrades to T0 keystroke injection,
  which in vim's insert mode is actually fine, and edit-by-voice reports
  "can't read this screen; dictation only here". A vim-specific integration
  is future work, not silently faked.
- Serial consoles and terminals without bracketed paste get newline-stripped
  T0 with a one-time warning.
- If neither the plugin socket nor OSC round-trips (hostile jump-host
  chains), we are a very good voice keyboard and say so.
