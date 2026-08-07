#!/usr/bin/env bash
# Build the macOS universal binary, .app bundle, Developer ID signature,
# notarization, stapling, and DMG.
#
# WHY this exists alongside scripts/bundle-macos.sh: bundle-macos.sh is the
# developer loop (host-arch build, ad-hoc signature). This script is the
# release path. They are separate because the release path must never be
# weakened "temporarily" to make local iteration faster.
#
# Signing is env-gated so the same script runs on a laptop with no
# certificate (produces an ad-hoc bundle, clearly labelled) and in CI with
# the real Developer ID (produces the shippable artifact).
#
# Required env for a real release:
#   MACOS_SIGN_IDENTITY   "Developer ID Application: Example Corp (TEAMID)"
#   MACOS_NOTARY_PROFILE  a notarytool keychain profile name, created with:
#       xcrun notarytool store-credentials <name> --apple-id .. --team-id .. \
#             --password <app-specific-password>
#     (CI creates it from secrets; see .github/workflows/release.yml)
#
# WHY real signing fixes the TCC problem from docs/macos-permissions.md:
# an ad-hoc signature has no chain of trust, so TCC pins the Accessibility
# grant to the exact cdhash of one build; every rebuild orphans the grant
# while the toggle still reads "on". A Developer ID signature lets us set a
# *designated requirement* anchored to the team certificate + bundle id, so
# TCC's stored approval matches every future build signed by the same team.
# The grant becomes durable across updates, which is the difference between
# "onboarding once" and "app randomly breaks after every update".

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="OutLoudSpike"
BUNDLE_ID="dev.hexavoice.spike"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
APP_DIR="dist/$APP_NAME.app"
DMG_PATH="dist/$APP_NAME-$VERSION-universal.dmg"

echo "==> Building both architectures"
# Both slices from the same source and toolchain; rust-toolchain.toml already
# lists both targets so this works on a fresh clone of either Mac arch.
cargo build --release --locked --target aarch64-apple-darwin
cargo build --release --locked --target x86_64-apple-darwin

echo "==> lipo universal binary"
mkdir -p dist
lipo -create \
    target/aarch64-apple-darwin/release/spike-cli \
    target/x86_64-apple-darwin/release/spike-cli \
    -output dist/spike-cli-universal
lipo -info dist/spike-cli-universal

echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp dist/spike-cli-universal "$APP_DIR/Contents/MacOS/$APP_NAME"

# Info.plist matches scripts/bundle-macos.sh (the canonical definition of the
# bundle identity); duplicated rather than sourced because that script also
# rebuilds for host arch only, which is exactly what a release must not do.
# The icon. Without this a downloaded release shows the generic blank
# document icon in Finder and the Dock, while a locally built bundle shows
# the skull -- so the version strangers see is the one nobody tests.
#
# Non-fatal, matching scripts/bundle-outloud-macos.sh: an iconless build is
# worth shipping, a failed release is not. But stderr is NOT discarded, or
# the reason is lost and a blank icon looks identical to a missing one.
mkdir -p "$APP_DIR/Contents/Resources"
icon_log="$(mktemp)"
if "$ROOT/scripts/make-icon.sh" "$APP_DIR/Contents/Resources/$APP_NAME.icns" \
        >"$icon_log" 2>&1; then
    echo "==> icon rendered from docs/assets/logo.svg"
    grep -E "^ +icon rasterized" "$icon_log" || true
    ICON_KEY="    <key>CFBundleIconFile</key>
    <string>$APP_NAME</string>"
else
    echo "==> WARNING: could not render the icon; the release will use the generic one" >&2
    sed 's/^/    /' "$icon_log" >&2
    ICON_KEY=""
fi
rm -f "$icon_log"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>OutLoud Spike</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
$ICON_KEY
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSAppleEventsUsageDescription</key>
    <string>OutLoud Spike identifies the frontmost application to choose formatting rules.</string>
</dict>
</plist>
PLIST

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    echo "==> Signing with Developer ID: $MACOS_SIGN_IDENTITY"
    # --options runtime (hardened runtime) is REQUIRED for notarization.
    # --timestamp embeds a secure timestamp so the signature outlives the
    # signing certificate's expiry.
    codesign --force --options runtime --timestamp \
        --sign "$MACOS_SIGN_IDENTITY" \
        --identifier "$BUNDLE_ID" \
        "$APP_DIR"
    codesign --verify --strict --verbose=2 "$APP_DIR"

    if [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
        echo "==> Notarizing"
        # notarytool wants a zip or dmg, not a bare .app directory.
        ditto -c -k --keepParent "$APP_DIR" dist/notarize-upload.zip
        # --wait blocks until Apple's verdict; typically 1-5 minutes. A
        # rejection prints a log URL; `notarytool log <id>` has the details.
        xcrun notarytool submit dist/notarize-upload.zip \
            --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
        rm -f dist/notarize-upload.zip

        echo "==> Stapling ticket"
        # Stapling attaches the notarization ticket to the bundle so
        # Gatekeeper can verify it OFFLINE. Without stapling, first launch
        # on an offline machine is blocked even though notarization passed.
        xcrun stapler staple "$APP_DIR"
        xcrun stapler validate "$APP_DIR"
    else
        echo "==> MACOS_NOTARY_PROFILE unset: skipping notarization (artifact will be Gatekeeper-quarantined on other machines)"
    fi
else
    echo "==> MACOS_SIGN_IDENTITY unset: ad-hoc signing (LOCAL USE ONLY, see docs/macos-permissions.md for why this must never ship)"
    codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_DIR"
fi

echo "==> Creating DMG"
# hdiutil rather than create-dmg: zero extra dependencies, and a plain
# read-only compressed image is all a CLI tool needs. UDZO is readable on
# every macOS version we support.
rm -f "$DMG_PATH"
STAGE="$(mktemp -d)"
cp -R "$APP_DIR" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG_PATH"
rm -rf "$STAGE"

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    # The DMG itself is signed too, so the download is verifiable before
    # the user even opens it.
    codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$DMG_PATH"
    if [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$DMG_PATH" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
        xcrun stapler staple "$DMG_PATH"
    fi
fi

echo
echo "Built: $DMG_PATH"
shasum -a 256 "$DMG_PATH"
