#!/usr/bin/env bash
# One-line installer for OutLoud, meant to be piped from curl:
#
#   curl -fsSL https://raw.githubusercontent.com/blarer/outloud/main/scripts/install.sh | bash
#
# WHY a curl installer rather than a .dmg: the app is signed ad-hoc, because
# notarization needs a paid Developer ID certificate. macOS attaches a
# quarantine flag to anything a BROWSER downloads, and refuses to open an
# ad-hoc bundle carrying that flag with "OutLoud is damaged and can't be
# opened. You should move it to the Trash." That message is a lie about the
# cause and it is unrecoverable for a non-technical user: there is no
# "open anyway" button for it, unlike the milder unidentified-developer
# warning that right-click-Open bypasses.
#
# curl does not set the quarantine flag, so the same bundle fetched this way
# opens normally. The installer clears the attribute anyway (belt and
# braces, and it also covers a re-run over a previously quarantined copy).
#
# This script is also the only place that can check her machine BEFORE
# anything is installed. A .dmg cannot tell someone their macOS is too old;
# it can only fail afterwards, which reads as a broken app.

set -euo pipefail

REPO="blarer/outloud"
APP_NAME="OutLoud.app"
INSTALL_DIR="/Applications"
APP_PATH="$INSTALL_DIR/$APP_NAME"

# Colours only when attached to a terminal, so a piped log stays clean.
if [[ -t 1 ]]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'
    GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; RESET=""
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
warn() { printf '%s!%s %s\n' "$YELLOW" "$RESET" "$*"; }
die()  { printf '%s✗%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }
ok()   { printf '%s✓%s %s\n' "$GREEN" "$RESET" "$*"; }

say ""
say "${BOLD}OutLoud${RESET} — hold a key, talk, and your words appear."
say ""

# ---------------------------------------------------------------------------
# Check the machine before touching it.
#
# Each of these produces a DIFFERENT silent failure if it is wrong, and all
# three are invisible after the fact: the app installs, launches, shows its
# menu bar icon, and simply never types anything.
# ---------------------------------------------------------------------------

step "Checking this Mac"

[[ "$(uname -s)" == "Darwin" ]] || die "OutLoud is macOS only."

# Apple Silicon. A universal binary would avoid this, but the release build is
# arm64-only, and an Intel Mac would fail at exec with a message about a bad
# CPU type that explains nothing.
arch="$(uname -m)"
if [[ "$arch" != "arm64" ]]; then
    die "This build needs an Apple Silicon Mac (M1 or newer). This one is $arch."
fi

# macOS 26 is where SpeechTranscriber lives. Below it the hotkey works, the
# overlay appears, and no words ever arrive: the recognizer is simply absent.
# doctor reports this too, but by then it is already installed.
os_major="$(sw_vers -productVersion | cut -d. -f1)"
if (( os_major < 26 )); then
    die "OutLoud needs macOS 26 or newer for the speech recognizer.
    This Mac is on macOS $(sw_vers -productVersion).
    Update in System Settings > General > Software Update, then run this again."
fi

ok "Apple Silicon, macOS $(sw_vers -productVersion)"

# ---------------------------------------------------------------------------
# Stop a running copy first.
#
# Overwriting the bundle underneath a running process leaves the old code in
# memory, so the user "installs" an update and sees no change until a reboot.
# ---------------------------------------------------------------------------

if pgrep -x outloud >/dev/null 2>&1; then
    step "Closing the running copy"
    pkill -x outloud || true
    sleep 1
fi

# ---------------------------------------------------------------------------
# Download.
# ---------------------------------------------------------------------------

step "Downloading the latest release"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

url="https://github.com/$REPO/releases/latest/download/OutLoud-macos-arm64.tar.gz"
if ! curl -fL --progress-bar -o "$tmp/outloud.tar.gz" "$url"; then
    die "Could not download OutLoud. Check the internet connection and try again."
fi

tar -xzf "$tmp/outloud.tar.gz" -C "$tmp" \
    || die "The download was incomplete. Run this command again."

[[ -d "$tmp/$APP_NAME" ]] || die "The download did not contain $APP_NAME."

# ---------------------------------------------------------------------------
# Install.
# ---------------------------------------------------------------------------

step "Installing to $INSTALL_DIR"

# Replacing rather than merging: a leftover file from an older layout inside a
# merged bundle can win over the new one and produce a mixed-version app.
rm -rf "$APP_PATH"
mv "$tmp/$APP_NAME" "$APP_PATH" \
    || die "Could not write to $INSTALL_DIR."

# Belt and braces: curl leaves no quarantine flag, but a re-run over a copy
# that arrived by AirDrop or a browser would inherit one.
xattr -dr com.apple.quarantine "$APP_PATH" 2>/dev/null || true

# Register the bundle so System Settings resolves the identifier to THIS copy.
# Without it, a stale record from an older install can win, and the permission
# toggle then lands on a bundle that is not the one running: the switch reads
# as on while nothing works.
lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[[ -x "$lsregister" ]] && "$lsregister" -f "$APP_PATH" 2>/dev/null || true

ok "Installed"

# ---------------------------------------------------------------------------
# Launch. The app itself owns the permission walkthrough from here, because it
# can SEE whether each grant landed; a script can only tell someone to click
# and hope.
# ---------------------------------------------------------------------------

step "Starting OutLoud"
open "$APP_PATH"

say ""
say "${GREEN}${BOLD}Done.${RESET} OutLoud is running. Look for the cat in your menu bar, at the"
say "top-right of the screen, near the clock."
say ""
say "It will walk you through two permission switches, opening each screen for"
say "you. It needs both:"
say "  ${DIM}Input Monitoring${RESET}   to notice when you hold the dictation key"
say "  ${DIM}Accessibility${RESET}      to type the words into whatever app you are using"
say ""
say "(The microphone prompt appears by itself the first time you speak.)"
say ""
say "Then hold the ${BOLD}right Option key${RESET} (right of the space bar), say something,"
say "and let go. The words appear wherever the cursor is."
say ""
