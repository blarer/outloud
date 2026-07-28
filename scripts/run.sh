#!/usr/bin/env bash
# Run the spike through LaunchServices and stream its output back to the terminal.
#
# Why this wrapper exists: macOS attributes an Accessibility grant to a
# process's *responsible* process. A binary run straight from a shell inherits
# the terminal as its responsible process, so the system checks the terminal's
# permission and ignores the binary's own grant entirely. That is why the app
# can be toggled on in System Settings and still be denied.
#
# Launching the bundle with `open` makes the app responsible for itself, which
# is exactly how a user will start the shipping product. The trade-off is that
# LaunchServices detaches the process, so the binary mirrors its output to a log
# file (OUTLOUD_SPIKE_LOG) that this script tails.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT/dist/AquaSpike.app"
LOG="${TMPDIR:-/tmp}/outloud-spike-$$.log"

if [[ ! -d "$APP_DIR" ]]; then
    "$ROOT/scripts/bundle-macos.sh" >/dev/null
fi

: > "$LOG"

# `--wait-apps` would block until the app quits, which is what we want, but it
# also swallows the exit status, so the status is carried through the log.
open -a "$APP_DIR" --env "OUTLOUD_SPIKE_LOG=$LOG" --args "$@"

# Wait for the run to finish, signalled by the sentinel the binary writes last.
for _ in $(seq 1 200); do
    if grep -q '^__EXIT__' "$LOG" 2>/dev/null; then
        break
    fi
    sleep 0.1
done

exit_code="$(grep '^__EXIT__' "$LOG" 2>/dev/null | tail -1 | sed 's/^__EXIT__//')"
grep -v '^__EXIT__' "$LOG" || true
rm -f "$LOG"

exit "${exit_code:-0}"
