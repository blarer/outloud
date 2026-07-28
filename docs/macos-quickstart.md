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
e2e: release->text 437ms (finalize 120ms, inject 316.6ms) via clipboard-paste | "Change the widget to a gadget."
```

`via clipboard-paste` means the accessibility write path was refused, which is
normal when the focused window is a terminal and expected until step 3 is done.
`via macos-ax` means the in-place path worked, which is what edit-by-voice needs.

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
