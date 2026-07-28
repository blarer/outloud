#!/usr/bin/env bash
# Linux packaging: tarball, AppImage, .deb, .rpm, and a Flatpak manifest.
# Runs on an ubuntu runner (or any Linux with the tools installed); each
# format is emitted only when its tool is present so the script degrades
# gracefully on a laptop, while CI installs everything and produces all four.
#
# Format rationale:
#   tarball  - the universal fallback; the musl build inside it runs anywhere.
#   AppImage - one-file download that works on any distro without root.
#   .deb/.rpm - what distro users actually expect; also the vehicle for
#              declaring runtime deps once the input-injection backend lands.
#   Flatpak  - manifest only (flatpak-builder needs a full runtime install);
#              built in CI's dedicated job. See flatpak notes below: an
#              accessibility/input tool is a WORST-CASE flatpak citizen and
#              the manifest documents exactly which holes it must punch.
#
# Wayland vs X11: this project's Linux backend does not exist yet, but the
# packaging must not paint it into a corner. The story (also in
# docs/build-and-release.md#linux):
#   - X11: XTEST + AT-SPI2 work everywhere, no permission model.
#   - Wayland: no global input injection by design. The paths are the
#     RemoteDesktop portal (org.freedesktop.portal.RemoteDesktop, needs user
#     consent per session unless the compositor persists it), compositor
#     virtual-keyboard protocols (zwp_virtual_keyboard_v1 / input-method-v2,
#     wlroots-family only), or libei on GNOME 45+.
#   - Detection is runtime, not compile time: check WAYLAND_DISPLAY first,
#     fall back to DISPLAY, since XWayland sets both and native Wayland must
#     win. Both backends compile into every Linux binary; a compile-time
#     choice would force distros to ship two packages.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${1:-x86_64-unknown-linux-gnu}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
ARCH="${TARGET%%-*}"                       # x86_64 | aarch64
DEB_ARCH="$([ "$ARCH" = x86_64 ] && echo amd64 || echo arm64)"
OUT="dist/linux/$TARGET"
BIN="target/$TARGET/release/spike-cli"

echo "==> Building $TARGET"
case "$TARGET" in
    aarch64-*)
        # aarch64 cross-builds go through cross (pinned images, Cross.toml).
        command -v cross >/dev/null || cargo install cross --locked
        cross build --release --locked --package spike-cli --target "$TARGET"
        ;;
    *)
        rustup target add "$TARGET"
        cargo build --release --locked --package spike-cli --target "$TARGET"
        ;;
esac

mkdir -p "$OUT"
cp "$BIN" "$OUT/outloud-spike"

echo "==> tarball"
tar -C "$OUT" -czf "$OUT/outloud-spike-$VERSION-$TARGET.tar.gz" outloud-spike

# ---------------------------------------------------------------- AppImage
if command -v appimagetool >/dev/null 2>&1; then
    echo "==> AppImage"
    APPDIR="$OUT/AppDir"
    rm -rf "$APPDIR"
    mkdir -p "$APPDIR/usr/bin"
    cp "$OUT/outloud-spike" "$APPDIR/usr/bin/"
    cat > "$APPDIR/outloud-spike.desktop" <<DESKTOP
[Desktop Entry]
Name=OutLoud Spike
Exec=outloud-spike
Icon=outloud-spike
Type=Application
Categories=Utility;Accessibility;
Terminal=true
DESKTOP
    # 1x1 placeholder icon; appimagetool requires one to exist.
    printf '\x89PNG\r\n\x1a\n' > "$APPDIR/outloud-spike.png" || true
    cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
# AppRun decides nothing about Wayland/X11: the binary itself does runtime
# detection (WAYLAND_DISPLAY, then DISPLAY) so one AppImage serves both.
exec "$(dirname "$0")/usr/bin/outloud-spike" "$@"
APPRUN
    chmod +x "$APPDIR/AppRun"
    # ARCH is read by appimagetool for the AppImage metadata.
    ARCH="$ARCH" appimagetool "$APPDIR" "$OUT/outloud-spike-$VERSION-$ARCH.AppImage"
else
    echo "==> appimagetool not found: skipping AppImage"
fi

# ---------------------------------------------------------------- .deb
if command -v dpkg-deb >/dev/null 2>&1; then
    echo "==> .deb"
    DEB="$OUT/deb"
    rm -rf "$DEB"
    mkdir -p "$DEB/usr/bin" "$DEB/DEBIAN" "$DEB/usr/share/doc/outloud-spike"
    cp "$OUT/outloud-spike" "$DEB/usr/bin/"
    cp LICENSE* "$DEB/usr/share/doc/outloud-spike/" 2>/dev/null || true
    cat > "$DEB/DEBIAN/control" <<CONTROL
Package: outloud-spike
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: aquaoss project
Section: utils
Priority: optional
Description: Local edit-by-voice spike harness
 Milestone-zero harness for reading and rewriting focused text fields.
CONTROL
    dpkg-deb --build --root-owner-group "$DEB" "$OUT/outloud-spike_${VERSION}_${DEB_ARCH}.deb"
else
    echo "==> dpkg-deb not found: skipping .deb"
fi

# ---------------------------------------------------------------- .rpm
if command -v rpmbuild >/dev/null 2>&1; then
    echo "==> .rpm"
    RPMTOP="$OUT/rpmbuild"
    rm -rf "$RPMTOP"
    mkdir -p "$RPMTOP"/{SPECS,BUILD,RPMS,SOURCES}
    cat > "$RPMTOP/SPECS/outloud-spike.spec" <<SPEC
Name: outloud-spike
Version: $VERSION
Release: 1
Summary: Local edit-by-voice spike harness
License: MIT
%description
Milestone-zero harness for reading and rewriting focused text fields.
%install
mkdir -p %{buildroot}/usr/bin
install -m 0755 $ROOT/$OUT/outloud-spike %{buildroot}/usr/bin/outloud-spike
%files
/usr/bin/outloud-spike
SPEC
    rpmbuild --define "_topdir $ROOT/$RPMTOP" \
             --define "_rpmdir $ROOT/$OUT" \
             --target "$ARCH" -bb "$RPMTOP/SPECS/outloud-spike.spec"
else
    echo "==> rpmbuild not found: skipping .rpm"
fi

# ---------------------------------------------------------------- Flatpak
echo "==> Flatpak manifest"
cat > "$OUT/dev.hexavoice.spike.yml" <<'FLATPAK'
# Flatpak manifest for the spike harness.
#
# An input-injection accessibility tool is the hardest possible Flatpak case:
# the sandbox exists precisely to prevent what this tool does. Every hole
# below is therefore deliberate and reviewed:
#
#   --socket=wayland / fallback-x11: talk to whichever display is present;
#     the binary picks at runtime (WAYLAND_DISPLAY before DISPLAY).
#   --talk-name=org.a11y.Bus + ally-bus proxy: AT-SPI2 access for READING
#     the focused text field. Without this the flatpak build can read
#     nothing and the app is pointless.
#   RemoteDesktop portal (org.freedesktop.portal.RemoteDesktop): the ONLY
#     sanctioned way to inject input on Wayland from a sandbox. It prompts
#     the user; with xdg-desktop-portal >= 1.16 the grant can persist. We do
#     NOT request --device=all or try to open /dev/uinput: that would be
#     rejected by Flathub review and rightly so.
#
# Flathub will still scrutinize this app. That is expected and healthy; the
# alternative (telling flatpak users to run the raw binary) is worse.
app-id: dev.hexavoice.spike
runtime: org.freedesktop.Platform
runtime-version: '24.08'
sdk: org.freedesktop.Sdk
sdk-extensions:
  - org.freedesktop.Sdk.Extension.rust-stable
command: outloud-spike
finish-args:
  - --socket=wayland
  - --socket=fallback-x11
  - --share=ipc                # required by X11
  - --talk-name=org.a11y.Bus   # AT-SPI2: read focused text field
  # Input injection goes through the RemoteDesktop portal at runtime; portals
  # need no finish-args, the portal service brokers the permission prompt.
build-options:
  append-path: /usr/lib/sdk/rust-stable/bin
  env:
    CARGO_HOME: /run/build/outloud-spike/cargo
modules:
  - name: outloud-spike
    buildsystem: simple
    build-commands:
      # --offline: flatpak-builder builds have no network; cargo sources are
      # vendored by the generated cargo-sources.json (flatpak-cargo-generator
      # from Cargo.lock, run by the CI flatpak job).
      - cargo --offline build --release --locked --package spike-cli
      - install -Dm755 target/release/spike-cli /app/bin/outloud-spike
    sources:
      - type: dir
        path: ../../..
FLATPAK

ls -l "$OUT"
