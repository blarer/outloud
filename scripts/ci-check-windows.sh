#!/usr/bin/env bash
# Lint the workspace for a real Windows target, from macOS.
#
# WHY: crates/outloud's Windows code was never compiled on this machine.
# `cargo check --target x86_64-pc-windows-msvc` fails at ring's build script
# (it needs a C toolchain for the target), so ci-check-cfg.sh skips the crate
# and says so. Everything Windows-only therefore reached CI unlinted, and the
# feedback loop was a ten-minute round trip through GitHub.
#
# cargo-xwin plus LLVM closes that. The first run of this script found four
# real defects that macOS could not see:
#
#   1. `std::mem::forget(handle)` on a Copy type in the single-instance
#      guard: a no-op that stated an intent the code was not carrying out.
#   2. Two needless-return clippy failures in cfg(windows) blocks, which are
#      errors under CI's -D warnings.
#   3. crates/audio/examples/mic_level.rs called libc unconditionally, so it
#      could not build for Windows at all.
#
# Requirements (install once):
#
#   cargo install cargo-xwin --locked
#   brew install llvm
#
# The Windows SDK headers are downloaded and cached by cargo-xwin on first
# run. Nothing here touches a Windows machine.
#
# Usage: scripts/ci-check-windows.sh [extra cargo args...]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${TARGET:-x86_64-pc-windows-msvc}"

# `cargo xwin` is a cargo subcommand, so the binary may live anywhere on the
# cargo bin path; ask cargo rather than looking for a name on PATH.
if ! cargo xwin --version >/dev/null 2>&1; then
  echo "cargo-xwin not installed. Run:" >&2
  echo "    cargo install cargo-xwin --locked" >&2
  exit 1
fi

# cc-rs needs llvm-lib to build ring's assembly for the MSVC target. The
# system clang at /usr/bin does not ship it.
LLVM_BIN="${LLVM_BIN:-/opt/homebrew/opt/llvm/bin}"
if [[ ! -x "$LLVM_BIN/llvm-lib" ]]; then
  echo "llvm-lib not found at $LLVM_BIN. Run:" >&2
  echo "    brew install llvm" >&2
  echo "(or set LLVM_BIN to a toolchain that has llvm-lib)" >&2
  exit 1
fi
export PATH="$LLVM_BIN:$PATH"

echo "==> clippy for $TARGET (workspace, all targets, -D warnings)"
cargo xwin clippy --workspace --target "$TARGET" --all-targets "$@" -- -D warnings
echo "    workspace clean for $TARGET"

# The daemon's Windows code only exists with the display feature: the UIA
# target, the tray icon, the overlay, and the undo read-back all live behind
# it. Without this the most Windows-specific code in the tree is skipped.
echo "==> clippy for $TARGET (outloud, --features display)"
cargo xwin clippy -p outloud --target "$TARGET" --features display --all-targets "$@" -- -D warnings
echo "    outloud+display clean for $TARGET"

echo "==> windows check OK"
echo
echo "NOTE: this is a COMPILE and LINT check. It does not run anything."
echo "Behaviour on Windows still needs a Windows machine; what this catches"
echo "is the large class of defects that never compile there at all."
