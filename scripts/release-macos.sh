#!/usr/bin/env bash
# Build and publish the macOS release the curl installer downloads.
#
# WHY this is separate from .github/workflows/release.yml: that workflow signs
# with a Developer ID certificate and notarizes, and neither is available on a
# free Apple account. It produces a .dmg that a browser download would mark
# with quarantine, which macOS refuses to open for an ad-hoc signature with
# "OutLoud is damaged and can't be opened" — a message that names the wrong
# cause and has no override button.
#
# This script produces the artifact scripts/install.sh expects: a plain
# tarball, fetched by curl, which sets no quarantine flag at all.
#
# Usage:
#   scripts/release-macos.sh            # build and upload to a new release
#   scripts/release-macos.sh --dry-run  # build and verify, upload nothing

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DRY_RUN=0
[[ "${1:-}" == "--dry-run" ]] && DRY_RUN=1

ASSET="OutLoud-macos-arm64.tar.gz"

# Build somewhere that is NOT dist/OutLoud.app.
#
# dist/OutLoud.app is the developer's daily driver: launched, granted
# permissions, left running. The bundler writes there by default, so this
# script used to replace the app they were using with whatever branch was
# checked out. It did exactly that from the cat branch, and nothing in the
# output connected a "dry run" to their menu bar icon changing.
#
# A separate staging directory removes the hazard rather than warning about
# it. The artifact is identical; only the path differs.
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
STAGE="dist/release-staging"
APP="$STAGE/OutLoud.app"

branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$branch" != "main" ]]; then
    # Still worth saying, because the BRANCH decides what the user downloads,
    # even though it can no longer damage the local install.
    echo "==> building the release from '$branch' (your own $PWD/dist/OutLoud.app is untouched)"
fi

[[ "$(uname -m)" == "arm64" ]] || {
    echo "error: this produces an arm64 build and must run on Apple Silicon" >&2
    exit 1
}

echo "==> Building the app bundle"
# OUTLOUD_KEEP_TCC: the bundler clears the local TCC grants after signing,
# which is right for a developer rebuild and wrong here. Publishing a release
# should not revoke the permissions on the machine doing the publishing.
# OUTLOUD_DIST_DIR: build into staging, NOT dist/OutLoud.app. Without this the
# bundler overwrites the app the developer is running, which is exactly the
# accident this script caused once already.
OUTLOUD_KEEP_TCC=1 OUTLOUD_DIST_DIR="$STAGE" scripts/bundle-outloud-macos.sh

[[ -d "$APP" ]] || { echo "error: $APP was not produced" >&2; exit 1; }

# The speech helper is a separate Swift binary, gitignored, and the bundler
# only WARNS when swiftc is missing. A release without it installs fine, runs
# fine, and never transcribes a word: exactly the silent failure this whole
# distribution path exists to prevent. Fail here instead.
helper="$APP/Contents/MacOS/outloud-speech-helper"
[[ -x "$helper" ]] || {
    echo "error: the speech helper is missing from the bundle." >&2
    echo "       Without it the app installs and runs but never transcribes." >&2
    exit 1
}

echo "==> Verifying the signature"
codesign --verify --deep --strict "$APP" || {
    echo "error: the bundle does not pass its own signature check" >&2
    exit 1
}

echo "==> Packaging $ASSET"
rm -f "dist/$ASSET"
# -C so the archive contains OutLoud.app at the root, which is what the
# installer's `mv "$tmp/$APP_NAME"` expects.
tar -czf "dist/$ASSET" -C "$STAGE" OutLoud.app

# Prove the archive round-trips before it is published. A truncated or
# wrongly-rooted tarball fails on the user's machine, where there is no way to
# diagnose it, and the failure looks like a broken app rather than a bad build.
echo "==> Verifying the archive round-trips"
verify="$(mktemp -d)"
# Remove the staged bundle on EVERY exit path, including failures.
#
# Skipping the LaunchServices registration is not enough on its own: macOS
# rescans disk on its own schedule and will re-register any .app it finds,
# which makes the staged copy a second claimant for this bundle identifier
# again, hours later, with nothing on screen connecting it to a release run.
# The only durable fix is to not leave a second bundle lying around.
# Unregister before deleting: LaunchServices keeps the record after the bundle
# is gone, so removing the directory alone leaves a phantom claimant pointing
# at a path that no longer exists, and the grant can still resolve to it.
cleanup() {
    [[ -d "$STAGE/OutLoud.app" ]] && "$LSREGISTER" -u "$STAGE/OutLoud.app" 2>/dev/null
    rm -rf "$verify" "$STAGE"
}
trap cleanup EXIT
tar -xzf "dist/$ASSET" -C "$verify"
[[ -x "$verify/OutLoud.app/Contents/MacOS/outloud" ]] || {
    echo "error: the archive does not contain a runnable app at its root" >&2
    exit 1
}
[[ -x "$verify/OutLoud.app/Contents/MacOS/outloud-speech-helper" ]] || {
    echo "error: the archive lost the speech helper" >&2
    exit 1
}
# The extracted copy must still verify: tar does not preserve extended
# attributes by default on every path, and a signature that survives locally
# but not through the archive would break only for the user.
codesign --verify --deep --strict "$verify/OutLoud.app" || {
    echo "error: the signature did not survive archiving" >&2
    exit 1
}

size="$(du -h "dist/$ASSET" | cut -f1)"
echo "==> $ASSET ($size) verified"

if (( DRY_RUN )); then
    echo "==> Dry run: nothing uploaded"
    exit 0
fi

command -v gh >/dev/null 2>&1 || {
    echo "error: gh is not installed; cannot publish" >&2
    exit 1
}

TAG="v$(date +%Y.%m.%d-%H%M)"
echo "==> Publishing $TAG"
# --latest matters: the installer downloads /releases/latest/download/, so a
# release that is not marked latest is invisible to every existing installer.
# The double-clickable installer ships as a release asset so the instruction
# can be "download this and open it" rather than a command to paste. macOS 26
# blocks pasting a curl-pipe-bash line into Terminal with "Possible Malware,
# Paste Blocked", which is the correct warning for that shape of instruction
# and a terrible first impression for a gift.
gh release create "$TAG" "dist/$ASSET" "scripts/Install-OutLoud.command" \
    --title "OutLoud $TAG" \
    --notes "### Install

Download **Install-OutLoud.command** below, then double-click it.

The first time, macOS may say it is from an unidentified developer. Right-click
the file and choose **Open**, then **Open** again.

Apple Silicon, macOS 26 or newer.

<details><summary>Terminal alternative</summary>

\`\`\`
curl -fsSL https://raw.githubusercontent.com/blarer/outloud/refs/heads/overlay/cat-mascot/scripts/install.sh | bash
\`\`\`

macOS 26 may block pasting this; the download above avoids that.
</details>" \
    --latest

echo "==> Published. The installer one-liner now serves this build."
