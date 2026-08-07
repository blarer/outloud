# The public install link depends on a branch you asked me to disregard

Found 2026-08-07, while looking for adjacent risk after the release-binary
question.

## The live problem

The published release page (v2026.08.06-1649) tells people to install with:

    curl -fsSL https://raw.githubusercontent.com/blarer/outloud/refs/heads/overlay/cat-mascot/scripts/install.sh | bash

That is a **feature branch**, not `main`. And the script does not exist on
`main` at all:

    $ curl -fsSL .../refs/heads/main/scripts/install.sh
    curl: (22) The requested URL returned error: 404

So the documented install path works only while `overlay/cat-mascot`
survives. Deleting that branch, which is the normal fate of a merged or
abandoned feature branch, breaks installation for everyone reading the
release page. The `Install-OutLoud.command` asset has the same dependency.

## Ruled out: the cat branch does not touch main

Asked about this, the answer was unambiguous: the cat-mascot branch must not
touch `main`. That removes merging and cherry-picking, so neither is on the
table regardless of what else the branch happens to carry.

For the record, the branch does hold more than the cat -- six of its 21
commits are the glyph, the rest are installer and release work, and four
scripts exist only there (`install.sh`, `Install-OutLoud.command`,
`release-macos.sh`, `test-install.sh`). That is context for what has to be
rebuilt, not an argument to reconsider. It also explains an earlier oddity:
the release tarball looked hand-uploaded because `release-macos.sh`, the
script that would have produced it, is not on `main`.

## What is left

Two options, both clean of the branch:

1. **Write a fresh `scripts/install.sh` on `main` from scratch**, sharing no
   commits with the branch, then repoint the release one-liner at `main`.
   The install link stops depending on a branch that may be deleted.
2. **Leave it.** The published link keeps working until someone deletes
   `overlay/cat-mascot`, then installation breaks for everyone reading the
   release page.

Not started, because a new installer is new product surface and should be
asked for rather than assumed.

## Verified, not remembered

    $ curl .../refs/heads/main/scripts/install.sh              -> HTTP 404
    $ curl .../refs/heads/overlay/cat-mascot/scripts/install.sh -> HTTP 200 (209 lines)

## Resolved: a fresh installer now lives on `main`

Written from scratch on `main`, sharing no commits with the cat-mascot
branch, so the constraint holds. `scripts/install.sh` is built against the
facts of the published release, each verified by downloading it rather than
assumed:

  - the asset is `OutLoud-macos-arm64.tar.gz`, not a DMG
  - it unpacks to `OutLoud.app` carrying the daemon AND the Swift
    `outloud-speech-helper`
  - it is ad-hoc signed with no Team ID
  - Apple Silicon only; `LSMinimumSystemVersion` is 13.0, but a usable
    recognizer needs macOS 26+

It refuses early and by name on a platform it cannot serve, on the wrong
architecture, and on an unwritable destination, rather than failing halfway
through. It quits a running copy before replacing one, since installing over
a running app leaves the old binary resident.

`scripts/test-install.sh` runs it for real against the live release, into a
temp directory, never touching `/Applications`. It is wired into CI as the
`install` job on `macos-15`.

Proven by sabotage rather than by a green run:

| broken thing | result |
|---|---|
| removed the bundle removal, so reinstall merges | FAIL "the old bundle was merged into rather than replaced" |
| removed the architecture guard | FAIL "expected an architecture refusal" |
| deleted `scripts/install.sh` entirely (the original defect) | FAIL "scripts/install.sh is missing -- this is the exact defect this file guards" |

The last row is the point: the failure that started all of this now breaks
the build.

Still to do, and not doable from here: the published release notes for
`v2026.08.06-1649` still point at the branch. Editing them is a GitHub
action on a live public release, so it is left to the maintainer. The new
one-liner is:

    curl -fsSL https://raw.githubusercontent.com/blarer/outloud/main/scripts/install.sh | bash
