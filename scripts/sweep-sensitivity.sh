#!/usr/bin/env bash
# Sweep the sensitivity dial against a fixed recording.
#
# The point: show that the dial actually changes whether quiet speech is
# heard, on real audio through the real segmenter, rather than only in a
# unit test against synthetic tones.
set -uo pipefail
cd "$(dirname "$0")/.."

WAV="${1:?usage: sweep-sensitivity.sh <wav>}"

for s in 25 50 60 75 90 100; do
  out=$(cargo run --release -q -p outloud -- \
          --once --wav "$WAV" --asr mock --sensitivity "$s" 2>&1)
  if grep -q 'release->text' <<<"$out"; then
    heard=$(grep -oE '"[^"]*"$' <<<"$out" | tail -1)
  else
    heard="(heard nothing)"
  fi
  printf 'sensitivity %3d  ->  %s\n' "$s" "$heard"
done
