#!/usr/bin/env bash
# CI: system packages required to build the workspace on Linux.
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
# NOTE: this covers native glibc builds only. musl and cross-compiled aarch64
# targets need the target-arch ALSA libraries (see Cross.toml pre-build hooks
# and docs/build-and-release.md#linux-system-dependencies).

set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
    libasound2-dev \
    pkg-config

echo "==> Linux system dependencies installed"
