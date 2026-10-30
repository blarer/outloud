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

# Keep ggml off AMX, so a build on one runner cannot crash on another.
#
# WHY: without this, the Linux job fails intermittently with
#
#   AMX is not ready to be used!
#   process didn't exit successfully: ... (signal: 4, SIGILL: illegal instruction)
#
# ggml's CMakeLists sets `GGML_NATIVE` ON by default (unless
# cross-compiling), which tunes for whatever CPU the build landed on.
# GitHub's runner fleet is not homogeneous: a build assigned to a Sapphire
# Rapids host compiles in AMX instructions, and the resulting binary may then
# execute on a CPU without them. The failure is a coin flip on runner
# allocation, which is why it has been passing and failing across commits
# that touch nothing related to whisper.
#
# Same class as the pre-Haswell AVX2 crash `scripts/ci-verify-baseline.sh`
# exists to catch, with one difference: that guard inspects the SHIPPED
# x86_64 release artifact, and this is a CI-only test build, so nothing was
# watching it.
#
# HOW, and what does NOT work: `CMAKE_ARGS` is a dead end. Neither
# `whisper-rs-sys`'s build script nor the `cmake` crate reads it (the crate
# only honours `CMAKE`, `CMAKE_GENERATOR`, `CMAKE_TOOLCHAIN_FILE`,
# `CMAKE_PREFIX_PATH`, and the `cc`-derived flag variables), so
# `-DGGML_NATIVE=OFF` passed that way is silently discarded. That was my
# first attempt here and it would have looked like a fix while changing
# nothing.
#
# `CFLAGS`/`CXXFLAGS` DO reach the compiler: cmake-rs builds
# `CMAKE_C_FLAGS`/`CMAKE_CXX_FLAGS` from `cc`, which reads them.
#
# x86_64 ONLY, and the MECHANISM DIFFERS BY TOOLCHAIN:
#
# - GCC/clang take `-mno-amx-*`, which disables those feature families while
#   leaving AVX2 and friends on, so the test keeps representative performance
#   and only the instruction set that faults is excluded.
# - MSVC's `cl.exe` has no equivalent. It silently ignores `-mno-*`, so the
#   same CFLAGS that fix Linux do NOTHING on Windows. That is not theoretical:
#   `whisper (windows-2025)` exited 132 (SIGILL) with these flags set and
#   echoed in the log. On MSVC the lever is ggml's own cmake option, and
#   `GGML_NATIVE=OFF` is what stops it tuning for the build machine.
# - Nothing to do on arm64: `-mno-amx-*` are x86 flags that make clang reject
#   the whole compilation, and AMX does not exist outside x86 anyway.
#
# GGML_NATIVE is passed through `CMAKE_TOOLCHAIN_FILE`, which cmake-rs does
# read (`getenv_target_os("CMAKE_TOOLCHAIN_FILE")`), because it does not read
# `CMAKE_ARGS` and `whisper-rs-sys` exposes no way to add a define.
case "$(uname -m)" in
    x86_64 | amd64)
        if [[ "${OS:-}" == Windows_NT ]]; then
            # A toolchain file is the only supported channel for a -D here.
            TOOLCHAIN="$(mktemp -t ggml-baseline-XXXXXX.cmake)"
            cat > "$TOOLCHAIN" <<'CMAKE'
# Generated by scripts/ci-whisper.sh. Stops ggml tuning for the build
# machine's CPU, which on a heterogeneous runner fleet means a binary that
# SIGILLs on whatever host runs the test. MSVC has no -mno-amx-* equivalent,
# so this option is the only lever.
set(GGML_NATIVE OFF CACHE BOOL "" FORCE)
CMAKE
            export CMAKE_TOOLCHAIN_FILE="$TOOLCHAIN"
            echo "==> windows x86_64: GGML_NATIVE=OFF via $TOOLCHAIN"
        else
            AMX_OFF="-mno-amx-tile -mno-amx-int8 -mno-amx-bf16"
            export CFLAGS="${CFLAGS:-} $AMX_OFF"
            export CXXFLAGS="${CXXFLAGS:-} $AMX_OFF"
            echo "==> x86_64: AMX disabled for ggml ($AMX_OFF)"
        fi
        ;;
    *)
        echo "==> $(uname -m): no AMX on this architecture, nothing to disable"
        ;;
esac

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
