#!/usr/bin/env bash
# Run `doctor` through LaunchServices and stream its output back.
#
# Why not just `cargo run --bin doctor`: macOS attributes an Accessibility
# grant to a process's *responsible* process. A binary run straight from a
# shell is judged against the TERMINAL's permission, so the doctor would be
# diagnosing the terminal's environment, not the app's. Launching a bundle
# with `open` makes the app responsible for itself, which is exactly the
# situation the shipping product runs in and exactly the situation the doctor
# must measure. See docs/macos-permissions.md.
#
# LaunchServices detaches the process from the terminal, so the binary mirrors
# its output to HEXA_SPIKE_LOG and this script tails it, same contract as
# scripts/run.sh.
#
# On non-macOS systems there is no LaunchServices and no responsible-process
# trap, so the binary is run directly.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "$(uname)" != "Darwin" ]]; then
    exec cargo run --quiet --manifest-path "$ROOT/Cargo.toml" --bin doctor -- "$@"
fi

APP_NAME="HexavoiceDoctor"
BUNDLE_ID="dev.hexavoice.doctor"
APP_DIR="$ROOT/dist/$APP_NAME.app"
LOG="${TMPDIR:-/tmp}/hexavoice-doctor-$$.log"

echo "==> Building doctor" >&2
cargo build --quiet --release --manifest-path "$ROOT/Cargo.toml" --bin doctor

# Assemble a minimal bundle every run. Cheap, and it guarantees the launched
# binary is the one just built rather than a stale copy.
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp "$ROOT/target/release/doctor" "$APP_DIR/Contents/MacOS/$APP_NAME"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <!-- Diagnostic tool; must not take over the Dock. -->
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
PLIST

# --identifier pins the designated requirement to the bundle id so the TCC
# grant survives rebuilds (as far as an ad-hoc signature allows).
codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_DIR" 2>/dev/null

: > "$LOG"
# HEXA_LAUNCHED_VIA_LS tells the doctor it is genuinely under LaunchServices:
# `open --env` leaks the caller's TERM through, which would otherwise trip the
# shell-launch heuristic even in a correct launch.
open -a "$APP_DIR" --env "HEXA_SPIKE_LOG=$LOG" --env "HEXA_LAUNCHED_VIA_LS=1" --args "$@"

# Wait for the sentinel the binary writes last; `open` gives no exit status.
for _ in $(seq 1 600); do
    if grep -q '^__EXIT__' "$LOG" 2>/dev/null; then
        break
    fi
    sleep 0.1
done

exit_code="$(grep '^__EXIT__' "$LOG" 2>/dev/null | tail -1 | sed 's/^__EXIT__//')"
grep -v '^__EXIT__' "$LOG" || true
rm -f "$LOG"

exit "${exit_code:-0}"
