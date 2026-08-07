#!/usr/bin/env bash
# Exercise scripts/install.sh without touching the developer's /Applications.
#
# WHY: the previous installer broke in a way no check could see -- it lived
# only on a feature branch, and the published one-liner 404s the moment that
# branch is deleted. An installer is the first thing a stranger runs and the
# last thing anyone tests. This runs it for real: against the live release,
# into a temp directory, and asserts the app that lands is launchable.
#
# Network-dependent by nature. Skips rather than fails when GitHub is
# unreachable, so an offline machine does not report a defect that is not
# there.
#
# Usage: scripts/test-install.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/scripts/install.sh"

pass() { printf '  ok  %s\n' "$*"; }
fail() { printf '  FAIL %s\n' "$*" >&2; exit 1; }

[ -f "$INSTALLER" ] || fail "scripts/install.sh is missing -- this is the exact defect this file guards"

echo "==> syntax"
bash -n "$INSTALLER" || fail "install.sh does not parse"
pass "parses"

echo "==> refuses a platform it cannot serve"
out="$(bash -c 'uname() { if [ "${1:-}" = "-s" ]; then echo Linux; else command uname "$@"; fi; }
export -f uname
bash "$0"' "$INSTALLER" 2>&1 || true)"
case "$out" in
    *"macOS only"*) pass "declines non-macOS" ;;
    *) fail "expected a macOS-only refusal, got: $out" ;;
esac

out="$(bash -c 'uname() { if [ "${1:-}" = "-m" ]; then echo x86_64; else command uname "$@"; fi; }
export -f uname
bash "$0"' "$INSTALLER" 2>&1 || true)"
case "$out" in
    *"Apple Silicon only"*) pass "declines the wrong architecture" ;;
    *) fail "expected an architecture refusal, got: $out" ;;
esac

echo "==> fails loudly on an unwritable destination"
if OUTLOUD_INSTALL_DIR=/System/nope bash "$INSTALLER" >/tmp/install-test-unwritable.log 2>&1; then
    fail "returned success while installing nothing"
fi
pass "non-zero exit when it cannot install"

echo "==> no install path is pinned to a branch"
# The defect that started all of this: the published one-liner and the
# double-click installer both fetched from refs/heads/overlay/cat-mascot,
# and the script did not exist on main at all. Deleting a stale branch --
# routine housekeeping -- would have broken installation for everyone.
for f in "$ROOT/scripts/Install-OutLoud.command" "$ROOT/README.md"; do
    [ -f "$f" ] || continue
    if grep -q "raw.githubusercontent.com/[^\"]*refs/heads/" "$f"; then
        grep -n "refs/heads/" "$f" >&2
        fail "$(basename "$f") fetches from a branch ref; use /main/ so it survives that branch being deleted"
    fi
done
pass "install paths point at main, not a branch"

echo "==> the double-click installer is present and parses"
CMD="$ROOT/scripts/Install-OutLoud.command"
[ -f "$CMD" ] || fail "scripts/Install-OutLoud.command is missing; the release asset would have no source on main"
bash -n "$CMD" || fail "Install-OutLoud.command does not parse"
pass "Install-OutLoud.command parses"

echo "==> the publish job's steps behave as the workflow expects"
# The publish job is gated on a v* tag, so it has never once executed: every
# workflow_dispatch run skips it, and the last tagged run was cancelled at
# the compliance gate. Its steps are the ones that produce what strangers
# download, so they are exercised here rather than first discovered during
# a release.
"$ROOT/scripts/ci-check-publish-steps.py" || fail "the publish job's steps would not behave as intended"

# Everything past here needs the network and the real release.
if ! curl -fsSL --max-time 10 -o /tmp/install-test-api.json \
        https://api.github.com/repos/blarer/outloud/releases/latest 2>/dev/null; then
    echo "==> SKIP the live install: GitHub unreachable"
    exit 0
fi

echo "==> installs the real release into a temp directory"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

OUTLOUD_INSTALL_DIR="$WORK" bash "$INSTALLER" >"$WORK/install.log" 2>&1 \
    || { cat "$WORK/install.log"; fail "the install failed"; }
pass "installed"

APP="$WORK/OutLoud.app"
[ -d "$APP" ] || fail "no OutLoud.app in $WORK"

# The binary must be the product, not the M0 spike harness. This is the
# check that would have caught a release built by the CI macOS path, which
# packages spike-cli and cannot dictate at all.
ver="$("$APP/Contents/MacOS/OutLoud" --version 2>&1 || true)"
case "$ver" in
    outloud*) pass "the installed binary is outloud ($ver)" ;;
    *) fail "expected the outloud daemon, got: $ver" ;;
esac

# The Swift recognizer is a separate child process, not a linked library. An
# app without it comes up unable to transcribe anything, which looks like a
# permissions problem and wastes an afternoon.
[ -x "$APP/Contents/MacOS/outloud-speech-helper" ] \
    || fail "the speech helper is missing; the app would launch and never transcribe"
pass "the speech helper is bundled"

echo "==> replacing an existing install really replaces it"
touch "$APP/stale-marker"
OUTLOUD_INSTALL_DIR="$WORK" bash "$INSTALLER" >"$WORK/reinstall.log" 2>&1 \
    || { cat "$WORK/reinstall.log"; fail "the reinstall failed"; }
[ ! -e "$APP/stale-marker" ] \
    || fail "the old bundle was merged into rather than replaced"
pass "reinstall replaces the bundle"

echo
echo "==> install check OK"
