#!/usr/bin/env bash
# Sweep the sensitivity dial against a fixed recording.
#
# The point: show that the dial actually changes whether quiet speech is
# heard, on real audio through the real segmenter, rather than only in a
# unit test against synthetic tones.
#
# Also used to locate the ceiling: the highest setting that still stays
# silent on a recording of room noise is the highest the menu may offer.
set -uo pipefail
cd "$(dirname "$0")/.."

# Never type into the user's focused window: this replays recordings, and
# without the guard every run injects its test sentence into whatever app
# they happen to be using.
export OUTLOUD_NO_INJECT=1

WAV="${1:?usage: sweep-sensitivity.sh <wav> [asr] [steps...]}"
ASR="${2:-mock}"
shift 2 2>/dev/null || shift 1 2>/dev/null || true
STEPS=("$@")
if [ "${#STEPS[@]}" -eq 0 ]; then
  STEPS=(25 50 60 75 90 100)
fi

for s in "${STEPS[@]}"; do
  out=$(cargo run --release -q -p outloud -- \
          --once --wav "$WAV" --asr "$ASR" --sensitivity "$s" 2>&1)
  if grep -q 'release->text' <<<"$out"; then
    heard=$(grep -oE '"[^"]*"$' <<<"$out" | tail -1)
  else
    heard="(heard nothing)"
  fi
  printf 'sensitivity %3d  ->  %s\n' "$s" "$heard"
done
