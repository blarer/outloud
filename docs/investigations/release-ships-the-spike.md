# The macOS release ships the wrong binary

Found 2026-08-07 while answering "what's the difference between OutLoudSpike
and OutLoud".

## What is wrong

`scripts/build-macos-release.sh` is what the release workflow runs for
macOS. It packages **`spike-cli`**, not `outloud`.

`spike-cli` is the M0 development harness. Its own module doc says it: *"It
does no speech recognition; it isolates the OS integration risk."* It has
three subcommands — `probe`, `watch`, `replace` — and cannot dictate.

So the DMG that pipeline produces is a diagnostic tool, not the product.
Anyone who downloads it and expects to dictate finds an app that cannot.

| | `OutLoud` | `OutLoudSpike` |
|---|---|---|
| Binary | `outloud` | `spike-cli` |
| Speech recognition | yes | **no** |
| Bundle ID | `dev.outloud.outloud` | `dev.hexavoice.spike` |
| Built by | `scripts/bundle-outloud-macos.sh` | `scripts/build-macos-release.sh` |
| Reaches a release | no | yes |

Linux packages only `spike-cli` too, but names its artifacts
`outloud-spike`, which is at least honest, and the README already says
"Linux does not work yet". macOS is the misleading one: it calls the
harness `OutLoudSpike` inside a DMG, on the one platform where the real
product works.

**Windows already does the right thing.** `build-windows.sh` builds both
packages and ships `outloud.exe` alongside `outloud-spike.exe`. That is the
precedent to copy, and it means the fix is a small edit rather than a
redesign.

## How it stayed hidden

The macOS release job passes. It builds successfully, signs successfully,
produces a valid DMG, and uploads it. Every check is green because every
check is asking "did the build succeed", not "did it build the product".

This is the same shape as the other defects found this week: a value
computed correctly that never reaches the user. The pipeline is honest about
what it did and silent about what it was for.

Corroborating evidence: the `OutLoud-macos-arm64.tar.gz` on the current
release page is not produced by any script in this repo. It was built and
uploaded by hand — which is what you would expect if the automated path
produced the wrong artifact and somebody worked around it once.

## One mitigation already in place

`publish` creates the release as a **draft** ("a human sanity-checks
artifacts before publishing"). So this defect cannot reach the public
without someone looking first. That is why the current release page has a
hand-built tarball on it: a human did look, saw the artifact was wrong, and
worked around it.

It also means the fix is not urgent, only overdue. What it costs today is a
manual step on every release, forever, plus the risk that the next person to
look does not know `OutLoudSpike` is not the product.

The release body is also empty: every artifact is uploaded with no note
saying which one a user should download. Worth fixing alongside, whichever
option is chosen.

## Why this is not fixed yet

Which binary a release ships is a product decision, not a mechanical one:

1. **Ship `outloud` instead of `spike-cli`.** The DMG becomes the real app.
   Matches Windows. Probably right.
2. **Ship both**, exactly as Windows does.
3. **Drop the spike from releases** if it is dead scaffolding, and keep it as
   a dev-only tool built from source.

(2) is the smallest change and loses nothing. (3) is the tidiest if the
harness has served its purpose — M0 is long finished — but that is a call
about whether anyone still uses `probe`/`watch`/`replace` for app-compat
testing, and `scripts/test-real-apps.sh` and `scripts/probe-app.sh` both
still drive it.

Awaiting a decision rather than guessing, because guessing here changes what
strangers download.

## When fixing

- `build-macos-release.sh` also names the app `OutLoudSpike` and uses bundle
  ID `dev.hexavoice.spike`. Both would need to change with the binary, and
  `dev.hexavoice.*` is the LEGACY identifier the daemon's bundler already
  resets stale TCC grants for (see `bundle-outloud-macos.sh`).
- Verify by mounting the produced DMG and running
  `Contents/MacOS/<name> --version`, not by reading the script. The whole
  point of this note is that reading it is what went wrong.
