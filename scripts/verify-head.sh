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
#     implement `Try` --> crates/outloud/src/main.rs:286
#
# while `cargo test` passed for every person who ran it locally.
#
# Run this before declaring anything done, and especially after a large
# mechanical change such as a rename. It is the only check that sees what a
# new contributor, or a release build, would see.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/outloud-verify-head.XXXXXX")"
# Always clean up the clone, including on failure: a stale multi-gigabyte
# target directory per invocation would fill the disk within a few runs.
trap 'rm -rf "$WORK"' EXIT

REF="${1:-HEAD}"

# A from-scratch build of this workspace needs roughly 8GB for target/ alone.
# Checking first turns "error: No space left on device" spraying out of six
# parallel rustc jobs into one sentence naming the real problem, which matters
# because that error reads like a compile failure and sends people debugging
# the wrong thing entirely.
AVAIL_KB="$(df -k "$WORK" | awk 'NR==2 {print $4}')"
if [[ "$AVAIL_KB" -lt $((8 * 1024 * 1024)) ]]; then
    echo "not enough disk to verify HEAD: $((AVAIL_KB / 1024 / 1024))GB free, need ~8GB" >&2
    echo "a fresh clone builds its own target/ from nothing; free some space first" >&2
    echo "(\`cargo clean\` in the working repo usually recovers several GB)" >&2
    exit 2
fi

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

# A crate directory that is not a workspace member compiles for nobody and
# tests for nobody, so anything inside it is dead weight that still looks
# present. That happened here mid-rename: `crates/outloud` existed while the
# members list still said `crates/aquad`, so a test living in outloud was never
# built and a whole class of check silently did nothing.
echo "==> every crate directory is a workspace member"
ORPHANS=""
for dir in crates/*/; do
    name="$(basename "$dir")"
    # A directory without a manifest is not a crate; ignore it.
    [[ -f "$dir/Cargo.toml" ]] || continue
    if ! grep -q "crates/$name\"" Cargo.toml; then
        ORPHANS="$ORPHANS $name"
    fi
done
if [[ -n "$ORPHANS" ]]; then
    echo "    orphaned crate(s), present but not in workspace members:$ORPHANS" >&2
    echo "    Nothing in them is compiled or tested. Usually a half-finished" >&2
    echo "    rename: add them to members in Cargo.toml, or delete them." >&2
    exit 1
fi

echo
echo "HEAD is buildable from a fresh clone."
