#!/usr/bin/env bash
# Install OutLoud from the latest GitHub release.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/blarer/outloud/main/scripts/install.sh | bash
#
# WHY THIS EXISTS ON main: the published release pointed its install
# one-liner at a feature branch, and that branch is the only place the
# script lived. Deleting a merged or abandoned branch is routine
# housekeeping, and doing it would have broken installation for everyone
# reading the release page. An installer is public interface; it belongs on
# the default branch.
#
# Written against the facts of the published release, verified by
# downloading it rather than assumed:
#
#   - the asset is OutLoud-macos-arm64.tar.gz, not a DMG
#   - it unpacks to OutLoud.app containing the daemon AND the Swift
#     speech helper (outloud-speech-helper)
#   - it is ad-hoc signed, with no Team ID, so Gatekeeper will quarantine
#     it after a browser download
#   - LSMinimumSystemVersion is 13.0, but a usable recognizer needs 26+
#     unless a whisper model is configured
#
# Deliberately NOT silent: it prints what it is about to do to a directory
# the user owns, and it refuses rather than guessing when something is off.

set -euo pipefail

REPO="blarer/outloud"
ASSET="OutLoud-macos-arm64.tar.gz"
APP="OutLoud.app"
DEST="${OUTLOUD_INSTALL_DIR:-/Applications}"

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }

# --- refuse early, on facts, rather than failing halfway through ----------

[ "$(uname -s)" = "Darwin" ] || die "macOS only. Linux cannot type at all yet (every text tier needs key synthesis that does not exist there), and Windows has its own installer."

ARCH="$(uname -m)"
[ "$ARCH" = "arm64" ] || die "the published build is Apple Silicon only (this machine is $ARCH). Build from source: https://github.com/$REPO#install"

MACOS_MAJOR="$(sw_vers -productVersion | cut -d. -f1)"
if [ "$MACOS_MAJOR" -lt 13 ]; then
    die "needs macOS 13 or newer (this is $(sw_vers -productVersion))"
fi

command -v curl >/dev/null 2>&1 || die "curl not found"
command -v tar  >/dev/null 2>&1 || die "tar not found"

# --- find the latest release asset ---------------------------------------

say "==> asking GitHub for the latest OutLoud release"
API="https://api.github.com/repos/$REPO/releases/latest"

# Parsed with grep/sed rather than jq: jq is not installed by default on
# macOS, and requiring it would trade one broken install path for another.
URL="$(curl -fsSL "$API" \
    | grep -o "\"browser_download_url\": *\"[^\"]*${ASSET}\"" \
    | head -1 \
    | sed 's/.*"browser_download_url": *"//; s/"$//')"

[ -n "$URL" ] || die "no $ASSET in the latest release. See https://github.com/$REPO/releases"

TAG="$(printf '%s\n' "$URL" | sed 's|.*/download/||; s|/.*||')"
say "    found $TAG"

# --- download and unpack into a temp dir the user does not own -----------

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

say "==> downloading $ASSET"
curl -fsSL --retry 3 -o "$WORK/$ASSET" "$URL" || die "download failed"

say "==> unpacking"
tar -xzf "$WORK/$ASSET" -C "$WORK" || die "the archive did not unpack; the download may be truncated"
[ -d "$WORK/$APP" ] || die "no $APP inside the archive"

# The tarball is fetched by curl, so it carries no quarantine flag. Strip it
# anyway: if this script is ever run on a file that came through a browser,
# the app would refuse to launch with a message that names neither the cause
# nor the fix. Failure here is not fatal, since the common case has nothing
# to remove.
xattr -dr com.apple.quarantine "$WORK/$APP" 2>/dev/null || true

# --- install -------------------------------------------------------------

TARGET="$DEST/$APP"
if [ -e "$TARGET" ]; then
    say "==> replacing the existing $TARGET"
    # Quit a running copy first. Installing over a running app leaves the
    # old binary resident and the new one unlaunchable until a logout, and
    # only one copy may run at a time anyway.
    osascript -e 'tell application "OutLoud" to quit' >/dev/null 2>&1 || true
    sleep 1
    rm -rf "$TARGET" || die "could not remove $TARGET (try: sudo rm -rf '$TARGET')"
fi

say "==> installing to $TARGET"
mkdir -p "$DEST" || die "could not create $DEST"
cp -R "$WORK/$APP" "$TARGET" || die "could not copy into $DEST"

# --- tell the user the two things that will otherwise confuse them -------

say ""
say "Installed $TAG to $TARGET"
say ""
say "Two things it needs from you, and neither prompts loudly:"
say ""
say "  1. Accessibility and Input Monitoring, in System Settings > Privacy"
say "     & Security. Without them the menu bar glyph shows a warning"
say "     triangle and every keystroke silently fails."
say ""
say "  2. A recognizer. macOS 26+ has one built in. On 13-25 there is none,"
say "     and you will see 'recognizer never becomes ready' until you set"
say "     OUTLOUD_WHISPER_MODEL and run with --asr whisper."
say ""
if [ "$MACOS_MAJOR" -lt 26 ]; then
    say "  You are on macOS $(sw_vers -productVersion), so item 2 applies to you."
    say ""
fi
say "Open it with:  open '$TARGET'"
