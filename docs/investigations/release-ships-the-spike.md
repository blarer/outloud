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

## Follow-up: on Linux, `outloud` cannot type at all

Before the "ship `outloud` instead" option could be taken seriously, the
obvious question is whether `outloud` even works on the platforms where the
spike is currently shipped. Checked rather than assumed.

It **compiles** for Linux: `scripts/ci-check-linux.sh` passes, which
clippy-builds `-p outloud` for `x86_64-unknown-linux-gnu`.

It **cannot deliver a single character** there. Every text tier is gated to
macOS or Windows and returns `Unsupported` on Linux:

| tier | Linux behaviour |
|------|-----------------|
| synthetic keys | `Err(Unsupported)` -- "CGEvent synthesis exists only on macOS display builds" / "SendInput exists only on Windows display builds" |
| accessibility  | `Err(Unsupported)` |
| IME            | `Err(Unsupported)` |
| clipboard      | sets the clipboard, then `send_paste_keystroke()` returns `Unsupported`: "paste keystroke synthesis needs the synthetic-keys tier on this platform" |

The clipboard tier is the interesting one. Its `insert` is not cfg-gated, so
it looks cross-platform at a glance, and it *does* overwrite the user's
clipboard before discovering it cannot paste. On Linux that is strictly
worse than refusing: the text never arrives and the clipboard is gone.

### What this does to the options

"Ship `outloud` on Linux" is not a shipping decision, it is a feature
request: it needs an X11/Wayland key-synthesis backend that does not exist
yet (`ax-edit/src` holds `macos.rs` and nothing else). So the real choices
are now:

1. **macOS**: ship `outloud`. It works there, and it is the product.
2. **Linux**: either keep shipping the spike, clearly labelled as a harness
   with no speech recognition, or stop publishing Linux artifacts until a
   delivery backend exists. Shipping a binary named `outloud` that silently
   eats the clipboard and types nothing would be the worst of the three.

Still not decided here: what to publish is a product call.

## Correction: the *published* download is the real product

Checked 2026-08-07 by downloading the latest release rather than reasoning
from the workflow. The distinction matters and I had blurred it.

The finding above is about `scripts/build-macos-release.sh`, the CI path.
That is still true: it packages `spike-cli` as `OutLoudSpike.app`.

But the release people actually download does **not** come from that path.
`v2026.08.06-1649` carries two assets, and its DMG is not among them:

    Install-OutLoud.command
    OutLoud-macos-arm64.tar.gz

That tarball holds the real thing:

    $ tar -xzf OutLoud-macos-arm64.tar.gz
    $ ./OutLoud.app/Contents/MacOS/OutLoud --version
    outloud 0.1.0
    $ ./OutLoud.app/Contents/MacOS/OutLoud --help
    outloud: hold the hotkey, speak, release, text appears

`OutLoud.app/Contents/MacOS/` contains `OutLoud` and
`outloud-speech-helper` -- the daemon and its Swift recognizer. Nobody
downloading this release gets the spike.

So the correct statement is narrower than the title of this file: **the CI
release pipeline builds the wrong macOS artifact, and that artifact is not
what was published.** The published one was produced out-of-band by
`scripts/release-macos.sh` (which lives only on the cat-mascot branch, and
so is not on `main`).

That reframes the severity. It is not "users are downloading a tool that
cannot dictate" -- they are not. It is "the automated pipeline cannot
produce a shippable macOS build, so releases depend on a script that is not
on `main`". Still worth fixing, but it is a release-engineering gap rather
than a live user-facing defect.

Linux is unchanged and is the genuine user-facing problem: its packages contain
`spike-cli` under the name `outloud-spike`, and `outloud` itself cannot type
on Linux at all (see the section above).
