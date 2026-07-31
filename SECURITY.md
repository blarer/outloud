# Security Policy

## Reporting a vulnerability

Please report security issues **privately**, not as a public GitHub issue.

Use GitHub's [private vulnerability reporting](https://github.com/blarer/outloud/security/advisories/new)
(the "Report a vulnerability" button on the Security tab). That opens a
private thread visible only to you and the maintainers.

Please include what you did, what happened, and what you expected. A short
reproduction is worth more than a long description.

This is a small project maintained by one person, so response time is
best-effort rather than a guarantee. Expect an acknowledgement within about a
week. If a report is valid and I cannot fix it quickly, I would rather say so
than leave you waiting.

## What this application can do

OutLoud is a local dictation tool. To do its job it holds two macOS
permissions that deserve scrutiny, because they are the same ones a keylogger
would want:

- **Input Monitoring** lets it see the hotkey. Without it the key never
  registers and nothing happens at all.
- **Accessibility** lets it read the focused text field and write into it.
  Without it text is delivered by clipboard paste instead of inserted in
  place.

Both are requested through the normal macOS permission system, and both can be
revoked in System Settings at any time. You can ask the app what it currently
holds:

```
OutLoud.app/Contents/MacOS/OutLoud --permissions
```

## What it does not do

There is no network code. The binary does not link CFNetwork or any other
networking framework, which you can check yourself on a build you compiled:

```
otool -L OutLoud.app/Contents/MacOS/OutLoud | grep -i network
```

Speech recognition runs on-device through Apple's `SpeechTranscriber`. No
audio, transcript, or telemetry is uploaded, because there is nothing in the
binary capable of uploading it.

Diagnostics are user-initiated only. `scripts/doctor.sh --report` prints a
redacted section intended for pasting into an issue; it is generated locally
and nothing is sent anywhere.

## Areas worth looking at

If you are reviewing this, these are the parts where a mistake would matter
most:

- **Text injection** (`crates/outloud/src/inject.rs`). Chooses between
  accessibility writes, synthetic keystrokes, and clipboard paste. A routing
  mistake here can deliver text to a window the user was not looking at.
- **Clipboard handling** (`paste_with_leading_space`). The clipboard is saved
  and restored around a paste. A failure to restore would silently destroy
  whatever the user had copied.
- **Shell bridge** (`crates/shell-bridge/`). A Unix domain socket that lets a
  shell pull a staged utterance, so dictation can edit a command line. The
  socket is created `0600` inside a `0700` directory, both asserted by tests,
  but it is the only IPC surface in the project and the most interesting thing
  to attack.
- **Focus handling**. The app records which application was focused at
  key-down and warns when focus moved before the text was written. Silent
  misdelivery is the failure mode this exists to prevent.

## Not vulnerabilities

These are known and documented, so please do not spend your time on them:

- **The app is ad-hoc signed.** It has no Apple Developer ID, so macOS
  Gatekeeper will warn on first launch and every rebuild silently voids the
  accessibility and input-monitoring grants. This is a funding gap, not a
  flaw, and it is why the project currently expects you to build from source.
- **Dictated text goes to whatever has focus.** That is the feature. The
  focus-moved warning exists because it can surprise you, not because the
  behaviour is wrong.
- **The recognizer is Apple's.** Accuracy problems, and anything about how
  `SpeechTranscriber` handles audio internally, belong to Apple rather than
  here.

## Supported versions

The project is pre-1.0 and only the `main` branch is supported. There are no
backports to older tags.
