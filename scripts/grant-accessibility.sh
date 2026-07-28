#!/usr/bin/env bash
# Open the exact System Settings pane needed to grant Accessibility access,
# and reveal the app bundle in Finder so it can be dragged straight in.
#
# macOS deliberately gives no programmatic way to grant this permission: it must
# be a human action. The best a tool can do is remove every step of friction
# around that action, which is what this script does. The shipping product will
# need the same onboarding flow, so it is worth getting right now.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT/dist/OutLoud.app"

if [[ ! -d "$APP_DIR" ]]; then
    # Must be the OutLoud bundler, not scripts/bundle-macos.sh: that one builds
    # the spike-cli development harness as dist/AquaSpike.app, so calling it
    # here announced "Building it first", produced a different app, and then
    # asked the user to drag a bundle that did not exist.
    echo "App bundle not found. Building it first."
    "$ROOT/scripts/bundle-outloud-macos.sh"
fi

BIN="$APP_DIR/Contents/MacOS/OutLoud"

# Ask the binary itself whether it already holds the permission, so a repeat run
# is a no-op rather than a pointless trip through Settings.
if "$BIN" probe >/dev/null 2>&1; then
    echo "Accessibility permission is already granted. Nothing to do."
    exit 0
fi

echo "Accessibility permission is required."
echo
echo "Opening the Accessibility pane and revealing the app in Finder."
echo "Drag OutLoud.app from the Finder window into the Accessibility list,"
echo "then make sure its toggle is on."
echo

open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
open -R "$APP_DIR"

echo "Waiting for the permission to be granted (Ctrl-C to cancel)."
for _ in $(seq 1 120); do
    if "$BIN" probe >/dev/null 2>&1; then
        echo
        echo "Permission granted. Verifying with a live probe:"
        echo
        "$BIN" probe
        exit 0
    fi
    sleep 1
done

echo
echo "Timed out waiting for the grant. Re-run this script once the toggle is on."
exit 1
