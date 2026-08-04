#!/usr/bin/env bash
# Lint the daemon for a real Linux target, from macOS.
#
# WHY: crates/outloud broke the Linux CI job three times in one day, each
# time for something no check on this machine could see, each time costing a
# ten-minute round trip through GitHub:
#
#   1. resolve_undo             - private, every caller platform-gated
#   2. write_literal_via_tiers  - same, one gate deeper
#   3. an unused `mode` parameter, used only inside the Windows block
#
# The first two have a source-reading check (ci-check-gated-only-callers.py).
# The third does not and could not: "is this parameter used" depends on which
# cfg branches compile, which is a compiler's job. Reading source was the
# wrong tool; the right one is a Linux compiler, which this supplies.
#
# Two obstacles, both worked around here rather than in the reader's head:
#
#   - The Linux target needs a cross linker. cargo-zigbuild uses zig's, which
#     needs no sysroot.
#   - alsa-sys runs pkg-config in its build script and panics without ALSA
#     development headers, which macOS does not have. Clippy never links, so
#     a stub alsa.pc is enough to get past the build script. It is written to
#     a temp dir, not installed anywhere.
#
# Requirements (install once):
#
#   cargo install cargo-zigbuild --locked
#   brew install zig pkgconf
#
# Usage: scripts/ci-check-linux.sh [extra cargo args...]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"

CARGO_ZIGBUILD="${CARGO_ZIGBUILD:-$HOME/.cargo/bin/cargo-zigbuild}"
if [[ ! -x "$CARGO_ZIGBUILD" ]]; then
  echo "cargo-zigbuild not installed. Run:" >&2
  echo "    cargo install cargo-zigbuild --locked" >&2
  exit 1
fi
if ! command -v zig >/dev/null 2>&1; then
  echo "zig not installed. Run: brew install zig" >&2
  exit 1
fi
if ! command -v pkg-config >/dev/null 2>&1; then
  echo "pkg-config not installed. Run: brew install pkgconf" >&2
  exit 1
fi
if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "target $TARGET not installed. Run:" >&2
  echo "    rustup target add $TARGET" >&2
  exit 1
fi

# A stub alsa.pc, enough for alsa-sys's build script to stop panicking.
# Nothing is linked during a clippy run, so the empty lib and include dirs
# never matter. Deliberately in a temp dir: this must not look like an ALSA
# installation to anything else.
STUB="$(mktemp -d)"
trap 'rm -rf "$STUB"' EXIT
mkdir -p "$STUB/pkgconfig" "$STUB/include" "$STUB/lib"
cat > "$STUB/pkgconfig/alsa.pc" <<EOF
prefix=$STUB
libdir=\${prefix}/lib
includedir=\${prefix}/include

Name: alsa
Description: Stub for cross-target LINTING only. Never linked.
Version: 1.2.11
Libs: -L\${libdir} -lasound
Cflags: -I\${includedir}
EOF

export PKG_CONFIG_PATH="$STUB/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS=1

echo "==> clippy for $TARGET (outloud, all targets, -D warnings)"
# The cargo-zigbuild BINARY, not `cargo zigbuild`: the cargo subcommand
# form only accepts `zigbuild`, while the binary itself exposes `clippy`
# (and `build`, `check`, `run`, `test`) with zig wired in as the linker.
"$CARGO_ZIGBUILD" clippy -p outloud --target "$TARGET" --all-targets "$@" -- -D warnings
echo "    outloud clean for $TARGET"

echo "==> linux check OK"
echo
echo "NOTE: a COMPILE and LINT check. It runs nothing, and the ALSA stub"
echo "means audio capture is not exercised. What it catches is the class"
echo "that only ever appears on a platform this machine cannot run."
