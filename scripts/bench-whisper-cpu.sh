#!/usr/bin/env bash
# Measure whisper's CPU finalize latency AND the transcript text, together.
#
# WHY BOTH, and why this script exists next to bench-latency.sh:
#
# bench-latency.sh benchmarks the OS-integration path (AX read/write) with
# criterion. This measures the recognizer, which criterion is a bad fit for:
# a run costs hundreds of milliseconds, needs a model on disk, and the thing
# most likely to go wrong is not the time but the WORDS.
#
# That is the whole design. An earlier attempt at tuning `audio_ctx` looked
# like a 3.5x win on a single utterance and was in fact two errors stacked:
# the "baseline" was a cold start (4x slower than steady state), and the
# faster transcript had silently dropped its first word. "The quick brown
# fox" became "quick brown fox", which reads as plausible output. A
# latency-only harness would have shipped it.
#
# So every line prints the milliseconds and the text, and any text that
# differs from the expected transcript is flagged. A knob that halves
# latency and loses a word is a regression, and this script is built so that
# is impossible to miss. Findings in
# docs/investigations/whisper-cpu-latency.md.
#
# Requires: a release build with --features whisper, which needs cmake:
#
#   nix shell nixpkgs#cmake --command \
#     cargo build --release -p outloud --features whisper
#
# and a model at ~/.outloud/models/whisper-base.en (the daemon fetches this
# itself on first use).
#
# Usage:
#   scripts/bench-whisper-cpu.sh                 # generated corpus, 3 reps
#   REPS=5 scripts/bench-whisper-cpu.sh          # more reps, less noise
#   CORPUS=my.txt scripts/bench-whisper-cpu.sh   # your own sentences
set -uo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/outloud}"
REPS="${REPS:-3}"
WORK="${WORK:-${TMPDIR:-/tmp}/outloud-whisper-bench}"
CORPUS="${CORPUS:-}"

if [ ! -x "$BIN" ]; then
    echo "no binary at $BIN" >&2
    echo "build it with: nix shell nixpkgs#cmake --command \\" >&2
    echo "  cargo build --release -p outloud --features whisper" >&2
    exit 1
fi

mkdir -p "$WORK"

# Default corpus spans 0.5s to 7s on purpose. The central finding is that
# latency is FLAT across that range (whisper pads everything to a 30s
# window), and a corpus of similar-length utterances cannot show that.
# Short ones are also where `audio_ctx` degenerated worst, so they are the
# cases most worth keeping.
if [ -z "$CORPUS" ]; then
    CORPUS="$WORK/corpus.txt"
    cat > "$CORPUS" <<'SENTENCES'
Yes.
A short one.
Change quick to slow.
The quick brown fox jumps over the lazy dog.
Please send the report to Sarah by Friday afternoon.
The meeting is at three o'clock in the main conference room.
I think we should refactor the authentication middleware before the release.
SENTENCES
fi

# Synthesize with `say` rather than committing WAVs: the repo should not
# carry megabytes of generated audio, and `say` is on every mac. The voice
# is deterministic enough for relative comparisons, which is all this is
# for. It is NOT a WER harness: R-07 in the backlog owns that, with real
# recorded speech.
i=0
while IFS= read -r line; do
    i=$((i + 1))
    wav="$WORK/u$i.wav"
    if [ ! -f "$wav" ]; then
        say -o "$WORK/u$i.aiff" "$line" 2>/dev/null || {
            echo "'say' failed; this script needs macOS or a pre-made corpus" >&2
            exit 1
        }
        afconvert -f WAVE -d LEI16@16000 -c 1 "$WORK/u$i.aiff" "$wav" 2>/dev/null
    fi
done < "$CORPUS"

echo "==> $i utterances, best of $REPS, model from ~/.outloud/models"
echo

i=0
total=0
mismatches=0
while IFS= read -r expected; do
    i=$((i + 1))
    wav="$WORK/u$i.wav"
    [ -f "$wav" ] || continue

    best=999999
    got=""
    for _ in $(seq 1 "$REPS"); do
        # OUTLOUD_NO_INJECT=1 is not optional: without it every single
        # measurement types its transcript into whatever window has focus.
        # That happened during this investigation and it is why the flag
        # exists.
        out=$(OUTLOUD_NO_INJECT=1 "$BIN" --once --asr whisper \
            --wav "$wav" --no-overlay 2>&1)
        ms=$(printf '%s' "$out" | grep -oE 'finalize [0-9]+ms' | head -1 | grep -oE '[0-9]+')
        txt=$(printf '%s' "$out" | grep -oE '\| "[^"]*"' | head -1 | sed 's/^| "//; s/"$//')
        if [ -n "$ms" ] && [ "$ms" -lt "$best" ]; then best=$ms; fi
        [ -n "$txt" ] && got="$txt"
    done

    # Case- and punctuation-insensitive: the question is whether WORDS
    # survived a tuning change, not whether whisper capitalized the same
    # way. Over-strict comparison would cry wolf and get ignored, which is
    # worse than no check.
    norm_e=$(printf '%s' "$expected" | tr 'A-Z' 'a-z' | tr -d ".,?!'" | tr -s ' ')
    norm_g=$(printf '%s' "$got" | tr 'A-Z' 'a-z' | tr -d ".,?!'" | tr -s ' ')
    flag="ok  "
    if [ "$norm_e" != "$norm_g" ]; then
        flag="DIFF"
        mismatches=$((mismatches + 1))
    fi
    total=$((total + best))
    printf '%s u%-2d %6sms  "%s"\n' "$flag" "$i" "$best" "$got"
    [ "$flag" = "DIFF" ] && printf '              expected  "%s"\n' "$expected"
done < "$CORPUS"

echo
printf '%d utterances, %d text mismatches, %dms total\n' "$i" "$mismatches" "$total"
echo
# Stated rather than left to be rediscovered: four identical runs of this
# corpus on one machine totalled 2254, 2097, 2014 and 2008ms under a load
# average of 9.2. Anything smaller than that spread is not a result.
echo "Note: ~10% run-to-run spread is normal under load. Discard the first"
echo "run of a fresh build; cold start is 2-4x steady state."
[ "$mismatches" -gt 0 ] && echo "Note: a few DIFFs are expected even at baseline (number formatting," && echo "elisions). What matters is whether a change ADDS them."
exit 0
