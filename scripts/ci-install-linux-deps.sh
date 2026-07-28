#!/usr/bin/env bash
# System packages required to build the workspace on Linux.
#
# One script instead of inline apt-get lines scattered across jobs, so every
# Linux CI job (check, msrv, repro, build-matrix, release) installs the same
# set and a new native dependency is added in exactly one place.
#
# Current requirements:
#   - libasound2-dev: alsa-sys (via cpal, the audio capture backend) runs
#     pkg-config for `alsa` in its build script. Without the -dev package the
#     build fails with "Package alsa was not found in the pkg-config search
#     path" on every glibc Linux build, including check/clippy runs that
#     never execute audio code.
#   - pkg-config: preinstalled on GitHub runners, listed anyway so the script
#     also works on a fresh container/minimal image.
#
# NOTE: this covers native glibc builds only. musl and cross-compiled targets
# cannot use it: their ALSA would have to be the TARGET architecture's. Those
# targets instead build with `--no-default-features`, which drops the audio
# capture backend entirely (see crates/audio's `capture` feature). A static
# headless daemon has no business linking an audio stack it never calls.
#
# WHY this is safe to call from other scripts rather than only from workflow
# YAML: it is a no-op everywhere it is not needed. It returns immediately on
# non-Linux, when the packages are already present, or when it cannot elevate.
# That property is what lets scripts/ci-check.sh and scripts/build-repro.sh
# self-provision, so a Linux CI job does not depend on someone remembering to
# add an install step to the workflow.

set -euo pipefail

# Not Linux: nothing to do. Callers are cross-platform.
if [ "$(uname -s)" != "Linux" ]; then
    exit 0
fi

# Already satisfied: skip the apt round trip entirely. Keeps repeated calls
# (ci-check.sh AND build-repro.sh in the same job) close to free.
if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists alsa 2>/dev/null; then
    echo "==> Linux system dependencies already present"
    exit 0
fi

# apt-get is Debian/Ubuntu only. On any other distro, say what is missing and
# let the build produce the real error rather than failing here with a
# confusing one about a package manager the user does not have.
if ! command -v apt-get >/dev/null 2>&1; then
    echo "==> no apt-get: install ALSA development headers with your package" >&2
    echo "    manager (alsa-lib-devel / alsa-lib / libasound2-dev) if the" >&2
    echo "    build fails on alsa-sys." >&2
    exit 0
fi

# Root in a container, sudo on a runner, neither on a locked-down box. The
# last case must not be fatal: a developer without sudo should still get the
# build's own error, not a permission failure from a helper script.
if [ "$(id -u)" = 0 ]; then
    SUDO=""
elif command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
else
    echo "==> cannot elevate to install ALSA headers; continuing anyway." >&2
    echo "    If the build fails on alsa-sys, install libasound2-dev." >&2
    exit 0
fi

export DEBIAN_FRONTEND=noninteractive

echo "==> installing Linux system dependencies (libasound2-dev, pkg-config)"
$SUDO apt-get update -qq
$SUDO apt-get install -y --no-install-recommends \
    libasound2-dev \
    pkg-config

echo "==> Linux system dependencies installed"
