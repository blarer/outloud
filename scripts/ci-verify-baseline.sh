#!/usr/bin/env bash
# CI: prove no post-baseline x86 instructions (AVX/AVX2/BMI2) leaked into a
# release binary we ship for x86_64.
#
# WHY: the "Handy lesson". Handy's Windows build once linked pyke's prebuilt
# ONNX Runtime, compiled with a global /arch:AVX2 baseline. AVX2 code ran in
# a STATIC INITIALIZER, i.e. before main(), so the process died with an
# illegal-instruction fault on every pre-Haswell CPU (Sandy/Ivy Bridge, older
# AMD) before any runtime CPU-detection code could possibly execute. The fix
# there was linking a baseline runtime with proper runtime dispatch; the
# lesson here is that "we don't set target-cpu" is a claim that must be
# VERIFIED on the artifact, because a prebuilt native dependency can raise
# the baseline without any change to our own flags.
#
# Method: objdump the shipped binary and grep the disassembly for VEX-encoded
# instructions. Runtime-dispatch libraries keep those behind cpuid checks in
# separate functions; a baseline violation shows them in ordinary code paths.
# We take the conservative approach: our OWN code and current deps should
# contain ZERO AVX instructions (nothing here does SIMD), so any hit is a
# regression worth a human look. When a legitimately-dispatching SIMD dep is
# added, tighten this to scan only .init/.ctors sections instead of deleting
# the check.
#
# Usage: scripts/ci-verify-baseline.sh <path-to-x86_64-binary>

set -euo pipefail

BIN="${1:?usage: ci-verify-baseline.sh <x86_64 binary>}"

if ! command -v objdump >/dev/null 2>&1; then
    echo "objdump not found; install binutils" >&2
    exit 1
fi

echo "==> Scanning $BIN for AVX/AVX2/BMI2 instructions"

# WHICH CODE, and why this is not a section filter.
#
# The original check scanned the whole binary and demanded zero hits, on the
# stated assumption that "our OWN code and current deps should contain ZERO
# AVX instructions (nothing here does SIMD)". That expired when the daemon
# started shipping whisper.cpp: ggml contains AVX2 kernels by design and
# picks them at RUNTIME via `ggml_cpu_has_avx2()` (ggml-cpu.c:3441, called
# from ggml-cpu.cpp:531 and repack.cpp). That is the "runtime dispatch" this
# script has always said is acceptable, and no cmake flag removes it:
# `GGML_NATIVE=OFF` was set, whisper-rs-sys verifiably recompiled, and the
# whole-binary count moved 12703 -> 12714.
#
# What actually matters is the Handy crash class: VEX-encoded work reached
# BEFORE main(), where no cpuid check can guard it, so a pre-Haswell machine
# dies at process start.
#
# MY FIRST ATTEMPT AT THIS WAS WRONG, and a sabotage test caught it. I
# filtered the disassembly to `.init_array`/`.init`/`.text.startup`. But
# `.init_array` contains POINTERS, not code:
#
#     Contents of section .init_array:
#      10025b0 d0140001 00000000
#
# The constructor's BODY lives in `.text` like any other function, so the
# filter matched nothing and the check passed a binary with
# `vpbroadcastd %ymm0` in a constructor. A guard that passes the thing it
# guards against is worse than no guard.
#
# So: read the pointers out of `.init_array`, resolve each to the function
# that starts at that address, and disassemble only those functions plus
# `.init` itself. That is the set of code that runs before main.
scan_preinit_code() {
    local bin="$1"

    # Addresses in .init_array, little-endian 8-byte words.
    local addrs
    addrs="$(objdump -s -j .init_array "$bin" 2>/dev/null \
        | awk 'NR > 3 { for (i = 2; i <= 5 && i <= NF; i++) printf "%s", $i; print "" }' \
        | tr -d ' ' \
        | awk '{
            # Each line is up to 16 hex bytes; split into 8-byte words and
            # byte-swap each (objdump -s prints in file order).
            for (w = 0; w + 16 <= length($0); w += 16) {
                word = substr($0, w + 1, 16)
                out = ""
                for (b = 7; b >= 0; b--) out = out substr(word, b * 2 + 1, 2)
                print out
            }
        }' \
        | sed 's/^0*//' | grep -v '^$' || true)"

    # .init always runs pre-main and is real code, so include it wholesale.
    objdump -d -j .init "$bin" 2>/dev/null || true

    # Each constructor, by symbol at that address.
    local a
    for a in $addrs; do
        objdump -d --start-address="0x$a" --stop-address="$((0x$a + 4096))" \
            "$bin" 2>/dev/null || true
    done
}

HITS="$(scan_preinit_code "$BIN" \
    | grep -Ec '\b(vpbroadcast|vperm|vpand|vpadd|vpmul|vfmadd|vgather|pdep|pext|vmovdq[au]|vzeroupper)\b' || true)"

if [ "$HITS" -gt 0 ]; then
    echo "FAIL: $HITS post-baseline (AVX/AVX2/BMI2) instructions reachable" >&2
    echo "before main() in $BIN." >&2
    echo "" >&2
    echo "This is the pre-Haswell crash class: code that runs during static" >&2
    echo "initialization cannot be behind a cpuid check, so the process dies" >&2
    echo "at startup on any CPU without the feature. Inspect with:" >&2
    echo "  objdump -s -j .init_array '$BIN'   # constructor pointers" >&2
    echo "  objdump -d '$BIN'                  # find those addresses" >&2
    echo "Fix by moving the work out of static init, or by building that" >&2
    echo "dependency for the baseline, or by dropping it." >&2
    echo "Context: docs/build-and-release.md#the-pre-haswell-avx2-crash-class" >&2
    exit 1
fi

echo "==> Clean: nothing post-baseline runs before main()"
