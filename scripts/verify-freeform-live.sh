#!/usr/bin/env bash
# Live verification of the freeform-over-selection fix, against TextEdit.
#
# Why a script and not a unit test: the bug was a WRITE into a real
# application. The unit tests assert the decision, but only a live run
# proves that the decision reaches the transport, that the user's text
# survives, and that what did get written is undoable through the app's
# own Cmd+Z. Those three facts are what the bug report was about.
#
# TextEdit ONLY. The daemon writes into whatever is focused, so this
# script focuses TextEdit itself before every run and reads the document
# back through the same scripting interface, never through the screen.
#
# Usage: scripts/verify-freeform-live.sh [path/to/outloud]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
BIN="${1:-./target/release/outloud}"

if [[ ! -x "$BIN" ]]; then
  echo "no binary at $BIN (cargo build --release -p outloud)" >&2
  exit 1
fi

ORIGINAL="The customers might possibly be quite upset about this."

# --- TextEdit control -------------------------------------------------------

te() { osascript -e "tell application \"TextEdit\" to $1"; }

open_doc() {
  osascript >/dev/null <<'APPLESCRIPT'
tell application "TextEdit"
  activate
  if (count of documents) = 0 then make new document
end tell
APPLESCRIPT
}

set_text() {
  osascript >/dev/null <<APPLESCRIPT
tell application "TextEdit"
  activate
  set text of front document to "$1"
end tell
APPLESCRIPT
}

read_text() { te 'get text of front document'; }

select_all() {
  # Through the UI, not the scripting model: the daemon reads
  # AXSelectedText, and only a real selection sets it.
  osascript >/dev/null <<'APPLESCRIPT'
tell application "TextEdit" to activate
delay 0.3
tell application "System Events" to keystroke "a" using command down
delay 0.3
APPLESCRIPT
}

undo_once() {
  osascript >/dev/null <<'APPLESCRIPT'
tell application "TextEdit" to activate
delay 0.3
tell application "System Events" to keystroke "z" using command down
delay 0.5
APPLESCRIPT
}

# --- one case ---------------------------------------------------------------

pass=0
fail=0

# run_case <label> <spoken> <expectation: preserved|inserted>
run_case() {
  local label="$1" spoken="$2" expect="$3"
  set_text "$ORIGINAL"
  select_all

  local before after
  before="$(read_text)"
  echo "--- $label"
  echo "    before: $before"
  echo "    spoke:  $spoken"

  # No OUTLOUD_NO_INJECT here: this run must reach the transport. That is
  # the whole point, and it is why TextEdit is focused above.
  "$BIN" --once --say "$spoken" --no-overlay 2>&1 | sed 's/^/    | /' || true
  sleep 0.5

  after="$(read_text)"
  echo "    after:  $after"

  case "$expect" in
    preserved)
      # Exact equality, not a substring probe. With a large document the
      # phrase repeats, so "contains the phrase" would pass even if most
      # of the text had been replaced. The claim is that NOTHING was
      # written, and only equality states that.
      if [[ "$after" == "$before" ]]; then
        echo "    PASS: the document is byte-for-byte unchanged (${#after} chars)"
        pass=$((pass + 1))
      else
        echo "    FAIL: the user's text changed (${#before} -> ${#after} chars)"
        fail=$((fail + 1))
      fi
      ;;
    inserted)
      if [[ "$after" != "$before" ]]; then
        echo "    PASS: dictation still reached the document"
        # And it must be undoable, which is the recoverability claim.
        undo_once
        local undone
        undone="$(read_text)"
        if [[ "$undone" == *"customers might possibly be quite upset"* ]]; then
          echo "    PASS: Cmd+Z restored the original text"
          pass=$((pass + 2))
        else
          echo "    FAIL: Cmd+Z did not restore (got: $undone)"
          pass=$((pass + 1))
          fail=$((fail + 1))
        fi
      else
        echo "    FAIL: nothing was written; dictation regressed"
        fail=$((fail + 1))
      fi
      ;;
  esac
  echo
}

open_doc

# 1. The reported bug. Must refuse and leave the sentence alone.
run_case "rewrite request over a selection" "tighten this up" preserved
# 2. The regression the old behaviour existed to prevent. Must still write,
#    and the write must be undoable.
run_case "dictation with a stale selection" "we should tell them soon" inserted
# 3. The escape hatch. A false refusal must cost the user exactly one
#    retry, so the documented "type:" prefix has to write the literal
#    words even though they look like an instruction. Without this, case
#    1's refusal would be a dead end rather than a speed bump.
run_case "type: prefix overrides the refusal" "type: tighten this up" inserted

# 4. Blast radius. A handful of dictated words landing on a whole
#    document is a deletion, not an edit, and it would happen on a
#    reading we already know is uncertain. The document must survive.
# Built in bash rather than with python3: this script gets run from
# non-interactive contexts with no usable stdin, where python3 aborts in
# init_sys_streams before it can print anything.
LARGE=""
for _ in $(seq 1 40); do
  LARGE+="The customers might possibly be quite upset about this. "
done
ORIGINAL="${LARGE% }"
run_case "a few words over a large selection" "we should tell them soon" preserved

echo "pass=$pass fail=$fail"
[[ $fail -eq 0 ]]
