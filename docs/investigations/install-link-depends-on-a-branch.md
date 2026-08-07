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

## Why this is not just "merge the branch"

You said to disregard the cat-mascot branch: "the only changes on the other
branch are the cat". That is not what the branch contains. Of its 21
commits, six are about the cat glyph. The other fifteen are the entire
install-and-release story:

    feat: a curl installer and a release path that survive having no Developer ID
    feat: ship a double-clickable installer, because macOS blocks the pasted one
    feat: guide a first-time user through the two silent permissions
    fix: build releases in staging so they cannot replace the running app
    fix: never let a staging build claim the running app's bundle identifier
    fix: stop this script from silently replacing the developer's running app
    fix: remove the staged bundle, and its registration, when the release ends
    fix: show OutLoud's own icon in the welcome dialogs, not a red folder
    fix: restart instead of insisting a correct switch is wrong
    test: exercise the installer before a stranger's Mac does
    test: compile every walkthrough dialog instead of eyeballing it
    ... and four more

Files that exist ONLY on that branch:

    scripts/install.sh
    scripts/Install-OutLoud.command
    scripts/release-macos.sh
    scripts/test-install.sh

Several of those fixes are about not destroying a running app during a
release, which is the kind of thing you want on `main` regardless of how you
feel about the cat.

## Why I did not act

Three options, and the choice is yours:

1. **Merge the branch.** Gets the installer onto `main` and the link stops
   depending on a feature branch. Brings the cat glyph with it, which now
   conflicts with the skull.
2. **Cherry-pick the fifteen non-cat commits** onto `main`, leave the cat
   behind. More work, keeps `main` free of a mark you did not choose.
3. **Leave it.** Works until someone deletes the branch.

I did not merge, because merging a branch you told me to disregard is
exactly the kind of thing that should not happen without you saying so, and
because it would reintroduce a cat glyph on top of the skull you just asked
for.

Note that this is also why my earlier "the release tarball was uploaded by
hand" finding looked odd: `scripts/release-macos.sh`, the thing that would
have produced it, is on that branch and not on `main`.
