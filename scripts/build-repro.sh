#!/usr/bin/env bash
# Reproducible release build: build the workspace twice under normalized
# conditions and fail unless the binaries are byte-identical.
#
# The three sources of nondeterminism in a Rust binary and how each is closed:
#   1. Toolchain drift      -> rust-toolchain.toml pins the exact compiler.
#   2. Embedded build paths -> --remap-path-prefix maps the checkout dir and
#      CARGO_HOME to stable names, so two checkouts at different paths agree.
#   3. Timestamps           -> SOURCE_DATE_EPOCH pinned to the commit date
#      (the reproducible-builds.org convention), so packaging metadata that
#      honours it (tar, dpkg) is stable too. rustc itself does not embed wall
#      clock time, but archive/packaging steps do.
#
# CARGO_INCREMENTAL=0: incremental compilation caches are keyed on mtimes and
# can leak differing codegen unit partitioning between runs.
#
# Usage: scripts/build-repro.sh [target-triple]
#   With no argument, builds the host target. The verify step (double build)
#   only runs with REPRO_VERIFY=1 because it doubles build time; CI's repro
#   job sets it, release jobs just use the normalized flags.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${1:-}"
export SOURCE_DATE_EPOCH="$(git log -1 --pretty=%ct)"
export CARGO_INCREMENTAL=0
# System dependencies before the first build. Same reason as ci-check.sh:
# alsa-sys runs pkg-config in a build script, so a Linux box without
# libasound2-dev cannot compile the workspace at all, reproducibly or
# otherwise. No-op where not needed.
scripts/ci-install-linux-deps.sh
# TZ/LC pinning: paranoia against proc-macros that format dates.
export TZ=UTC LC_ALL=C

# Remap both the checkout and the cargo registry cache. Without the registry
# remap, dependency debug info still embeds /home/runner/.cargo/... paths.
REMAP_FLAGS="--remap-path-prefix=$ROOT=/build --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
export RUSTFLAGS="${RUSTFLAGS:-} $REMAP_FLAGS"

TARGET_ARGS=()
BIN_DIR="target/release"
if [ -n "$TARGET" ]; then
    TARGET_ARGS=(--target "$TARGET")
    BIN_DIR="target/$TARGET/release"
fi

build_once() {
    cargo build --release --locked --workspace "${TARGET_ARGS[@]}"
}

echo "==> Reproducible build (SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH)"
build_once
HASH1="$(shasum -a 256 "$BIN_DIR/spike-cli"* | awk '{print $1}' | head -1)"
echo "    first build:  $HASH1"

if [ "${REPRO_VERIFY:-0}" = "1" ]; then
    echo "==> Rebuilding from clean to verify determinism"
    cargo clean --release "${TARGET_ARGS[@]}" 2>/dev/null || cargo clean
    build_once
    HASH2="$(shasum -a 256 "$BIN_DIR/spike-cli"* | awk '{print $1}' | head -1)"
    echo "    second build: $HASH2"
    if [ "$HASH1" != "$HASH2" ]; then
        echo "FAIL: build is not reproducible. Diff the two binaries with" >&2
        echo "      'diffoscope' to find the nondeterministic section." >&2
        exit 1
    fi
    echo "==> Reproducible: both builds hash $HASH1"
fi
