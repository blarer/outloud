#!/usr/bin/env bash
# Package the outloud daemon as a macOS .app bundle.
#
# WHY this exists alongside scripts/bundle-macos.sh: that script bundles
# spike-cli, the accessibility development harness. The thing a user actually
# runs is outloud, and it needs the same two things spike-cli needs:
#
#   1. A stable bundle identity, because TCC attaches the Accessibility grant
#      to a CFBundleIdentifier + code signature, not to a path.
#   2. To be its own responsible process. A binary started from a shell
#      inherits the terminal as its responsible process, so macOS checks the
#      *terminal's* permissions and ignores the binary's own grant entirely.
#      See docs/macos-permissions.md.
#
# Running ./target/release/outloud directly still works, but then the grants you
# must approve are the terminal's, which is a confusing thing to ask of a user
# and silently changes meaning when they switch terminals.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="OutLoud"
BUNDLE_ID="dev.outloud.outloud"
# The identifier the app shipped under before the OutLoud rename. Old bundles
# on disk still declare it, so LaunchServices cleanup below must consider
# them too; drop this once no pre-rename bundles remain in the wild.
LEGACY_BUNDLE_ID="dev.hexavoice.hexad"
# LaunchServices' registration tool. Not on PATH; the full path is stable
# across macOS versions.
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
APP_DIR="$ROOT/dist/$APP_NAME.app"

echo "==> Building outloud (release)"
cargo build --release -p outloud --manifest-path "$ROOT/Cargo.toml"

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
    echo "         outloud will start but cannot transcribe (use --asr mock)." >&2
fi

echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

# The icon is generated from the same SVG the README renders, so the app and
# the project page can never drift apart. Failure here is a warning rather than
# an error: an iconless build is worth shipping, a broken build is not.
if "$ROOT/scripts/make-icon.sh" "$APP_DIR/Contents/Resources/OutLoud.icns" >/dev/null 2>&1; then
    echo "==> icon rendered from docs/assets/logo.svg"
else
    echo "==> WARNING: could not render the icon; the bundle will use the generic one"
fi
cp "$ROOT/target/release/outloud" "$APP_DIR/Contents/MacOS/$APP_NAME"
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
    <string>OutLoud</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIconFile</key>
    <string>OutLoud</string>
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
    <string>OutLoud transcribes your speech on this device to type it for you.</string>
    <key>NSSpeechRecognitionUsageDescription</key>
    <string>OutLoud uses the on-device speech recognizer; no audio leaves this Mac.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>OutLoud identifies the frontmost application to choose formatting rules.</string>
</dict>
</plist>
PLIST

echo "==> Signing ad-hoc"
# --identifier sets the bundle id. It does NOT make the TCC grant survive a
# rebuild, and an earlier version of this comment claimed it did, which sent
# a user chasing a permission bug that was really this:
#
#   $ codesign -d -r- dist/OutLoud.app
#   designated => cdhash H"19c2e3f9..."
#
# An ad-hoc signature has no signing identity, so the Designated Requirement
# degenerates to the hash of one exact binary. Every rebuild is a different
# app as far as macOS is concerned: it denies the new one and leaves the old
# entry sitting in System Settings still switched on, which reads as
# "already granted" while nothing works.
#
# The real fix is a Developer ID certificate, where the requirement becomes
# the identity rather than a hash and grants persist. Until then, re-granting
# after every rebuild is unavoidable, so the script clears the stale entries
# for you rather than letting them accumulate and mislead.
codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_DIR"
codesign --verify --verbose=2 "$APP_DIR" 2>&1 | sed 's/^/    /'

if [[ "${OUTLOUD_KEEP_TCC:-0}" != "1" ]]; then
    # Best-effort: fails harmlessly when nothing was granted yet.
    tccutil reset Accessibility "$BUNDLE_ID" >/dev/null 2>&1 || true
    tccutil reset ListenEvent "$BUNDLE_ID" >/dev/null 2>&1 || true
    # The legacy identifier too: a grant made before the rename is pinned to
    # it and is unreachable from the renamed app, so leaving it makes System
    # Settings show a stale "already granted" entry that explains nothing.
    tccutil reset Accessibility "$LEGACY_BUNDLE_ID" >/dev/null 2>&1 || true
    tccutil reset ListenEvent "$LEGACY_BUNDLE_ID" >/dev/null 2>&1 || true
    echo "==> Cleared the stale permission entries this rebuild invalidated"
fi

# Unregister any OTHER bundle claiming our identifier, current or legacy.
#
# Two product renames left Hexavoice.app and AquaSpike.app bundles on disk,
# and copies under /tmp from audit runs, ALL declaring dev.hexavoice.hexad.
# LaunchServices happily indexes every one of them, then resolves the id to
# whichever it likes. The user saw the symptom: granting Accessibility
# brought up a bundle with the pre-rename star icon, because the winning
# record pointed at an old app that has no icon of its own.
#
# This is not cosmetic. The permission is granted to whichever bundle
# LaunchServices resolved, so a stale winner means the grant lands on an app
# that is not the one running.
#
# The legacy identifier is included because old bundles declare IT, not the
# new one: after the rename they would no longer look like rivals and could
# win a resolution for the old id, which is exactly the confusion above.
# Legacy cleanup only; drop it once no old bundles remain in the wild.
#
# Only the freshly built bundle should answer to this identifier, so drop
# the rest from the database. The bundles themselves are left alone: this
# script does not delete anything a human might still want.
stale=$(
    "$LSREGISTER" -dump 2>/dev/null \
        | awk -v id="$BUNDLE_ID" -v legacy="$LEGACY_BUNDLE_ID" '
            /^path:/ { p = $2 }
            $1 == "identifier:" && ($2 == id || $2 == legacy) { print p }
          ' \
        | sort -u \
        | grep -v "^$APP_DIR$" || true
)
if [[ -n "$stale" ]]; then
    echo "==> Other bundles claim $BUNDLE_ID or $LEGACY_BUNDLE_ID; unregistering so this build wins:"
    while IFS= read -r path; do
        [[ -n "$path" ]] || continue
        echo "    $path"
        "$LSREGISTER" -u "$path" 2>/dev/null || true
    done <<< "$stale"
fi
# Re-register ours last so it is unambiguously the current record, then
# nudge the icon cache. LaunchServices resolving to the right bundle is not
# sufficient on its own: macOS caches the rendered icon per bundle, so a
# corrected registration can still be drawn with a previous build's artwork.
# Touching the bundle invalidates that cache entry.
"$LSREGISTER" -f "$APP_DIR" 2>/dev/null || true
touch "$APP_DIR"

cat <<EOF

Built: $APP_DIR

Grant permissions once (macOS gives no programmatic way to do this):
  System Settings > Privacy & Security > Accessibility  -> add $APP_DIR, toggle on
  System Settings > Privacy & Security > Microphone     -> toggle OutLoud on
                                                           (prompted on first run)

Then start it so it is its own responsible process:
  open -a "$APP_DIR"

It has no Dock icon by design (it must never steal focus from the field it
is typing into). Look for its icon at the RIGHT END OF YOUR MENU BAR: a
waveform when idle, a filled microphone while listening. Click it for
status, settings, diagnostics, and Quit.

Grant BOTH permissions. They are different, and each fails differently:
  Input Monitoring  -> without it the hotkey never fires. Nothing happens
  Accessibility     -> without it text lands via clipboard paste, not in place

Ad-hoc signing note: the Designated Requirement is this build's cdhash, so
EVERY REBUILD invalidates both grants. The stale entries have already been
cleared for you; re-add the app in both panes. A Developer ID certificate is
what makes this stop.
EOF
