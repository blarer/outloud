#!/usr/bin/env bash
# Repoint the published release's install paths at `main`.
#
# WHY THIS IS A SCRIPT AND NOT SOMETHING ALREADY DONE: it mutates a live,
# public release that strangers are downloading right now. That is a
# maintainer's call, not an automated one. Everything it does is prepared,
# verified, and reversible, but it is not run for you.
#
# WHAT IS WRONG RIGHT NOW: release v2026.08.06-1649 offers two install
# paths, and both fetch from `refs/heads/overlay/cat-mascot`:
#
#   1. the release notes' curl one-liner
#   2. the Install-OutLoud.command asset, which fetches install.sh at runtime
#
# Both work today only because that branch still exists. Deleting a stale
# feature branch is routine housekeeping, and doing it breaks installation
# for everyone reading the release page. `scripts/install.sh` now exists on
# `main`, so nothing needs that branch any more.
#
# WHAT THIS DOES:
#   - rewrites the release notes to use the /main/ one-liner
#   - replaces the Install-OutLoud.command asset with the one from `main`
#
# Both are reversible: the previous notes are saved to a file first, and the
# old asset is a file in the release you can re-upload.
#
# Usage: scripts/fix-release-install-links.sh [TAG]
#   TAG defaults to the latest release.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

REPO="blarer/outloud"
TAG="${1:-}"

command -v gh >/dev/null 2>&1 || { echo "needs the gh CLI: https://cli.github.com" >&2; exit 1; }

if [ -z "$TAG" ]; then
    TAG="$(gh release view --repo "$REPO" --json tagName --jq .tagName)"
fi
echo "==> target release: $TAG"

# Keep the current notes before touching them.
BACKUP="dist/release-notes-$TAG.backup.md"
mkdir -p dist
gh release view "$TAG" --repo "$REPO" --json body --jq .body > "$BACKUP"
echo "    previous notes saved to $BACKUP"

if ! grep -q "refs/heads/" "$BACKUP"; then
    echo "==> notes already free of branch refs; nothing to rewrite"
else
    NEW="dist/release-notes-$TAG.new.md"
    sed 's|refs/heads/overlay/cat-mascot|main|g' "$BACKUP" > "$NEW"
    echo "==> rewriting the release notes"
    diff "$BACKUP" "$NEW" || true
    gh release edit "$TAG" --repo "$REPO" --notes-file "$NEW"
    echo "    done"
fi

# The asset fetches install.sh at runtime, so a stale copy keeps pointing at
# the branch even after the notes are fixed.
CMD="scripts/Install-OutLoud.command"
[ -f "$CMD" ] || { echo "$CMD missing" >&2; exit 1; }
grep -q "raw.githubusercontent.com/$REPO/main/" "$CMD" \
    || { echo "$CMD does not point at main; refusing to upload it" >&2; exit 1; }

echo "==> replacing the Install-OutLoud.command asset"
gh release upload "$TAG" "$CMD" --repo "$REPO" --clobber
echo "    done"

echo
echo "Verify with:"
echo "  gh release view $TAG --repo $REPO --json body --jq .body | grep -c refs/heads   # expect 0"
