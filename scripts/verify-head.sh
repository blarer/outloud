#!/usr/bin/env bash
# Build a fresh clone of committed HEAD, not the working tree.
#
# Why this exists: several agents share one dirty checkout, and `git add` on a
# shared file can silently adopt half of someone else's in-flight refactor.
# When that happens, HEAD carries one half of a two-file change while EVERY
# local tree still compiles, because every local tree has both halves. The
# tree lies to everyone who has it, and only a stranger cloning fresh sees the
# truth.
#
# That is not hypothetical. It happened here: a `spawn_mic -> Result` change
# sat uncommitted in source.rs while the matching `?` at the call site was
# committed, and HEAD failed with
#
#     error[E0277]: the `?` operator can only be applied to values that
#     implement `Try` --> crates/aquad/src/main.rs:286
#
# while `cargo test` passed for every person who ran it locally.
#
# Run this before declaring anything done, and especially after a large
# mechanical change such as a rename. It is the only check that sees what a
# new contributor, or a release build, would see.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/aqua-verify-head.XXXXXX")"
# Always clean up the clone, including on failure: a stale multi-hundred-MB
# target directory per invocation would be a nasty surprise.
trap 'rm -rf "$WORK"' EXIT

REF="${1:-HEAD}"

echo "==> Cloning $REF into a scratch directory"
# --no-hardlinks so the clone cannot share objects with, and therefore cannot
# be perturbed by, the working repository.
git clone --quiet --no-hardlinks "$ROOT" "$WORK/repo"
git -C "$WORK/repo" checkout --quiet "$REF"

echo "    $(git -C "$WORK/repo" log --oneline -1)"

cd "$WORK/repo"

# Uncommitted files are invisible here by construction, which is the point.
# Report what the working repo is still holding, so a failure below has an
# obvious first suspect.
DIRTY="$(git -C "$ROOT" status --porcelain | wc -l | tr -d ' ')"
if [[ "$DIRTY" != "0" ]]; then
    echo "    note: the source repo has $DIRTY uncommitted path(s), none of which are tested here"
fi

echo "==> cargo fmt --check"
cargo fmt --check

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --quiet --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --quiet --workspace

# The headless configuration is a separate feature resolution, so it can break
# on its own. It is also the one a Linux or server build uses, and the one
# least likely to be exercised by anyone working on macOS.
echo "==> cargo check --workspace --no-default-features"
cargo check --quiet --workspace --no-default-features

echo
echo "HEAD is buildable from a fresh clone."
