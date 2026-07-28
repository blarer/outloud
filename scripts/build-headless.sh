#!/usr/bin/env bash
# Build the headless daemon binary: no GUI, no display server, works over SSH.
#
# CONTRACT with the crates (which this script does not own): the workspace
# exposes a cargo feature named `headless` on `spike-cli` that compiles out
# every GUI/display-touching code path. On macOS the Accessibility calls stay
# (they need a user session but not a display we create); on Linux the point
# is that the binary must not link X11/Wayland client libraries at all, so a
# server with no display stack can run it.
#
# Until the crates grow that feature this script still produces a usable
# artifact: the plain CLI already runs with zero display server on Linux
# because nothing in the current dependency graph links a display library
# (verified below, mechanically, not by assertion). The feature flag becomes
# meaningful the moment a GUI dependency lands, and the link check below is
# the tripwire that forces it to be feature-gated.
#
# Failure modes:
#   - GUI dep sneaks into the default feature set -> otool/ldd check FAILS the
#     build here rather than failing at runtime on a headless server.
#   - feature exists but bitrots               -> CI builds it on every PR.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${1:-}"          # optional explicit target triple
PKG="spike-cli"
OUT_DIR="dist/headless"

CARGO_ARGS=(build --release --locked --package "$PKG")
if [ -n "$TARGET" ]; then
    CARGO_ARGS+=(--target "$TARGET")
fi

# Probe for the feature rather than hardcoding, so this script works both
# before and after the crates add it. `cargo metadata` is the source of truth.
if cargo metadata --format-version 1 --no-deps \
    | python3 -c '
import json, sys
meta = json.load(sys.stdin)
for pkg in meta["packages"]:
    if pkg["name"] == "spike-cli" and "headless" in pkg.get("features", {}):
        sys.exit(0)
sys.exit(1)
'; then
    echo "==> headless feature found; building with --no-default-features --features headless"
    CARGO_ARGS+=(--no-default-features --features headless)
else
    echo "==> NOTE: no 'headless' cargo feature on $PKG yet; building default features"
    echo "==>       (safe today: link check below proves no display libs are pulled in)"
fi

cargo "${CARGO_ARGS[@]}"

if [ -n "$TARGET" ]; then
    BIN="target/$TARGET/release/$PKG"
else
    BIN="target/release/$PKG"
fi

echo "==> Verifying no display-server libraries are linked"
case "$(uname -s)" in
    Linux)
        # ldd fails on fully static (musl) binaries; that is itself a pass.
        if LINKED="$(ldd "$BIN" 2>&1)"; then
            if echo "$LINKED" | grep -Ei 'libX11|libxcb|libwayland|libgtk|libgdk'; then
                echo "FAIL: headless binary links a display library (above). Gate it behind the gui feature." >&2
                exit 1
            fi
        else
            echo "    static binary (no dynamic deps) - pass"
        fi
        ;;
    Darwin)
        # AppKit would mean we accidentally grew a GUI. CoreGraphics and the
        # ApplicationServices umbrella are expected and allowed: the AX API
        # itself lives under ApplicationServices and its types (CGPoint,
        # AXUIElement geometry) come from CoreGraphics, without us owning any
        # window or display. Verified empirically: the current binary links
        # CoreGraphics purely via accessibility-sys.
        if otool -L "$BIN" | grep -E 'AppKit'; then
            echo "FAIL: headless binary links AppKit." >&2
            exit 1
        fi
        echo "    no AppKit - pass"
        ;;
esac

mkdir -p "$OUT_DIR"
cp "$BIN" "$OUT_DIR/hexavoice-spiked${TARGET:+-$TARGET}"
echo "Built: $OUT_DIR/hexavoice-spiked${TARGET:+-$TARGET}"
