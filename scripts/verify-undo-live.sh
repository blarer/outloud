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
# --dry-run: exercise the whole harness (document setup, focus checks,
# read-back, comparison) with delivery suppressed, so nothing is typed
# anywhere.
#
# Worth having because the harness has its own bugs, and every one of them
# so far was found the expensive way: a run that reported PASS against a
# binary with no Accessibility grant, an open_doc that raced TextEdit's
# launch, a focus check that was made once and then assumed to hold. Each
# needed a real run to find, and a real run types on someone's machine.
#
# A dry run proves the plumbing without that cost. It cannot prove undo
# works -- suppression means no text moves -- so it says so rather than
# printing a pass.
DRY_RUN=0
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN=1
  shift
fi

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
# A dry run is exempt: the grant matters because an ungranted binary
# silently fails to write and the comparison then passes vacuously. A dry
# run writes nothing BY DESIGN and says so instead of claiming a pass, so
# the hazard does not exist there. Without this exemption the harness could
# never be exercised on a CI runner, which is the one place it can be
# checked without someone's keyboard being taken over.
perms="$("$BIN" --permissions 2>&1 || true)"
if [[ "$DRY_RUN" == "0" ]] && ! grep -q "^accessibility: *granted" <<<"$perms"; then
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
  # `activate` returns before TextEdit has finished launching, so the
  # document count read immediately after it can be 0 for an app that is
  # about to restore windows, or stale for one still starting. Waiting for
  # the process first, then verifying a document exists, removes both races.
  osascript >/dev/null 2>&1 <<'APPLESCRIPT'
tell application "TextEdit" to activate
delay 0.5
tell application "TextEdit"
  if (count of documents) = 0 then make new document
end tell
delay 0.3
APPLESCRIPT
  local docs
  docs="$(osascript -e 'tell application "TextEdit" to count documents' 2>/dev/null || echo 0)"
  if [[ "$docs" -lt 1 ]]; then
    echo "could not open a TextEdit document to test against" >&2
    exit 1
  fi
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

# Refuse to dictate unless TextEdit is frontmost AND has a document.
#
# The daemon writes into whatever is focused. Every step here focuses
# TextEdit first, but focus is not ours to keep: the user can click away, or
# close the window, between the focus call and the dictation. When that
# happened during a real run the text went into the window they had switched
# to, and separately the daemon read TextEdit's title bar ("Untitled") as if
# it were the document.
#
# So this is checked immediately before each utterance rather than assumed.
# Aborting costs one re-run; typing someone's test sentence into whatever
# they are actually doing costs their trust, and it has already happened
# once.
require_textedit_focused() {
  local front docs
  front="$(osascript -e 'tell application "System Events" to name of first application process whose frontmost is true' 2>/dev/null || true)"
  docs="$(osascript -e 'tell application "TextEdit" to count documents' 2>/dev/null || echo 0)"
  if [[ "$front" != "TextEdit" ]]; then
    echo "ABORTING: $front is frontmost, not TextEdit." >&2
    echo "Dictating now would type into it. Nothing was written." >&2
    exit 1
  fi
  if [[ "$docs" -lt 1 ]]; then
    echo "ABORTING: TextEdit has no open document." >&2
    echo "The daemon would read the title bar instead of text." >&2
    exit 1
  fi
}

# Every invocation goes through here so the dry-run switch cannot be
# applied to some utterances and forgotten on others.
run_outloud() {
  if [[ "$DRY_RUN" == "1" ]]; then
    OUTLOUD_NO_INJECT=1 "$BIN" "$@"
  else
    "$BIN" "$@"
  fi
}

pass=0
fail=0

check() {
  local label="$1" got="$2" want="$3"
  # A dry run suppresses delivery, so the document legitimately does not
  # change. Reporting that as FAIL would train the reader to ignore a red
  # line from this script, which is the one thing it cannot afford. It is
  # reported as SKIP, and the run refuses to claim an overall pass.
  if [[ "$DRY_RUN" == "1" ]]; then
    echo "    SKIP $label (dry run: nothing was written)"
    return 0
  fi
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

# --- consent -----------------------------------------------------------------
#
# This script SPEAKS AND TYPES on the machine running it. It synthesizes
# audio through `say`, and the daemon writes the result into whatever window
# is focused. It focuses TextEdit first and re-checks before every utterance,
# but the person at the keyboard needs to know it is about to start, because
# for the next few seconds their typing will fight it.
#
# Announce and wait, rather than starting instantly. This is not paranoia: a
# run that began while the user was mid-sentence in another app put a test
# phrase into their window. Set OUTLOUD_LIVE_YES=1 to skip the prompt when
# running unattended.
if [[ "$DRY_RUN" == "0" && "${OUTLOUD_LIVE_YES:-}" != "1" ]]; then
  cat >&2 <<'BANNER'

  ABOUT TO DICTATE ON THIS MACHINE
  --------------------------------
  This uses the real microphone path and TYPES INTO TEXTEDIT.
  It takes about 15 seconds. Stop typing until it finishes.

  Press Return to start, or Ctrl-C to cancel.

BANNER
  read -r _ || exit 1
fi

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
require_textedit_focused
run_outloud --once --say "change quick to slow" --no-overlay 2>&1 | sed 's/^/    | /' || true
sleep 0.8
edited="$(read_text)"
echo "    after:  $edited"
check "a spoken edit reached the document" "$edited" "$EDITED"
if [[ "$DRY_RUN" == "0" && "$edited" != "$EDITED" ]]; then
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
require_textedit_focused
out="$(run_outloud --once \
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
require_textedit_focused
run_outloud --once --say "scratch that" --no-overlay 2>&1 | sed 's/^/    | /' || true
sleep 0.8
after_empty="$(read_text)"
echo "    after:  $after_empty"
check "document untouched by an unbacked undo" "$after_empty" "$ORIGINAL"

echo
if [[ "$DRY_RUN" == "1" ]]; then
  echo "DRY RUN: the harness ran end to end and wrote nothing."
  echo "This proves the plumbing (setup, focus guards, read-back), NOT that"
  echo "undo works. Re-run without --dry-run for that."
  exit 0
fi
echo "passed: $pass   failed: $fail"
[[ "$fail" -eq 0 ]]
