#!/usr/bin/env bash
# Live verification of edit-by-voice UNDO ("scratch that"), against TextEdit.
#
# Why a script and not a unit test: the undo ring shipped complete, tested,
# and wired to nothing for weeks. Every piece had passing tests; nothing
# proved that a spoken "scratch that" reached the ring and put the user's
# text back. The unit tests now cover the routing and the ring's decision.
# This covers the half they cannot: that the decision reaches a real
# application and leaves the right characters in the document.
#
# Both utterances go through ONE process, via repeated --say. That is not a
# convenience: the undo ring is process-lifetime, because undo spans
# utterances by definition (the dictation being undone finished before the
# one asking for the undo began). A verification built on two separate
# `--once` runs would pass while never exercising undo at all, since the
# second process's ring is empty.
#
# TextEdit ONLY, in a scratch document. The daemon writes into whatever is
# focused, so this focuses TextEdit and reads the document back through the
# scripting interface rather than off the screen.
#
# Usage: scripts/verify-undo-live.sh [path/to/outloud]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
BIN="${1:-./target/release/outloud}"

if [[ ! -x "$BIN" ]]; then
  echo "no binary at $BIN (cargo build --release -p outloud)" >&2
  exit 1
fi

# REFUSE TO RUN WITHOUT ACCESSIBILITY.
#
# Without the grant every write falls back to clipboard-paste, which needs a
# Cmd+V that also needs the grant, so nothing reaches the document at all.
# The checks below then compare "unchanged document" against "unchanged
# document" and PASS while proving nothing.
#
# That is not hypothetical: the first run of this script reported 2 passed,
# 0 failed against a binary with no grants, and the only clue was a refusal
# line buried in the daemon's own output. A verification that cannot fail is
# worse than none, because it is believed.
perms="$("$BIN" --permissions 2>&1 || true)"
if ! grep -q "^accessibility: *granted" <<<"$perms"; then
  {
    echo "REFUSING TO RUN: this binary has no Accessibility grant."
    echo
    sed "s/^/    /" <<<"$perms"
    echo
    echo "Every write would silently fall back and this script would pass"
    echo "without testing anything. Run the signed bundle instead:"
    echo
    echo "    scripts/verify-undo-live.sh ./dist/OutLoud.app/Contents/MacOS/OutLoud"
    echo
    echo "If that bundle predates your changes, rebuild it with"
    echo "scripts/bundle-outloud-macos.sh -- note that an ad-hoc signature"
    echo "means macOS will ask you to re-approve Accessibility afterwards."
  } >&2
  exit 1
fi

ORIGINAL="the quick brown fox"
EDITED="the slow brown fox"

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

pass=0
fail=0

check() {
  local label="$1" got="$2" want="$3"
  if [[ "$got" == "$want" ]]; then
    echo "    PASS $label"
    pass=$((pass + 1))
  else
    echo "    FAIL $label"
    echo "         want: $want"
    echo "         got:  $got"
    fail=$((fail + 1))
  fi
}

open_doc

# --- 0. the harness itself has to be able to fail ---------------------------
#
# "undo restored the original" and "nothing happened at all" leave the
# document in the SAME state, so that check alone passes against a binary
# where undo does not exist. It did: an older bundle, built before any of
# this work and without repeatable --say, reported PASS.
#
# So first prove the edit lands. If this fails, the run is not telling us
# anything about undo and says so instead of scoring a pass.
echo "--- the harness can observe a write at all"
set_text "$ORIGINAL"
select_all
"$BIN" --once --say "change quick to slow" --no-overlay 2>&1 | sed 's/^/    | /' || true
sleep 0.8
edited="$(read_text)"
echo "    after:  $edited"
check "a spoken edit reached the document" "$edited" "$EDITED"
if [[ "$edited" != "$EDITED" ]]; then
  echo
  echo "ABORTING: the edit never landed, so an unchanged document after" >&2
  echo "\"scratch that\" would prove nothing. Fix delivery first." >&2
  exit 1
fi

# --- 1. edit then undo, in one process --------------------------------------
echo "--- an edit, then \"scratch that\", restores the original"
set_text "$ORIGINAL"
select_all
echo "    before: $(read_text)"

# No OUTLOUD_NO_INJECT: this run must reach the transport, which is the
# whole point, and is why TextEdit is focused above. The selection is made
# once; the edit replaces it, and TextEdit leaves the written text selected,
# so the second utterance still sees a selection and routes as an edit.
out="$("$BIN" --once \
  --say "change quick to slow" \
  --say "scratch that" \
  --no-overlay 2>&1)" || true
sed 's/^/    | /' <<<"$out"
sleep 1.0

# Both utterances must actually have run. A binary without repeatable --say
# silently drops the second one, and then "the document still holds the
# original" is true because nothing was ever edited.
spoken="$(grep -c 'via ' <<<"$out" || true)"
if [[ "$spoken" -lt 4 ]]; then
  echo "    FAIL both utterances must reach the transport (saw $spoken/4 lines)"
  fail=$((fail + 1))
fi

after="$(read_text)"
echo "    after:  $after"
check "undo restored the original text" "$after" "$ORIGINAL"

# --- 2. undo with nothing recorded ------------------------------------------
echo "--- \"scratch that\" with an empty ring must not write the phrase"
set_text "$ORIGINAL"
select_all
"$BIN" --once --say "scratch that" --no-overlay 2>&1 | sed 's/^/    | /' || true
sleep 0.8
after_empty="$(read_text)"
echo "    after:  $after_empty"
check "document untouched by an unbacked undo" "$after_empty" "$ORIGINAL"

echo
echo "passed: $pass   failed: $fail"
[[ "$fail" -eq 0 ]]
