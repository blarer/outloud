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

APP_NAME="OutLoud"
# Matches scripts/bundle-outloud-macos.sh. The bundle identifier is load
# bearing: TCC attaches the Accessibility and Microphone grants to it, so a
# release that ships a different identifier than the developer bundle asks
# the user to re-approve everything, and a release that ships the HARNESS's
# identifier (dev.hexavoice.spike, which this script used until 2026-10-30)
# grants permissions to a program the user never meant to run.
BUNDLE_ID="dev.outloud.outloud"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
APP_DIR="dist/$APP_NAME.app"
DMG_PATH="dist/$APP_NAME-$VERSION-universal.dmg"
# The asset scripts/install.sh downloads. A DMG is for people who drag
# icons; the installer needs a tarball it can unpack without mounting
# anything. Both are shipped.
TARBALL_PATH="dist/OutLoud-macos-universal.tar.gz"

echo "==> Building both architectures"
# Both slices from the same source and toolchain; rust-toolchain.toml already
# lists both targets so this works on a fresh clone of either Mac arch.
#
# `-p outloud`: the DICTATION DAEMON, which is the program a user runs.
#
# This script built and shipped `spike-cli` until 2026-10-30, so the macOS
# DMG contained the accessibility development harness and no way to dictate.
# That is the same defect 36bb9fe fixed for Windows and 27b59c2 fixed for
# Linux, here on the platform the project treats as primary, and it survived
# longest because the DMG was never opened and checked: the app launched, so
# nothing looked wrong. Verified by the absence of whisper/ggml symbols and
# the presence of the harness's bundle id in the shipped artifact.
cargo build --release --locked -p outloud --target aarch64-apple-darwin
cargo build --release --locked -p outloud --target x86_64-apple-darwin

echo "==> lipo universal binary"
mkdir -p dist
lipo -create \
    target/aarch64-apple-darwin/release/outloud \
    target/x86_64-apple-darwin/release/outloud \
    -output dist/outloud-universal
lipo -info dist/outloud-universal

# The Apple recognizer is a Swift CHILD PROCESS, not a linked library, so it
# ships as a second executable inside the bundle. Without it the daemon
# starts and then cannot transcribe anything, which is the failure mode this
# whole commit is about, so here it is fatal rather than a warning: a release
# that cannot transcribe is not a release. (bundle-outloud-macos.sh warns
# instead, correctly, because `--asr mock` is useful in the dev loop.)
HELPER_SRC="crates/asr/helper/transcriber.swift"
if ! command -v swiftc >/dev/null 2>&1; then
    echo "FATAL: swiftc not found, so the speech helper cannot be built." >&2
    echo "  The macOS recognizer is a Swift child process; without it the" >&2
    echo "  daemon runs and silently cannot transcribe. Install the Xcode" >&2
    echo "  Command Line Tools." >&2
    exit 1
fi
echo "==> Building the speech helper (universal)"
# Built per-arch and lipo'd for the same reason as the daemon: a release
# artifact must run on both architectures, and a host-arch helper would make
# the universal daemon fail on the other one.
#
# macos26 is NOT a free choice. transcriber.swift uses SpeechAnalyzer
# (`AnalyzerInput`, `result.isFinal`), which is macOS 26 only; building it
# against an older deployment target fails outright:
#
#   error: 'AnalyzerInput' is only available in macOS 26.0 or newer
#
# Found by running this script locally with `-target ...-macos13`, chosen to
# match the bundle's LSMinimumSystemVersion. The mismatch is real and
# deliberate: the DAEMON runs on macOS 13, and on an older system it falls
# back to whisper rather than the Apple recognizer, so shipping a helper
# that only the newest systems can load is correct. The release notes
# already say macOS 26 for the Apple-recognizer path.
HELPER_TARGET_VERSION=26
swiftc -O -target "arm64-apple-macos$HELPER_TARGET_VERSION" "$HELPER_SRC" -o dist/helper-arm64
swiftc -O -target "x86_64-apple-macos$HELPER_TARGET_VERSION" "$HELPER_SRC" -o dist/helper-x86_64
lipo -create dist/helper-arm64 dist/helper-x86_64 -output dist/outloud-speech-helper-universal
lipo -info dist/outloud-speech-helper-universal
rm -f dist/helper-arm64 dist/helper-x86_64

echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp dist/outloud-universal "$APP_DIR/Contents/MacOS/$APP_NAME"
# NOT renamed with the product: crates/asr's find_helper() looks for this
# exact filename. See the same note in bundle-outloud-macos.sh.
cp dist/outloud-speech-helper-universal "$APP_DIR/Contents/MacOS/outloud-speech-helper"

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
    <string>OutLoud</string>
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
    <key>NSMicrophoneUsageDescription</key>
    <string>OutLoud transcribes your speech on this device to type it for you.</string>
    <key>NSSpeechRecognitionUsageDescription</key>
    <string>OutLoud uses the on-device speech recognizer; no audio leaves this Mac.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>OutLoud identifies the frontmost application to choose formatting rules.</string>
</dict>
</plist>
PLIST

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    echo "==> Signing with Developer ID: $MACOS_SIGN_IDENTITY"
    # INSIDE OUT. The speech helper is a nested executable, and codesign does
    # not sign nested code implicitly: signing only the bundle leaves the
    # helper unsigned, `codesign --verify --strict` rejects it, and
    # notarization rejects it too. Nested first, bundle second.
    codesign --force --options runtime --timestamp \
        --sign "$MACOS_SIGN_IDENTITY" \
        "$APP_DIR/Contents/MacOS/outloud-speech-helper"
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
    # Nested code first here too; see the signed branch above.
    codesign --force --sign - "$APP_DIR/Contents/MacOS/outloud-speech-helper"
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
echo "==> Creating the installer tarball"
# scripts/install.sh, which the release notes recommend FIRST, downloads
# `OutLoud-macos-*.tar.gz` and looks for `OutLoud.app` inside it. Nothing in
# the release workflow produced such a file: the only asset was a DMG. The
# asset on the August release was uploaded by hand, so the installer worked
# only by virtue of that one manual upload and would have broken on the next
# tag, telling the user "no OutLoud-macos-arm64.tar.gz in the latest
# release".
#
# ditto rather than tar: it preserves resource forks, extended attributes
# and the code signature. A `tar` round-trip can invalidate the signature,
# which turns into a Gatekeeper rejection on the user's machine and not on
# ours.
rm -f "$TARBALL_PATH"
ditto -c -k --keepParent --sequesterRsrc "$APP_DIR" "dist/OutLoud-macos-universal.zip"
# Both formats: install.sh expects a .tar.gz, and a zip is what ditto makes
# natively. Keeping the tarball name the installer already looks for avoids
# changing the installer and the asset at the same time.
STAGE="$(mktemp -d)"
cp -R "$APP_DIR" "$STAGE/"
(cd "$STAGE" && COPYFILE_DISABLE=1 tar -czf - "$APP_NAME.app") > "$TARBALL_PATH"
rm -rf "$STAGE"

# --- prove the artifact is the product, not the harness ------------------
#
# This script shipped the wrong binary for three months, and every check
# that existed still passed, because "a signed bundle that launches" was
# never the same question as "the dictation app". These assertions are
# cheap and they are the reason that cannot recur silently.
echo
echo "==> Verifying the artifact"
BUNDLED_BIN="$APP_DIR/Contents/MacOS/$APP_NAME"
fail=0
if ! lipo -archs "$BUNDLED_BIN" | grep -q "arm64" \
   || ! lipo -archs "$BUNDLED_BIN" | grep -q "x86_64"; then
    echo "  FAIL: not universal: $(lipo -archs "$BUNDLED_BIN")" >&2
    fail=1
else
    echo "  ok: universal ($(lipo -archs "$BUNDLED_BIN"))"
fi
# The daemon links the audio stack and the Apple recognizer bridge; the
# harness does not. A nonzero count is the cheapest honest discriminator
# between the two binaries.
if [ "$(strings -a "$BUNDLED_BIN" | grep -c 'AudioToolbox\|AVAudio\|cpal')" -eq 0 ]; then
    echo "  FAIL: no audio-stack references. This looks like the harness, not the daemon." >&2
    fail=1
else
    echo "  ok: audio stack referenced"
fi
if [ ! -x "$APP_DIR/Contents/MacOS/outloud-speech-helper" ]; then
    echo "  FAIL: the speech helper is missing; the daemon cannot transcribe." >&2
    fail=1
else
    echo "  ok: speech helper present ($(lipo -archs "$APP_DIR/Contents/MacOS/outloud-speech-helper"))"
fi
if ! /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP_DIR/Contents/Info.plist" 2>/dev/null \
      | grep -qx "$BUNDLE_ID"; then
    echo "  FAIL: Info.plist identifier is not $BUNDLE_ID." >&2
    fail=1
else
    echo "  ok: bundle id $BUNDLE_ID"
fi
# NSMicrophoneUsageDescription is mandatory: a process that opens an input
# device without it is KILLED by the system rather than denied, so its
# absence is a crash on first use, not a prompt.
if ! /usr/libexec/PlistBuddy -c 'Print :NSMicrophoneUsageDescription' "$APP_DIR/Contents/Info.plist" >/dev/null 2>&1; then
    echo "  FAIL: NSMicrophoneUsageDescription missing; macOS kills the process on mic access." >&2
    fail=1
else
    echo "  ok: NSMicrophoneUsageDescription present"
fi
if [ "$fail" -ne 0 ]; then
    echo "Refusing to publish this artifact." >&2
    exit 1
fi

echo
echo "Built: $DMG_PATH"
shasum -a 256 "$DMG_PATH"
echo "Built: $TARBALL_PATH"
shasum -a 256 "$TARBALL_PATH"
echo "Built: dist/OutLoud-macos-universal.zip"
shasum -a 256 dist/OutLoud-macos-universal.zip
