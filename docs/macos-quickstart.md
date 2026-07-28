# macOS quickstart: from clone to talking at your Mac

Copy-pasteable path to a working dictation setup on macOS, and the exact
permission dialogs you must approve. Written because every failure in this
category looks like a bug and is almost always a permission.

Verified on macOS 26.5.2, Apple silicon, on 2026-07-28.

## 1. Build and check

```bash
cd aqua-oss-spike
cargo build --workspace
cargo build -p aquad --release
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
./target/release/aquad --once --say "the rain in spain falls mainly on the plain" --no-overlay

# Same, from a WAV file. Make one with the system voice:
say -o /tmp/aqua.aiff "change the widget to a gadget"
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/aqua.aiff /tmp/aqua.wav
./target/release/aquad --once --wav /tmp/aqua.wav --no-overlay
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

## 3. Grant the two permissions

Run the doctor first; it names the next action for anything wrong:

```bash
./scripts/doctor.sh
```

Then build the daemon as an app bundle and grant against **that**:

```bash
./scripts/bundle-aquad-macos.sh
open -R dist/Aqua.app          # reveals it in Finder to drag into Settings
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
```

- **System Settings > Privacy & Security > Accessibility** — click `+`, choose
  `dist/Aqua.app`, toggle it on. This is the permission that lets Aqua read the
  selected text and rewrite it in place. Without it you get dictation only, via
  clipboard paste.
- **System Settings > Privacy & Security > Microphone** — macOS prompts on the
  first real capture; approve it. If you never see the prompt, add Aqua here
  manually.

For the development harness (`spike-cli`) the equivalent is
`./scripts/bundle-macos.sh && ./scripts/grant-accessibility.sh`.

### The responsible-process trap

**A binary run straight from your shell inherits the terminal as its
responsible process.** macOS then checks *the terminal's* Accessibility
permission and ignores the binary's own grant entirely. The symptom is that
Aqua appears in System Settings with its toggle on and every accessibility call
still fails. Full detail in [macos-permissions.md](macos-permissions.md).

Two ways out, pick one and be consistent:

```bash
# Preferred: launch through LaunchServices so the app is responsible for itself.
open -a "$PWD/dist/Aqua.app"

# Or: grant Accessibility to your terminal (Terminal / iTerm / WezTerm) and keep
# running ./target/release/aquad directly. Convenient for development, but the
# grant then belongs to the terminal, so it changes meaning if you switch terminals.
```

The ad-hoc signature is pinned to the exact build's `cdhash`, so **after every
rebuild** the grant silently dies while the toggle still reads on:

```bash
tccutil reset Accessibility dev.aquaoss.aquad   # then re-grant
```

## 4. Actually dictate

```bash
open -a "$PWD/dist/Aqua.app"    # runs in the background, no Dock icon
```

Then in any app:

1. Put the cursor where you want text.
2. **Hold right-option.** The overlay appears.
3. Speak.
4. **Release.** The text appears at the cursor.

Edit-by-voice: select some text, hold right-option, say
`change hello to goodbye`, release. The selection is rewritten in place.

To watch the logs instead of running detached:

```bash
./target/release/aquad --no-overlay    # needs Accessibility on your terminal
```

Stop it with:

```bash
pkill -f 'Aqua.app/Contents/MacOS/Aqua'
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
| `via clipboard-paste` on every run | no Accessibility grant, or the grant is on the wrong process | step 3, and mind the responsible-process trap |
| Toggle is on but everything is denied | rebuilt since granting (`cdhash` changed), or wrong responsible process | `tccutil reset Accessibility dev.aquaoss.aquad`, re-grant |
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
history with mangled text. Aqua checks whether the field is actually writable
before touching it, so this cannot happen, but it is worth knowing if you are
adding a transport.
