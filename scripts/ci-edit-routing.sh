#!/usr/bin/env bash
# Does "scratch that" still reach the undo ring?
#
# The GUI-free half of scripts/verify-undo-live.sh, split out so CI can run
# it. That script drives TextEdit and System Events through AppleScript,
# which needs a logged-in GUI session; a GitHub macOS runner has none, so it
# hangs there until the job times out. Discovered by shipping exactly that
# job and watching it burn four minutes producing no output at all.
#
# What survives the split is the part that never needed a window:
# OUTLOUD_NO_INJECT=1 makes the daemon run the whole pipeline (recognizer,
# intent parse, edit routing) and report the route it WOULD have taken,
# writing nothing. "scratch that" reaching the undo ring is observable as
# "[route: undo]" with no keystroke and no focused field.
#
# This is why that reporting exists. The undo ring shipped complete, tested,
# and wired to nothing, because the only way to observe which branch an edit
# took was to speak into a live window and watch. Every automated path
# stopped at an early return.
#
# What this canNOT prove: that the routed text reaches an application. That
# needs a real window and is what verify-undo-live.sh is for.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
BIN="${1:-./target/release/outloud}"

if [[ ! -x "$BIN" ]]; then
  echo "no binary at $BIN (cargo build --release -p outloud)" >&2
  exit 1
fi

# Belt and braces. The daemon suppresses delivery in the presence of this
# variable, and every invocation below also passes --no-overlay, but this
# script must be safe to run on a developer's machine by accident.
export OUTLOUD_NO_INJECT=1

pass=0
fail=0

# route <phrase> <expected route label>
#
# On a CI runner nothing is focused, so the daemon takes the Mode::Dictate
# path. That path reports an undo route too, deliberately: our own write
# consumes the selection, so by the time the user says "scratch that" the
# field holds a caret and no selection, and gating undo on Mode::Edit made
# it unreachable in exactly the sequence it exists for (commit 0590d7e,
# "'scratch that' was typed into the document instead of undoing").
#
# So this exercises the no-selection path, which is the one a real user hits
# second and the one that regressed before. The with-selection path is
# covered by route_edit's unit tests and by verify-undo-live.sh.
route() {
  local phrase="$1" want="$2" out
  out="$("$BIN" --once --say "$phrase" --no-overlay 2>&1 || true)"
  if grep -q "\[route: $want\]" <<<"$out"; then
    echo "    PASS ${phrase@Q} -> $want"
    pass=$((pass + 1))
  else
    echo "    FAIL ${phrase@Q} -> expected [route: $want]"
    sed 's/^/         | /' <<<"$out" | grep -E "e2e|route" | head -3
    fail=$((fail + 1))
  fi
}

echo "==> edit routing with no selection, delivery suppressed"

# The ones this whole exercise exists for. If these stop reaching the ring
# they silently become "that command did not match", or worse, get typed
# into the document as literal text -- which is the bug 0590d7e fixed.
route "scratch that" "undo"
route "undo that" "undo"

# A phrase that merely CONTAINS the trigger words is not a command: undo
# matches the whole utterance, not a prefix. Ordinary dictation carries NO
# route tag at all (only an edit-shaped phrase is routed), so the assertion
# is the absence of one. Pinning this keeps the parser from being loosened
# into eating normal speech, which would silently swallow a sentence
# instead of typing it.
no_route() {
  local phrase="$1" out
  out="$("$BIN" --once --say "$phrase" --no-overlay 2>&1 || true)"
  if grep -q "\[route:" <<<"$out"; then
    echo "    FAIL ${phrase@Q} -> should be plain dictation, but was routed"
    sed 's/^/         | /' <<<"$out" | grep -E "e2e" | head -2
    fail=$((fail + 1))
  else
    echo "    PASS ${phrase@Q} -> plain dictation, not a command"
    pass=$((pass + 1))
  fi
}
no_route "scratch that idea it was wrong"

echo
echo "passed: $pass   failed: $fail"
echo
echo "NOTE: proves the ROUTE, not delivery. Only verify-undo-live.sh can"
echo "prove the text reaches an application, and that needs a GUI session."
echo "The with-selection routing is covered by route_edit's unit tests."
[[ "$fail" -eq 0 ]]
