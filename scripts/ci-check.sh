#!/usr/bin/env bash
# CI: formatting, lints, and tests. One script so local runs and CI runs are
# the identical command (`scripts/ci-check.sh`), which eliminates the classic
# "passes locally, fails in CI" divergence caused by flag drift.
#
# Failure modes this guards:
#   - rustfmt drift making every later diff noisy        -> fmt --check
#   - warnings accumulating until nobody reads them      -> clippy -D warnings
#   - lockfile silently drifting from Cargo.toml         -> --locked everywhere

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# System dependencies before anything that compiles. cpal's alsa-sys runs
# pkg-config in a build script, so even `cargo clippy` fails on a Linux box
# without libasound2-dev, with an error about a package no lint touches.
# Self-provisioning here rather than in workflow YAML means a green local run
# and a green CI run stay the same claim, which is this script's whole reason
# for existing. It is a no-op on macOS/Windows, when the packages are already
# present, or when it cannot elevate.
scripts/ci-install-linux-deps.sh

echo "==> workflow YAML"
# Cheapest check, so it runs first: a workflow that does not parse produces a
# 0-second run with no jobs and no line number, which is far more expensive to
# diagnose from the GitHub UI than from here.
scripts/ci-validate-workflows.sh

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy (warnings are errors)"
# --all-targets covers tests and benches, where lint rot usually hides.
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "==> cargo test"
cargo test --workspace --locked

echo "==> ci-check OK"
