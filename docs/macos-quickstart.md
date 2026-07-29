# macOS quickstart: from clone to talking at your Mac

Copy-pasteable path to a working dictation setup on macOS, and the exact
permission dialogs you must approve. Written because every failure in this
category looks like a bug and is almost always a permission.

Verified on macOS 26.5.2, Apple silicon, on 2026-07-28.

## 1. Build and check

```bash
cd outloud-spike
cargo build --workspace
cargo build -p outloud --release
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

## 2. Prove the pipeline without touching a microphone

No permission is needed for these: they synthesize audio, run the recognizer,
and inject through whatever transport the focused app allows.

```bash
# Ask the accessibility layer what it can see and how it would write.
cargo run -p spike-cli -- probe
cargo run -p spike-cli -- target

# One full dictation cycle from synthesized speech.
./target/release/outloud --once --say "the rain in spain falls mainly on the plain" --no-overlay

# Same, from a WAV file. Make one with the system voice:
say -o /tmp/outloud.aiff "change the widget to a gadget"
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/outloud.aiff /tmp/outloud.wav
./target/release/outloud --once --wav /tmp/outloud.wav --no-overlay
```

Each `--once` run prints a line like:

```
e2e: release->text 147ms (finalize 131ms, inject 16.5ms) via set-value | "Regression check for normal text fields."
```

The `via` field names the transport that actually delivered the text:

| `via` | What it means |
|---|---|
| `set-value` / `selected-text` | The accessibility path. In place, undo preserved. What you want in a normal text field |
| `synthetic-keys` | Typed as key events. What terminals get, because a terminal exposes no writable field. Leaves your clipboard alone |
| `clipboard-paste` | Fallback when neither of the above worked |

Measured on this machine: TextEdit 147ms end to end (16ms inject),
Terminal.app 177ms (45ms inject).

## Upgrading from Aqua

This product used to be called Aqua. If you ever installed it under the old
name, do these three things once. Skipping the first is the one that will
waste your afternoon.

1. **Remove the stale `Aqua` entry from Accessibility.** macOS attaches a
   permission to a bundle identifier, and ours changed
   (`dev.aquaoss.aquad` to `dev.hexavoice.hexad`). The old entry stays in the
   list, still switched on, pointing at an identifier nothing uses. It reads
   as "already granted" while nothing works.

   System Settings > Privacy & Security > Accessibility, select **Aqua**,
   click the **-** button. Do the same under **Microphone**. Then grant
   `OutLoud.app` as described below.

   ```bash
   # The equivalent from a terminal, if you prefer:
   tccutil reset Accessibility dev.aquaoss.aquad
   tccutil reset Microphone dev.aquaoss.aquad
   ```

2. **Your settings move themselves.** On first launch OutLoud copies
   `~/.config/aqua/config.toml` to `~/.config/outloud/config.toml` and says
   so. It copies rather than moves, so the original is still there if
   anything looks wrong. Nothing happens if you already have a config at the
   new path, and a config that does not parse is reported rather than
   promoted.

3. **`AQUA_*` environment variables still work.** The current spelling is
   `OUTLOUD_*`, and it wins if you set both, but nothing in your shell profile
   or CI config breaks. There is no deadline on this.

Delete the old `dist/Aqua.app` whenever you like; `scripts/uninstall-macos.sh`
knows about both names.

## 3. Grant the two permissions

Run the doctor first; it names the next action for anything wrong:

```bash
./scripts/doctor.sh
```

Then build the daemon as an app bundle and grant against **that**:

```bash
./scripts/bundle-outloud-macos.sh
open -R dist/OutLoud.app          # reveals it in Finder to drag into Settings
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
```

- **System Settings > Privacy & Security > Accessibility** — click `+`, choose
  `dist/OutLoud.app`, toggle it on. This is the permission that lets OutLoud read the
  selected text and rewrite it in place. Without it you get dictation only, via
  clipboard paste.
- **System Settings > Privacy & Security > Microphone** — macOS prompts on the
  first real capture; approve it. If you never see the prompt, add OutLoud here
  manually.

For the development harness (`spike-cli`) the equivalent is
`./scripts/bundle-macos.sh && ./scripts/grant-accessibility.sh`.

### The responsible-process trap

**A binary run straight from your shell inherits the terminal as its
responsible process.** macOS then checks *the terminal's* Accessibility
permission and ignores the binary's own grant entirely. The symptom is that
OutLoud appears in System Settings with its toggle on and every accessibility call
still fails. Full detail in [macos-permissions.md](macos-permissions.md).

Two ways out, pick one and be consistent:

```bash
# Preferred: launch through LaunchServices so the app is responsible for itself.
open -a "$PWD/dist/OutLoud.app"

# Or: grant Accessibility to your terminal (Terminal / iTerm / WezTerm) and keep
# running ./target/release/outloud directly. Convenient for development, but the
# grant then belongs to the terminal, so it changes meaning if you switch terminals.
```

The ad-hoc signature is pinned to the exact build's `cdhash`, so **after every
rebuild** the grant silently dies while the toggle still reads on:

```bash
tccutil reset Accessibility dev.outloud.outloud   # then re-grant
```

## 4. Actually dictate

```bash
open -a "$PWD/dist/OutLoud.app"    # runs in the background, no Dock icon
```

### What you will see: the menu bar item

OutLoud has no Dock icon and no window on purpose (`LSUIElement`): it writes
into whatever text field you are focused on, so it must never activate and
steal that focus. Its entire visible presence is **one icon at the right of
your menu bar**, which appears within a second of launch:

| Icon | State | Meaning |
|---|---|---|
| waveform | `idle` | Running, model resident, waiting for the hotkey |
| filled microphone | `listening` | **The microphone is hot right now** |
| waveform + magnifier | `transcribing` | Key released, finalizing |
| circular arrows | `model-loading` | Model paging in. Dictate anyway; audio buffers |
| slashed microphone | `no-permission` | Accessibility or Microphone is missing |
| warning triangle | `error` | Something failed; the menu says what |

Click the icon for the menu:

```
OutLoud: ready
─────────────────────────────
Hold right-option to dictate
Microphone: MacBook Pro Microphone
─────────────────────────────
✓ Pause Dictation
─────────────────────────────
Settings                    >
Edit Config File…
Open Vocabulary Folder…
─────────────────────────────
Run Diagnostics…
Reload Config
─────────────────────────────
Quit OutLoud
```

- **Pause Dictation** drops the hotkey without stopping the daemon. Paused
  means the microphone is never opened, not that audio is recorded and
  discarded.
- **Settings** holds the hotkey presets, and a switch for the floating
  overlay. It deliberately offers only settings that are actually
  implemented: `config.toml` lists every key the schema knows, and several of
  them are not wired to anything yet. A menu row that writes a key nothing
  reads would be a lie, so those live in the file until they work.
  Changing the hotkey needs a quit and reopen, and the menu says so.
- **Run Diagnostics** runs the same checks as `./scripts/doctor.sh`, but from
  inside the bundled app, so it reports on the permissions *OutLoud* has rather
  than the ones your terminal has. The report opens in your text editor.
- If a permission is missing, an extra item appears at the top of the menu
  that opens the exact System Settings pane you need. Accessibility and
  Microphone are separate grants and get separate rows.
- **Quit OutLoud** stops the daemon. It is the reason you no longer need
  `pkill`.

Every change writes straight into your `config.toml`, comments and all, and
edits you make in a text editor show up in the menu within a second. The menu
and the file are two views of the same thing; neither hides the other's edits.

The config file is created on first launch, fully commented, at
`~/.config/outloud/config.toml` (or `$XDG_CONFIG_HOME/outloud/config.toml`).

### Dictating

Then in any app:

1. Put the cursor where you want text.
2. **Hold right-option.** The overlay appears.
3. Speak.
4. **Release.** The text appears at the cursor.

Edit-by-voice: select some text, hold right-option, say
`change hello to goodbye`, release. The selection is rewritten in place.

To watch the logs instead of running detached, **quit the bundled app first**
from its menu bar item. This is the same daemon with its panel logged rather
than drawn, not a second mode: two copies would both bind the hotkey and both
open the microphone, so one utterance can be delivered twice.

```bash
./target/release/outloud --no-overlay    # needs Accessibility on your terminal
```

Run this way it has no menu bar item (it is not launched as a bundle), so
stop it with Ctrl-C.

Stop it with the menu bar item's **Quit OutLoud**, or if it is wedged:

```bash
pkill -f 'OutLoud.app/Contents/MacOS/OutLoud'
```

## 5. Shell command-line editing (optional)

```bash
cargo run --release -p shell-bridge -- install   # appends one guarded line to your rc file
cargo run --release -p shell-bridge -- serve     # run alongside the daemon
```

Type a command without running it, speak an edit, press `Ctrl-X Ctrl-A` to
apply and `Ctrl-X u` to undo through your shell's own undo.

## When something does not work

| Symptom | Cause | Fix |
|---|---|---|
| No icon in the menu bar after `open` | the app is not running, or the bar is full and macOS hid the item | `ps aux \| grep OutLoud.app`; if it is running, widen the bar (quit another menu bar app) or check with `cargo run -p overlay --bin status-demo` |
| `via clipboard-paste` on every run | no Accessibility grant, or the grant is on the wrong process | step 3, and mind the responsible-process trap |
| Toggle is on but everything is denied | rebuilt since granting (`cdhash` changed), or wrong responsible process | `tccutil reset Accessibility dev.outloud.outloud`, re-grant |
| Recognizer never becomes ready | macOS below 26, so no `SpeechTranscriber` | `--asr mock` to test wiring; a downloadable backend is stubbed |
| Records silence | Microphone permission | System Settings > Privacy & Security > Microphone |
| Anything else | run `./scripts/doctor.sh` | it classifies each failure as permission, configuration, or bug |

## Dictating into a terminal

This works, and it takes a different path from a normal text field. A terminal
exposes no writable accessibility field (its "field" is a character grid owned
by whatever program is running inside), so the text is delivered as synthesized
key events instead, exactly as if you had typed it. You will see
`via synthetic-keys` rather than `via set-value`.

Two consequences worth knowing:

- Your clipboard is not touched. The old paste-based fallback clobbered it.
- The text arrives at the shell prompt as normal input, so history, line
  editing, and your shell's own undo all behave normally. Nothing is executed:
  the transcript stops at the prompt and waits for you to press enter.

Terminal.app exposes its *scrollback* as a readable accessibility text area,
which is a trap: treating it as an editable field rewrites your visible shell
history with mangled text. OutLoud checks whether the field is actually writable
before touching it, so this cannot happen, but it is worth knowing if you are
adding a transport.
