#!/usr/bin/env bash
# Build and test the whisper backend against a real model.
#
# Why a dedicated script and job: the whisper integration test skips when no
# model is present, so the default CI run proves nothing about it. A backend
# that only has skipping tests is a backend that can rot green, which is the
# exact failure this repository has hit before with the speech helper.
#
# Not folded into ci-check.sh because it is expensive: whisper-rs compiles
# whisper.cpp from source (needs cmake) and the model is a 142MiB download.
# Run locally the same way CI does:
#
#   scripts/ci-whisper.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODEL_DIR="${OUTLOUD_MODEL_DIR:-$HOME/.outloud/models}"
MODEL="$MODEL_DIR/whisper-base.en"
URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"

# The expected hash is read out of the registry rather than copied here, so a
# re-pin cannot leave this script verifying a stale value. If the grep stops
# matching, that is a failure, not a reason to skip verification.
# `|| true` on the pipeline: under `set -e` a non-matching grep would abort
# the script with no output at all, which is a worse diagnosis than the
# explicit message below. The comment block above the pin is long, so the
# window has to be generous.
EXPECTED="$(grep -A16 '"whisper-base.en"' crates/asr/src/models.rs \
    | grep -oE 'sha256: Some\("[0-9a-f]{64}"\)' \
    | grep -oE '[0-9a-f]{64}' | head -1 || true)"
if [[ -z "$EXPECTED" ]]; then
    echo "ci-whisper: could not read the pinned sha256 out of crates/asr/src/models.rs" >&2
    exit 1
fi

if [[ ! -f "$MODEL" ]]; then
    echo "==> fetching ggml-base.en.bin (142MiB)"
    mkdir -p "$MODEL_DIR"
    # To a temporary name first: a cache restored mid-download would look
    # like a complete model and fail the hash check on a later run instead
    # of this one.
    curl -fsSL -o "$MODEL.partial" "$URL"
    mv "$MODEL.partial" "$MODEL"
fi

if command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$MODEL" | cut -d' ' -f1)"
else
    ACTUAL="$(sha256sum "$MODEL" | cut -d' ' -f1)"
fi
if [[ "$ACTUAL" != "$EXPECTED" ]]; then
    echo "ci-whisper: model hash mismatch" >&2
    echo "  expected $EXPECTED (crates/asr/src/models.rs)" >&2
    echo "  actual   $ACTUAL ($MODEL)" >&2
    # Remove it: a cached bad file would fail every future run identically.
    rm -f "$MODEL"
    exit 1
fi
echo "==> model verified against the registry pin"

# The test resolves the model from the environment first; setting it makes
# the run independent of where the cache landed.
export OUTLOUD_WHISPER_MODEL="$MODEL"

echo "==> cargo test -p asr --features whisper"
cargo test --locked -p asr --features whisper

# The assertion above is a transcript comparison, so a silent skip would
# still exit 0. Prove the test actually ran the model.
echo "==> confirming the transcription test did not skip"
output="$(cargo test --locked -p asr --features whisper --test whisper_transcribes_testdata -- --nocapture 2>&1)"
if grep -q "skipping: no whisper model" <<<"$output"; then
    echo "ci-whisper: the transcription test skipped despite a verified model" >&2
    exit 1
fi
grep -E "whisper finalize:" <<<"$output" || {
    echo "ci-whisper: expected the test to report its finalize time" >&2
    exit 1
}
