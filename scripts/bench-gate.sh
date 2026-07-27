#!/usr/bin/env bash
# Run the latency regression gate against a real focused text field.
#
# Exit code is the gate's verdict: 0 when p50/p99 are inside budget (or the
# environment cannot measure, in which case the gate explains and skips),
# 1 when a percentile crossed its threshold. Wire this into CI on a macOS
# runner whose terminal holds the Accessibility grant.
set -euo pipefail
cd "$(dirname "$0")/.."

PREV_APP=$(osascript -e 'tell application "System Events" to get name of first process whose frontmost is true' || true)

osascript <<'EOF'
tell application "TextEdit"
    activate
    make new document with properties {text:"the quick brown fox jumps over the lazy dog"}
end tell
EOF
sleep 1

status=0
cargo bench -p ax-edit --bench gate || status=$?

osascript -e 'tell application "TextEdit" to close front document saving no' || true
if [ -n "${PREV_APP:-}" ]; then
    osascript -e "tell application \"System Events\" to set frontmost of process \"$PREV_APP\" to true" || true
fi
exit "$status"
