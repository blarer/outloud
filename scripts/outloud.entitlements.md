# Why `outloud.entitlements` exists, and why it has no comments

The entitlements plist next to this file is deliberately bare. The reasoning
lives here because **`codesign` cannot parse an entitlements file containing
XML comments**:

```
Failed to parse entitlements: AMFIUnserializeXML: syntax error near line 6
```

`plutil -lint` accepts the commented version, which makes this an easy trap:
the file validates as a property list and still breaks signing. Keep the
plist free of comments and put the explanation in this document.

## What the entitlements are for

The release signs with `--options runtime` (the hardened runtime), which
notarization requires. Hardened runtime flips several capabilities from
"allowed, prompt the user" to **"denied unless the binary declares the
entitlement"**. So a hardened bundle with no entitlements has *fewer*
abilities than an unhardened one, and loses them silently at the point of
use rather than at build or launch time.

| Entitlement | Why |
|---|---|
| `com.apple.security.device.audio-input` | Record audio. The product is dictation; without it the app opens an input device, gets nothing, and sits in `listening` for ever with no error. |
| `com.apple.security.automation.apple-events` | Identify the frontmost application so per-app formatting profiles apply. This is what `NSAppleEventsUsageDescription` in `Info.plist` describes to the user. |

Nothing else is listed. Every entitlement is attack surface and Apple
reviews them, so the file stays minimal.

**Deliberately absent: `com.apple.security.inherit`.** It only has meaning
inside the App Sandbox, which this app does not use, and the speech helper
does not need it anyway: the helper reads raw PCM from **stdin**
(`crates/asr/helper/transcriber.swift`: "raw little-endian f32 mono 16kHz
PCM, until EOF") and never opens an input device. The daemon owns the
microphone and the helper only sees samples it already captured. For the
same reason the helper is signed with the hardened runtime but **without**
these entitlements.

## The bug this fixes

Shipped on 2026-11-01 and reported as "it's not capturing my mic". The
bundle had `flags=0x10000(runtime)` and zero entitlements. `tccd` said so
plainly in the unified log:

```
Prompting policy for hardened runtime; service: kTCCServiceAppleEvents
requires entitlement com.apple.security.automation.apple-events
but it is missing
```

Three things made it hard to see:

1. **The app looked healthy.** Signature valid, bundle launched, recognizer
   ready, mic device opened and named in the log.
2. **`--permissions` reported everything granted.** Accessibility and Input
   Monitoring are granted through a different mechanism that hardened
   runtime does not gate, so the one diagnostic a user would run was
   accurate and useless. It does not report microphone at all.
3. **The dev loop could not reproduce it.** `bundle-outloud-macos.sh` never
   passes `--options runtime`, so the bundle a developer builds and tests is
   not hardened and works fine.

`build-macos-release.sh` now asserts, against the **signed artifact** rather
than against this file, that hardened runtime implies the audio entitlement.
That check was sabotage-tested against the broken bundle before the fix
landed, and it failed as intended.
