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

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy (warnings are errors)"
# --all-targets covers tests and benches, where lint rot usually hides.
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "==> cargo test"
cargo test --workspace --locked

echo "==> ci-check OK"
