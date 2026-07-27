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
# vbroadcast/vperm/vpand etc all start with 'v'; gather the common families.
# FMA (vfmadd) is Haswell+ too. pdep/pext are BMI2 (the exact instructions
# that crashed Handy's ONNX static init).
HITS="$(objdump -d "$BIN" 2>/dev/null \
    | grep -Ec '\b(vpbroadcast|vperm|vpand|vpadd|vpmul|vfmadd|vgather|pdep|pext|vmovdq[au]|vzeroupper)\b' || true)"

if [ "$HITS" -gt 0 ]; then
    echo "FAIL: $HITS post-baseline (AVX/AVX2/BMI2) instructions found." >&2
    echo "A dependency raised the ISA baseline. Find it with:" >&2
    echo "  objdump -d '$BIN' | grep -B20 vpbroadcast | less" >&2
    echo "and either build it with runtime dispatch or drop it." >&2
    echo "Context: docs/build-and-release.md#the-pre-haswell-avx2-crash-class" >&2
    exit 1
fi

echo "==> Clean: x86-64 baseline (SSE2) only"
