# Debugging guide

Almost every failure in this project is environmental, not logical. The fastest
debugging move is nearly always `./scripts/doctor.sh`, which runs every check
below and prints a named next action. This document is the human-readable
version of the same knowledge: symptom first, because that is all you have when
something breaks.

Every failure gets classified as one of four kinds, and the classification
decides where it goes:

| Class | Meaning | Where it goes |
|---|---|---|
| Environment | The machine/session is unsuitable (SSH, Wayland, no mic, other Space) | Fix the environment |
| Permission | An OS grant is missing or attached to the wrong process | System Settings |
| Configuration | The install is wrong (bare binary, ad-hoc signature, no models) | Re-run setup |
| Bug | The code misbehaved | **The only class worth a GitHub issue** |

## Symptom → cause → fix

### macOS permission and identity traps

| Symptom | Cause | Fix |
|---|---|---|
| Every AX call fails with `-25204` even though `AXIsProcessTrusted` returns true | You are using `AXUIElementCreateSystemWide` + `AXFocusedUIElement`. On current macOS it returns `kAXErrorCannotComplete` even for trusted processes | Resolve the focused *application* first, then ask it for its focused element. `ax-edit` already does this; do not "simplify" it back |
| Toggle is ON in System Settings, all calls still fail, binary was run from a shell | TCC judges the *responsible process*. Shell-launched binaries are judged against the terminal's grant, not their own | Launch through LaunchServices: `open -a dist/OutLoudSpike.app` or `./scripts/doctor.sh`. Or grant the terminal itself (dev only) |
| Worked yesterday, broken after `cargo build`, toggle still reads ON | Ad-hoc signature: TCC pins the grant to the cdhash, every rebuild silently revokes it | `tccutil reset Accessibility dev.hexavoice.spike`, re-grant. Long term: Developer ID certificate, which pins to team id instead |
| App never appears in the Accessibility pane at all | Bare binary has no stable identity for TCC to list | Bundle it: `./scripts/bundle-macos.sh`, then add the .app |
| Recognizer transcribes only silence | Microphone permission denied, or no input device. These are different failures | `doctor` distinguishes them: check `microphone-permission` vs `audio-input` |

### Window and application visibility

| Symptom | Cause | Fix |
|---|---|---|
| App reports **zero windows**, looks like it exposes nothing | Its windows are on another Space; the window server hides them completely | Drag one window to the current Space (Mission Control), retry |
| Walking an app's `AXChildren` finds thousands of menu items and no text | App windows hang off `AXWindows`, not `AXChildren` (children are the menu bar) | Use `AXWindows`. `ax-edit::find_text_fields` already does |
| Chrome/Electron exposes no accessibility tree at all | Chromium builds its AX tree lazily and only after an opt-in | Set the private `AXManualAccessibility` attribute to true on the app element first, then re-read. Implemented in `ax-edit` |
| VS Code / Slack / Electron app shows a tree but text fields read empty | Electron's AX support varies per app and version; some render text into a canvas-like surface | Fall back to clipboard-paste strategy (`RewriteStrategy::ClipboardPaste`). If reading is required, test that specific app version and record it in the matrix |
| A hotkey press appears to do nothing, then everything happens at once | AX calls are synchronous IPC; a busy target (spinning Electron renderer) blocks the caller | `ax-edit` sets a 500ms messaging timeout. If you see multi-second stalls, the timeout is not being inherited: bug, file it |

### Linux display servers

| Symptom | Cause | Fix |
|---|---|---|
| Injection silently does nothing, `WAYLAND_DISPLAY` is set | Wayland forbids synthetic input by design | Use the XDG RemoteDesktop portal or wlroots virtual-keyboard protocol. XTEST does not exist here |
| Injection works in some apps, not others, on Wayland | XWayland apps accept XTEST, native Wayland apps do not | Same portal fix; per-app behavior is expected until then |
| Everything fails, `SSH_TTY` or `SSH_CONNECTION` set | You are in an SSH session: no local display, and a forwarded `DISPLAY` points at a **remote** X server | Run on the machine's own graphical session |
| Clipboard operations fail on Linux | No helper installed | `wl-clipboard` (Wayland) or `xclip`/`xsel` (X11) |

### Terminals

| Symptom | Cause | Fix |
|---|---|---|
| Terminal text cannot be read or rewritten via AX | Terminals are read-only through accessibility; expected, not a bug | Paste fallback is the designed path |
| Paste into a tmux session garbles or splits text | tmux intercepts paste; bracketed paste not negotiated | Enable bracketed paste in `.tmux.conf`, or inject via `tmux load-buffer` / `paste-buffer` |
| Paste into GNU screen misbehaves (`STY` set) | Same interception, screen flavor | `screen -X paste` or avoid the multiplexer for dictation |
| Injection lands in the wrong place over SSH | Local injection cannot reach a remote shell's prompt | Dictate only into local applications |

Identify what you are actually inside with env vars: `TERM_PROGRAM`
(Apple_Terminal, iTerm.app, vscode, WarpTerminal), `TMUX`, `STY`, `SSH_TTY`.
`doctor`'s `terminal-emulator` check prints the interpretation.

### Performance

| Symptom | Cause | Fix |
|---|---|---|
| Edits feel sluggish, no errors anywhere | Latency regression; feel is not measurement | `./scripts/doctor.sh --bench` reports p50/p90/p99 for the read path against the 100ms budget. M0 baseline: read 25-33ms, write ~13ms |
| ONNX inference several times slower than reported numbers | x86 machine without AVX2 | Check `cpu-features` in doctor; use a smaller model or better hardware |
| Model download fails mid-stream | Disk full | `disk-space` check; free 4 GiB |

## Debugging something that only reproduces in one application

This will happen constantly: accessibility support is per-app, per-framework,
per-version. The procedure:

1. **Reproduce with the harness, not the product.** `spike-cli watch 500`,
   then click into the problem app. You now see exactly what the AX layer
   reports for its focused field, refreshed twice a second.
2. **Separate "no tree" from "wrong tree".** `spike-cli inspect "<App>"`
   walks the app directly. Zero *windows* means Space visibility or Chromium
   opt-in, not the app's text system. Windows but zero *fields* means the
   app's framework does not expose its text: a genuinely different problem.
3. **Compare against a known-good app of the same family.** TextEdit for
   AppKit, Safari for web content, VS Code for Electron. If the family
   baseline works and the app does not, the difference is app-specific
   configuration (Electron flags, custom widgets), not our code.
4. **Check what the field admits to supporting.** `probe` prints
   `value_settable` / `selected_text_settable`. An app that reads fine but
   refuses writes is not broken; it needs the paste fallback.
5. **Use Apple's own reference.** Xcode's Accessibility Inspector shows the
   tree macOS itself sees. If the Inspector cannot see the text either, no
   amount of our code will; the app must fix its AX support (or we paste).
6. **Only then suspect our code.** If the Inspector sees text that `ax-edit`
   does not, that is a Bug-class failure: file it with `doctor --report`
   output (which is redacted by construction) and the app name and version.

## Bug reports

`./scripts/doctor.sh --report` prints a pasteable bundle. Transcribed text,
clipboard contents, window titles, and file paths are redacted mechanically,
because this tool sees everything the user types; do not "helpfully" paste raw
logs around it. Only Bug-class failures belong in the issue tracker, and the
report counts them for you.
