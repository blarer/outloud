#!/usr/bin/env bash
# Type-check the non-macOS code paths locally, so cfg rot is caught here
# rather than on a CI runner ten minutes after a push.
#
# WHY: every `cfg(not(target_os = "macos"))` stub is invisible to a normal
# `cargo check` on a Mac. It rots silently. That has now broken CI twice,
# both times the same way: a stub exposed its constructor but not the
# methods callers go on to use, so the type existed and the calls did not.
#
# What this can and cannot do, stated plainly rather than implied:
#
# A full `cargo check --target x86_64-unknown-linux-gnu` is NOT possible on
# this machine. It fails inside `ring`'s build script, which needs a C
# compiler for the target, and no cross toolchain is installed. That is a
# build-script failure in a dependency, not a type error in our code, so it
# stops the compiler before it reaches anything worth checking.
#
# What IS possible is cross-checking the crates whose dependencies are pure
# Rust. That covers every crate where the macOS/non-macOS split actually
# lives, which is where the bug class comes from. `outloud` itself is
# excluded only because it pulls `ureq` -> `rustls` -> `ring`.
#
# So this is a real check with a named gap, not a proxy that pretends.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${1:-x86_64-unknown-linux-gnu}"

if ! rustup target list --installed | grep -q "^${TARGET}$"; then
    echo "SKIP: ${TARGET} not installed (rustup target add ${TARGET})"
    exit 0
fi

# Crates with a platform split and no C dependencies. Adding a crate here is
# free; leaving one out means its stubs are only checked by CI.
CRATES=(
    -p text-target
    -p overlay
    -p stream
    -p edit-intent
    -p config
    -p diag
)

# clippy, not check: CI runs clippy with -D warnings, so a dead constant
# left behind a platform gate is a BUILD FAILURE there while being merely
# unused here. `cargo check` is blind to exactly that.
echo "==> cross-checking platform stubs for ${TARGET}"
cargo clippy "${CRATES[@]}" --no-default-features --target "$TARGET" --quiet -- -D warnings
echo "    stubs compile clean for ${TARGET}"

# The headless configuration on the host: the most cfg branches reachable
# without a cross toolchain, and the shape a Linux server actually runs.
echo "==> checking headless build on the host"
cargo clippy --workspace --all-targets --no-default-features --quiet -- -D warnings
echo "    headless OK"

# The gap above is real, so close the specific hole it leaves. A
# macOS-gated *definition* called from ungated code compiles perfectly on
# this machine and fails on every other target. That just happened:
# `inject::app_identity` was `#[cfg(target_os = "macos")]` while its call
# site in pipeline.rs was not, and CI's Linux, Windows and msrv jobs all
# failed with "cannot find function".
#
# Cross-checking outloud would catch it, and cannot run here. Reading the
# source can: find items gated to macOS, then look for callers of the same
# name outside any macOS-gated region.
echo "==> macOS-gated items called from ungated code"
python3 "$ROOT/scripts/ci-check-gated-calls.py"

echo "==> cfg check OK"
echo
echo "NOTE: crates/outloud is not cross-checked here, because ureq -> rustls"
echo "-> ring needs a C compiler for the target and none is installed. Its"
echo "non-macOS branches are covered by CI's Linux jobs, not by this script."
