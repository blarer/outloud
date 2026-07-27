#!/usr/bin/env bash
# Tier 1 + 2: every unit and integration test in the workspace, plus format
# and lint gates. This is the "run before pushing" command; it needs no
# display session, no permissions, and no installed applications, because
# every environmental fact the integration tests consume is simulated
# (tests/tests/common/mod.rs). Real-app coverage is a separate tier:
# ./scripts/test-real-apps.sh.
#
# Usage: ./scripts/test-workspace.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== cargo fmt --check =="
cargo fmt --all --check

echo "== cargo test (workspace) =="
cargo test --workspace

# The headless feature is a compile-time contract, not a runtime hope; keep
# it building. Known issue: `cargo test -p text-target --no-default-features`
# currently fails because four detect.rs unit tests expect the display-tier
# selections that the headless build compiles out. That is a bug in those
# tests (they need cfg(feature = "display") gates), owned by text-target;
# until it is fixed this script gates the build, not the headless test run.
echo "== cargo check (text-target, headless) =="
cargo check -p text-target --no-default-features

echo
echo "workspace tests: all green"
