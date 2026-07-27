#!/usr/bin/env bash
# Package the spike CLI as a macOS .app bundle.
#
# A bare Mach-O binary is a poor citizen of the TCC permission system: an
# ad-hoc, linker-signed executable has no stable identity, so macOS may treat
# each rebuild as a different program and re-prompt for Accessibility access.
# Wrapping the binary in a bundle with a fixed CFBundleIdentifier and a stable
# code signature gives the permission something durable to attach to, which is
# what the shipping product will need regardless.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="AquaSpike"
BUNDLE_ID="dev.aquaoss.spike"
APP_DIR="$ROOT/dist/$APP_NAME.app"

echo "==> Building release binary"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp "$ROOT/target/release/spike-cli" "$APP_DIR/Contents/MacOS/$APP_NAME"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>Aqua OSS Spike</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <!-- The harness is a command-line tool; it must not take over the Dock. -->
    <key>LSUIElement</key>
    <true/>
    <!-- Required so the frontmost-application lookup can drive System Events. -->
    <key>NSAppleEventsUsageDescription</key>
    <string>Aqua OSS Spike identifies the frontmost application to choose formatting rules.</string>
</dict>
</plist>
PLIST

echo "==> Signing with a stable ad-hoc identity"
# `--identifier` pins the designated requirement to the bundle id, so the TCC
# grant survives rebuilds. `--force` replaces the linker-signed signature.
codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_DIR"
codesign --verify --verbose=2 "$APP_DIR" 2>&1 | sed 's/^/    /'

echo
echo "Built: $APP_DIR"
echo
echo "Grant Accessibility permission once:"
echo "  1. Open System Settings > Privacy & Security > Accessibility"
echo "  2. Click +, then select $APP_DIR"
echo
echo "Then run the harness through the bundled binary:"
echo "  $APP_DIR/Contents/MacOS/$APP_NAME probe"
