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

# Check every shell script against the OLDEST bash it will meet.
#
# macOS ships bash 3.2 as /bin/bash (the last GPLv2 release, from 2007) and
# that is what a GitHub macos runner uses for `shell: bash`. A developer's
# Homebrew bash is 5.x, so bash-4+ syntax passes locally and fails only in
# CI. Not hypothetical: `${var@Q}` in ci-edit-routing.sh passed every local
# run and died on the runner with "bad substitution".
#
# Two checks, because they catch different things and I tried the weaker one
# first:
#
#   1. `bash -n` for syntax. Catches structure (an unclosed `if`), and is
#      BLIND to parameter expansions: `${x@Q}` parses fine under 3.2 and
#      only explodes when the line executes. Verified by trying it.
#   2. A grep for the specific bash-4+ constructs, which is what actually
#      catches the class. Running the scripts is not an option here: several
#      of them type into windows.
if [[ -x /bin/bash ]]; then
  bash32="$(/bin/bash --version | head -1 | sed 's/.*version //;s/(.*//')"
  echo "==> shell scripts work under bash $bash32 (what CI's macOS runners use)"
  bad=0
  for f in scripts/*.sh; do
    if ! /bin/bash -n "$f" 2>/tmp/bash32-parse.err; then
      echo "    FAIL $f (syntax)"
      sed 's/^/         /' /tmp/bash32-parse.err
      bad=1
    fi
    # ${x@Q}, ${x^^}, ${x,,}: all bash 4+, all invisible to `bash -n`.
    # `declare -A` too, which 3.2 rejects at runtime.
    # Skips this file: the pattern below necessarily CONTAINS the
    # constructs it looks for, and a checker that fails on its own source
    # is a checker nobody keeps.
    if [[ "$f" != "scripts/ci-check.sh" ]] && grep -nE '\$\{[A-Za-z_][A-Za-z0-9_]*@[QEPAaKkLUu]\}|\$\{[A-Za-z_][A-Za-z0-9_]*(\^\^|,,)|declare -A' "$f" >/tmp/bash32-feat.err 2>&1; then
      echo "    FAIL $f (bash 4+ only)"
      sed 's/^/         /' /tmp/bash32-feat.err
      bad=1
    fi
  done
  rm -f /tmp/bash32-parse.err /tmp/bash32-feat.err
  if [[ "$bad" != "0" ]]; then
    echo "FAIL: a script needs a newer bash than CI has." >&2
    echo "CI's macOS runners use /bin/bash 3.2, not your Homebrew bash." >&2
    exit 1
  fi
  echo "    all scripts OK for bash $bash32"
fi

echo "==> platform cfg stubs"
# Cheap, and catches the class that has broken CI twice: a
# cfg(not(target_os = "macos")) stub whose surface drifted from the real
# type, which a Mac-only `cargo check` cannot see.
scripts/ci-check-cfg.sh

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy (warnings are errors)"
# --all-targets covers tests and benches, where lint rot usually hides.
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "==> cargo test"
cargo test --workspace --locked

echo "==> ci-check OK"
