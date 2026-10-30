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
# The dictation daemon: the thing a user actually runs.
#
# This script shipped ONLY `spike-cli` until 2026-10-30, so every `.deb`,
# `.rpm` and tarball it produced contained a diagnostic harness and no way to
# dictate. That is the same defect `36bb9fe` fixed for Windows nine days
# earlier ("a dictation app that could not transcribe"), in the one place it
# was not also fixed, and nothing caught it because no CI job installs a
# Linux package and runs it.
DAEMON="target/$TARGET/release/outloud"

# GLIBC ONLY, and this is a constraint rather than a preference.
#
# The daemon's default features pull cpal, whose `alsa-sys` build script runs
# `pkg-config --exists alsa`. On musl that cannot be satisfied: the ALSA dev
# package on the runner is the HOST's, and a musl target needs its own, which
# is exactly what `scripts/ci-install-linux-deps.sh` says it deliberately
# does not provide. Those targets exist to produce a *static headless* binary
# that has no business linking an audio stack it never calls.
#
# So musl keeps shipping the harness alone. A `--no-default-features` daemon
# would build there and then refuse every utterance for lack of a microphone
# backend, which is a worse artifact than an honestly absent one.
case "$TARGET" in
    *-musl) WANT_DAEMON=0 ;;
    *)      WANT_DAEMON=1 ;;
esac

echo "==> Building $TARGET"
# --features outloud/whisper, for the same reason as Windows: whisper.cpp is
# the only recognizer that exists off macOS and the feature is off by default
# (it compiles whisper.cpp from source and needs cmake). Without it the
# daemon starts, fetches a 147MB model, and then fails every utterance with
# "this build has no whisper backend".
PACKAGES=(--package spike-cli)
FEATURES=()
if [ "$WANT_DAEMON" = 1 ]; then
    PACKAGES+=(--package outloud)
    FEATURES+=(--features outloud/whisper)
    # alsa-sys needs libasound2-dev, cmake builds whisper.cpp. The deps
    # script is a no-op when they are already present.
    ./scripts/ci-install-linux-deps.sh || true
    command -v cmake >/dev/null || {
        echo "ERROR: cmake is required to build the whisper recognizer." >&2
        echo "Install it (apt-get install -y cmake) or run with a musl target," >&2
        echo "which ships the harness only." >&2
        exit 1
    }

    # GGML_NATIVE=OFF, and this is not optional for a SHIPPED artifact.
    #
    # ggml defaults it ON, which tunes for whatever CPU the build ran on.
    # The first release build that included the daemon produced
    #
    #   FAIL: 4870 post-baseline (AVX/AVX2/BMI2) instructions found.
    #
    # from `ci-verify-baseline.sh`, because the runner had AVX2 and ggml
    # compiled for it. That is precisely the pre-Haswell crash class the
    # guard exists for: a binary built on a modern runner and SIGILLing at
    # process start on a Sandy Bridge or older machine, before any
    # cpu-detection code of ours can run. Same root cause as the AMX SIGILL
    # in the whisper CI job, seen in a shipped artifact instead of a test.
    #
    # The cost is real and accepted: a baseline build is slower than one
    # tuned for the host. Correct-everywhere beats fast-on-the-builder for
    # something strangers download, and the measurements in
    # docs/investigations/whisper-cpu-latency.md show whisper's CPU cost is
    # dominated by its fixed 30s encoder window rather than by SIMD width,
    # so the loss is smaller than it sounds. ggml's GGML_CPU_ALL_VARIANTS
    # runtime dispatch is the better long-term answer; it needs
    # GGML_BACKEND_DL and shared objects, which changes packaging, so it is
    # a deliberate follow-up rather than something to slip in here.
    #
    # Passed through CMAKE_TOOLCHAIN_FILE because cmake-rs reads that
    # (`getenv_target_os`) and does NOT read CMAKE_ARGS, and whisper-rs-sys
    # exposes no way to add a define.
    GGML_TOOLCHAIN="$(mktemp -t ggml-baseline-XXXXXX.cmake)"
    cat > "$GGML_TOOLCHAIN" <<'CMAKE'
# Generated by scripts/build-linux.sh. Keeps ggml off the build machine's
# instruction set so the shipped binary runs on any x86-64 CPU. Verified by
# scripts/ci-verify-baseline.sh, which fails the release if this regresses.
set(GGML_NATIVE OFF CACHE BOOL "" FORCE)
CMAKE
    export CMAKE_TOOLCHAIN_FILE="$GGML_TOOLCHAIN"
    echo "    ggml: GGML_NATIVE=OFF (x86-64 baseline) via $GGML_TOOLCHAIN"
fi

case "$TARGET" in
    aarch64-*)
        # Native when the runner is already arm64, cross otherwise.
        #
        # Native is strongly preferred, and the release workflow now puts
        # the aarch64 rows on `ubuntu-24.04-arm` so this is the path CI
        # takes. Reason: the cross 0.2.5 aarch64 image is Ubuntu *xenial*,
        # which ships cmake 3.5 and libclang 3.8. ggml's CMakeLists requires
        # cmake >= 3.14, and xenial cannot satisfy it (`cmake : Depends:
        # cmake-data (= 3.5.1-1ubuntu3)` conflicts with any newer
        # cmake-data). Building whisper.cpp inside that image means
        # backporting a toolchain into a 2016 userland.
        #
        # Everything the cross path needed the elaborate Cross.toml
        # pre-build for (arm64 ALSA headers extracted with `dpkg -x`,
        # ports.ubuntu.com sources, a glibc floor set by the image's
        # sysroot) is simply absent natively: apt installs arm64
        # libasound2-dev because the machine IS arm64.
        #
        # The cross branch is kept, not deleted, because it is the only way
        # to build this target on an x86_64 developer machine. It cannot
        # build the daemon (no cmake), so it is restricted to the harness,
        # and says so rather than failing 90 seconds into a whisper build.
        if [ "$(uname -m)" = "aarch64" ]; then
            rustup target add "$TARGET"
            cargo build --release --locked "${PACKAGES[@]}" "${FEATURES[@]}" \
                --target "$TARGET"
        else
            command -v cross >/dev/null || cargo install cross --locked
            if [ "$WANT_DAEMON" = 1 ]; then
                echo "==> cross-building aarch64 from $(uname -m): harness only"
                echo "    (the cross image's cmake is too old for whisper.cpp;"
                echo "     build on an arm64 host for a daemon-bearing package)"
                WANT_DAEMON=0
                PACKAGES=(--package spike-cli)
                FEATURES=()
            fi
            cross build --release --locked "${PACKAGES[@]}" "${FEATURES[@]}" \
                --target "$TARGET"
        fi
        ;;
    *)
        rustup target add "$TARGET"
        cargo build --release --locked "${PACKAGES[@]}" "${FEATURES[@]}" \
            --target "$TARGET"
        ;;
esac

mkdir -p "$OUT"
cp "$BIN" "$OUT/outloud-spike"

echo "==> tarball"
if [ "$WANT_DAEMON" = 1 ]; then
    cp "$DAEMON" "$OUT/outloud"
    tar -C "$OUT" -czf "$OUT/outloud-$VERSION-$TARGET.tar.gz" outloud outloud-spike
else
    echo "    (musl: harness only, see the WANT_DAEMON comment above)"
    tar -C "$OUT" -czf "$OUT/outloud-spike-$VERSION-$TARGET.tar.gz" outloud-spike
fi

# ---------------------------------------------------------------- AppImage
#
# BOTH tools, not just appimagetool. appimagetool shells out to
# desktop-file-validate and exits 1 when it is absent ("desktop-file-validate
# command is missing, please install it"), which killed all three Linux jobs
# of the v0.1.0 release while every other target succeeded. The script
# already degrades gracefully when appimagetool itself is missing; it was
# only the transitive dependency that turned a missing optional tool into a
# failed release.
#
# Skipping is the right behaviour here: the tarball and .deb are the
# artifacts people actually install, and an AppImage that cannot be built is
# a missing convenience, not a broken release. A release that fails
# entirely, after producing every other platform's binaries, is worse.
if command -v appimagetool >/dev/null 2>&1 && ! command -v desktop-file-validate >/dev/null 2>&1; then
    echo "==> appimagetool found but desktop-file-validate is missing: skipping AppImage"
    echo "    (install desktop-file-utils to build one)"
elif command -v appimagetool >/dev/null 2>&1; then
    echo "==> AppImage"
    APPDIR="$OUT/AppDir"
    rm -rf "$APPDIR"
    mkdir -p "$APPDIR/usr/bin"
    # Both binaries when the daemon exists: it is what a user runs, the
    # harness is what they run when it does not work. On musl only the
    # harness was built, so AppRun falls back to it.
    cp "$OUT/outloud-spike" "$APPDIR/usr/bin/"
    if [ "$WANT_DAEMON" = 1 ]; then
        cp "$OUT/outloud" "$APPDIR/usr/bin/"
        APP_EXEC=outloud
    else
        APP_EXEC=outloud-spike
    fi
    cat > "$APPDIR/$APP_EXEC.desktop" <<DESKTOP
[Desktop Entry]
Name=OutLoud
Exec=$APP_EXEC
Icon=$APP_EXEC
Type=Application
Categories=Utility;Accessibility;
Terminal=true
DESKTOP
    # 1x1 placeholder icon; appimagetool requires one to exist.
    printf '\x89PNG\r\n\x1a\n' > "$APPDIR/$APP_EXEC.png" || true
    cat > "$APPDIR/AppRun" <<APPRUN
#!/bin/sh
# Launches the DAEMON where one was built, not the harness: this is the thing
# a user double-clicks expecting to dictate. The harness stays in usr/bin for
# diagnosis. musl builds have no daemon (no ALSA), so they launch the harness.
#
# AppRun decides nothing about Wayland/X11: the binary itself does runtime
# detection (WAYLAND_DISPLAY, then DISPLAY) so one AppImage serves both.
exec "\$(dirname "\$0")/usr/bin/$APP_EXEC" "\$@"
APPRUN
    chmod +x "$APPDIR/AppRun"
    # ARCH is read by appimagetool for the AppImage metadata.
    ARCH="$ARCH" appimagetool "$APPDIR" "$OUT/$APP_EXEC-$VERSION-$ARCH.AppImage"
else
    echo "==> appimagetool not found: skipping AppImage"
fi

# ---------------------------------------------------------------- .deb
if command -v dpkg-deb >/dev/null 2>&1; then
    echo "==> .deb"
    DEB="$OUT/deb"
    rm -rf "$DEB"
    mkdir -p "$DEB/usr/bin" "$DEB/DEBIAN" "$DEB/usr/share/doc/outloud"
    cp "$OUT/outloud-spike" "$DEB/usr/bin/"
    if [ "$WANT_DAEMON" = 1 ]; then
        cp "$OUT/outloud" "$DEB/usr/bin/"
        DEB_DESC=" Dictate and edit text by voice, entirely on this machine. Ships the\n daemon (outloud) and a diagnostic harness (outloud-spike)."
    else
        DEB_DESC=" Diagnostic harness only: this musl build links no audio backend, so\n it cannot dictate. See the glibc package for the daemon."
    fi
    cp LICENSE* "$DEB/usr/share/doc/outloud/" 2>/dev/null || true

    # Dependencies, READ OFF THE BINARIES rather than hardcoded.
    #
    # This field was absent until 2026-10-30, so every `.deb` this script
    # produced declared no dependencies whatsoever. apt therefore considered
    # it installable on any Debian or Ubuntu release, and the failure landed
    # at *runtime* as a dynamic-linker error naming a glibc version, which is
    # not something a user can act on. The floor is real and high: a
    # ubuntu-24.04 runner links `pidfd_spawnp@GLIBC_2.39`, so these binaries
    # do not run on Debian 12 (2.36) or Ubuntu 22.04 (2.35) at all.
    #
    # Derived, not written down, because a hardcoded floor silently becomes a
    # lie the next time the runner image moves, which is exactly how the
    # "glibc floor is 2.31" line in docs/build-and-release.md came to be
    # wrong. `strings` is used rather than readelf/objdump because binutils
    # is the one package guaranteed present on every runner here, and the
    # version tokens are plain symbol-table strings.
    DEB_DEPENDS=""
    case "$TARGET" in
        *-musl) : ;;  # static: no libc dependency to declare
        *)
            # Scan EVERY binary in the package, not just one. The daemon
            # links whisper.cpp and the harness does not, so they can
            # legitimately differ, and the package's floor is the highest of
            # the two. Measured equal (2.39) on both arches today, which is
            # precisely why taking the max rather than sampling one has to be
            # structural: the day they diverge, a sampled floor is too low
            # and the package installs onto a system it cannot run on.
            GLIBC_MAX="$(strings -a "$DEB"/usr/bin/* \
                | grep -oE 'GLIBC_2\.[0-9]+' \
                | sort -t. -k2 -n -u | tail -1 | sed 's/GLIBC_//')"
            if [ -z "${GLIBC_MAX:-}" ]; then
                echo "FATAL: could not read a GLIBC_2.x floor out of the binary." >&2
                echo "  A dynamically linked glibc build always references one," >&2
                echo "  so this means the probe broke, not that there is no" >&2
                echo "  floor. Refusing to ship a .deb with no Depends: that is" >&2
                echo "  the bug this block exists to fix." >&2
                exit 1
            fi
            echo "    glibc floor read from binary: $GLIBC_MAX"
            DEB_DEPENDS="libc6 (>= $GLIBC_MAX)"
            if [ "$WANT_DAEMON" = 1 ]; then
                # cpal links libasound at runtime. Alternation because the
                # 64-bit-time_t transition renamed the package: Ubuntu 24.04
                # and Debian 13 ship libasound2t64, older releases
                # libasound2. apt accepts the first that exists.
                DEB_DEPENDS="$DEB_DEPENDS, libasound2t64 | libasound2"
            fi
            ;;
    esac

    cat > "$DEB/DEBIAN/control" <<CONTROL
Package: outloud
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: OutLoud project
Section: utils
Priority: optional
${DEB_DEPENDS:+Depends: $DEB_DEPENDS}
Description: Local voice dictation and edit-by-voice
$(printf '%b' "$DEB_DESC")
CONTROL
    # An empty Depends expansion leaves a blank line, which dpkg-deb reads as
    # the end of the control stanza and then rejects the file for having no
    # Description. Harmless to run unconditionally.
    sed -i.bak '/^$/d' "$DEB/DEBIAN/control" && rm -f "$DEB/DEBIAN/control.bak"
    # The filename carries the libc, because the release flattens every
    # artifact into one directory and attaches them by basename. glibc and
    # musl x86_64 both produced `outloud_0.1.0_amd64.deb`, so whichever job
    # uploaded second would silently replace the other on the release page,
    # and the survivor is decided by matrix timing. A musl package winning
    # is the bad case: it contains no daemon, so a user would download
    # "outloud for amd64" and get a package that cannot dictate.
    #
    # Only the dynamically linked build is plain `_amd64.deb`, since that is
    # the one a normal user wants.
    case "$TARGET" in
        *-musl) DEB_NAME="outloud_${VERSION}_${DEB_ARCH}-musl.deb" ;;
        *)      DEB_NAME="outloud_${VERSION}_${DEB_ARCH}.deb" ;;
    esac
    dpkg-deb --build --root-owner-group "$DEB" "$OUT/$DEB_NAME"
else
    echo "==> dpkg-deb not found: skipping .deb"
fi

# ---------------------------------------------------------------- .rpm
#
# The spec below sets two globals that must not be explained inside it:
#
#   __os_install_post   rpm's automatic post-install pass. It runs brp-strip,
#                       which invokes the HOST's /usr/bin/strip on everything
#                       it packages. Cross-building aarch64 on an x86_64
#                       runner hands it a foreign ELF and it fails with
#                       "Unable to recognise the format of the input file",
#                       taking the whole job down after the binary had
#                       already built. The release profile strips already, so
#                       the pass has nothing to do even when it can read the
#                       file.
#   debug_package       debuginfo extraction, which shells out to the same
#                       host-arch tools for the same reason.
#
# Why the explanation lives HERE and not in the spec: rpm expands macros
# inside spec comments. A comment naming a macro is not a comment, it is a
# command, and mine turned into "Unknown tag: /usr/lib/rpm/brp-compress"
# which broke all four Linux jobs including the two that had been passing.
if command -v rpmbuild >/dev/null 2>&1; then
    echo "==> .rpm"
    RPMTOP="$OUT/rpmbuild"
    rm -rf "$RPMTOP"
    mkdir -p "$RPMTOP"/{SPECS,BUILD,RPMS,SOURCES}
    # Same basename-collision problem as the .deb above: glibc and musl
    # x86_64 both produced `outloud-0.1.0-1.x86_64.rpm`. Encoded in the
    # Release field rather than by renaming the file afterwards, so the
    # package's own metadata distinguishes the two and `rpm -q` tells the
    # truth, rather than only the filename doing so.
    case "$TARGET" in
        *-musl) RPM_RELEASE="1.musl" ;;
        *)      RPM_RELEASE="1" ;;
    esac
    if [ "$WANT_DAEMON" = 1 ]; then
        RPM_INSTALL="install -m 0755 $ROOT/$OUT/outloud %{buildroot}/usr/bin/outloud"
        RPM_FILES="/usr/bin/outloud"
        # alsa-lib is the RPM name for what Debian calls libasound2. Only
        # the daemon links it; see the .deb block for the full reasoning
        # about declared dependencies.
        RPM_REQUIRES="Requires: alsa-lib"
    else
        RPM_INSTALL=""
        RPM_FILES=""
        RPM_REQUIRES=""
    fi
    cat > "$RPMTOP/SPECS/outloud.spec" <<SPEC
Name: outloud
Version: $VERSION
Release: $RPM_RELEASE
Summary: Local voice dictation and edit-by-voice
License: MIT
$RPM_REQUIRES
%global __os_install_post %{nil}
%global debug_package %{nil}

%description
Dictate and edit text by voice, entirely on this machine.
%install
mkdir -p %{buildroot}/usr/bin
$RPM_INSTALL
install -m 0755 $ROOT/$OUT/outloud-spike %{buildroot}/usr/bin/outloud-spike
%files
$RPM_FILES
/usr/bin/outloud-spike
SPEC
    rpmbuild --define "_topdir $ROOT/$RPMTOP" \
             --define "_rpmdir $ROOT/$OUT" \
             --target "$ARCH" -bb "$RPMTOP/SPECS/outloud.spec"
    # rpmbuild writes to _rpmdir/<arch>/, NOT to _rpmdir. Release artifacts
    # are globbed as dist/linux/<target>/*.rpm, which never matched that
    # subdirectory, so no release has ever actually contained an RPM despite
    # the workflow listing one and docs/build-and-release.md promising one.
    # Nothing failed: the glob simply matched nothing, and
    # upload-artifact only warns.
    #
    # Moved rather than re-pointed because -bb ignores a _rpmdir without an
    # arch component in some rpm versions; moving the result is the behaviour
    # that does not depend on the rpm version.
    if [ -d "$OUT/$ARCH" ]; then
        find "$OUT/$ARCH" -name '*.rpm' -exec mv -f {} "$OUT/" \;
        rmdir "$OUT/$ARCH" 2>/dev/null || true
    fi
    # The whole point of this block is an artifact that reaches users, so
    # assert it exists rather than assuming the move worked.
    if ! ls "$OUT"/*.rpm >/dev/null 2>&1; then
        echo "FATAL: rpmbuild reported success but no .rpm is in $OUT." >&2
        echo "  The release globs $OUT/*.rpm; an RPM anywhere else ships" >&2
        echo "  to nobody. This guard exists because that silently happened" >&2
        echo "  for every release up to 2026-10-30." >&2
        exit 1
    fi
else
    echo "==> rpmbuild not found: skipping .rpm"
fi

# ---------------------------------------------------------------- Flatpak
#
# Emitted for ONE target only, because the manifest below is a quoted
# heredoc with nothing target-specific in it: all four Linux jobs produced
# byte-identical copies under the same name, which collided when the release
# flattened them. Harmless in content, but it made the duplicate-name guard
# in the publish job unable to distinguish "four copies of the same file"
# from "two different packages, one about to be lost".
#
# flatpak-builder resolves its own architecture, so one manifest is correct
# for every arch anyway.
if [ "$TARGET" = "x86_64-unknown-linux-gnu" ]; then
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
else
    echo "==> Flatpak manifest: emitted by the x86_64 glibc job only (arch-independent)"
fi

ls -l "$OUT"
