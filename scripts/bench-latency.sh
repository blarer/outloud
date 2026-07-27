#!/usr/bin/env bash
# Run the ax-edit latency benchmarks against a real focused text field.
#
# The benchmarks measure synchronous IPC into another process, so they need a
# real target: this script opens TextEdit with a known sentence, brings it to
# the front so its document owns keyboard focus, runs `cargo bench`, and then
# returns focus to wherever the operator was.
#
# The terminal this runs from must hold the Accessibility grant (the bench
# binary inherits it as the responsible process). If the benches print SKIP,
# grant the terminal in System Settings > Privacy & Security > Accessibility.
set -euo pipefail
cd "$(dirname "$0")/.."

FILTER="${1:-}"

# Remember the frontmost app so focus can be restored afterwards. Politeness
# matters: the bench run takes about a minute of stolen focus otherwise.
PREV_APP=$(osascript -e 'tell application "System Events" to get name of first process whose frontmost is true' || true)

# A fresh, known document: benchmarks that write AXValue back must not be
# aimed at a document the operator cares about.
osascript <<'EOF'
tell application "TextEdit"
    activate
    make new document with properties {text:"the quick brown fox jumps over the lazy dog"}
end tell
EOF

# Give the window server a moment to settle focus; benchmarking during the
# focus animation would sample a transitional state.
sleep 1

cargo bench -p ax-edit --bench ax_latency -- ${FILTER:+"$FILTER"}

# Close the scratch document without saving, and hand focus back.
osascript <<'EOF' || true
tell application "TextEdit" to close front document saving no
EOF
if [ -n "${PREV_APP:-}" ]; then
    # System Events handles processes (like terminal emulators) that do not
    # respond to a direct `tell application ... activate`.
    osascript -e "tell application \"System Events\" to set frontmost of process \"$PREV_APP\" to true" || true
fi
