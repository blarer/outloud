#!/usr/bin/env bash
# Package the hexad daemon as a macOS .app bundle.
#
# WHY this exists alongside scripts/bundle-macos.sh: that script bundles
# spike-cli, the accessibility development harness. The thing a user actually
# runs is hexad, and it needs the same two things spike-cli needs:
#
#   1. A stable bundle identity, because TCC attaches the Accessibility grant
#      to a CFBundleIdentifier + code signature, not to a path.
#   2. To be its own responsible process. A binary started from a shell
#      inherits the terminal as its responsible process, so macOS checks the
#      *terminal's* permissions and ignores the binary's own grant entirely.
#      See docs/macos-permissions.md.
#
# Running ./target/release/hexad directly still works, but then the grants you
# must approve are the terminal's, which is a confusing thing to ask of a user
# and silently changes meaning when they switch terminals.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Hexavoice"
BUNDLE_ID="dev.hexavoice.hexad"
APP_DIR="$ROOT/dist/$APP_NAME.app"

echo "==> Building hexad (release)"
cargo build --release -p hexad --manifest-path "$ROOT/Cargo.toml"

# The Apple recognizer is a Swift child process, not a linked library, so it
# has to be built and shipped separately. It is gitignored (a compiled
# artifact), so a fresh clone has none and the daemon would come up with no
# recognizer at all. swiftc is present on any machine with Xcode or the
# Command Line Tools; without it we warn rather than fail, because --asr mock
# and the WAV paths still work.
HELPER_SRC="$ROOT/crates/asr/helper/transcriber.swift"
HELPER_BIN="$ROOT/crates/asr/helper/aqua-speech-helper"
if command -v swiftc >/dev/null 2>&1; then
    if [[ ! -x "$HELPER_BIN" || "$HELPER_SRC" -nt "$HELPER_BIN" ]]; then
        echo "==> Building the speech helper"
        (cd "$(dirname "$HELPER_SRC")" && swiftc -O transcriber.swift -o aqua-speech-helper)
    fi
else
    echo "warning: swiftc not found; skipping the speech helper." >&2
    echo "         hexad will start but cannot transcribe (use --asr mock)." >&2
fi

echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp "$ROOT/target/release/hexad" "$APP_DIR/Contents/MacOS/$APP_NAME"
# NOT renamed with the product: crates/asr's find_helper() looks for this
# exact filename, and that crate is owned elsewhere. Renaming here alone
# would produce a bundle that builds and silently cannot transcribe, which
# is worse than a stale name. Tracked as a follow-up for the asr owner.
#
# Beside the executable, which is the first place find_helper() looks. Without
# this the bundle only works on the machine it was built on, via the in-repo
# fallback path baked in at compile time.
if [[ -x "$HELPER_BIN" ]]; then
    cp "$HELPER_BIN" "$APP_DIR/Contents/MacOS/aqua-speech-helper"
fi

# NSMicrophoneUsageDescription is mandatory, not cosmetic: a process that
# opens an input device without it is killed by the system rather than denied.
cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>Hexavoice</string>
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
    <!-- A background dictation daemon; it must not take over the Dock. -->
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Hexavoice transcribes your speech on this device to type it for you.</string>
    <key>NSSpeechRecognitionUsageDescription</key>
    <string>Hexavoice uses the on-device speech recognizer; no audio leaves this Mac.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>Hexavoice identifies the frontmost application to choose formatting rules.</string>
</dict>
</plist>
PLIST

echo "==> Signing with a stable ad-hoc identity"
# --identifier pins the designated requirement to the bundle id so the TCC
# grant survives rebuilds; --force replaces the linker-signed signature.
codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_DIR"
codesign --verify --verbose=2 "$APP_DIR" 2>&1 | sed 's/^/    /'

cat <<EOF

Built: $APP_DIR

Grant permissions once (macOS gives no programmatic way to do this):
  System Settings > Privacy & Security > Accessibility  -> add $APP_DIR, toggle on
  System Settings > Privacy & Security > Microphone     -> toggle Hexavoice on
                                                           (prompted on first run)

Then start it so it is its own responsible process:
  open -a "$APP_DIR"

It has no Dock icon by design (it must never steal focus from the field it
is typing into). Look for its icon at the RIGHT END OF YOUR MENU BAR: a
waveform when idle, a filled microphone while listening. Click it for
status, settings, diagnostics, and Quit.

Ad-hoc signing note: the grant is pinned to this build's cdhash, so after a
rebuild run \`tccutil reset Accessibility $BUNDLE_ID\` and re-grant.
EOF
