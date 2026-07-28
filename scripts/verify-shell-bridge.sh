#!/usr/bin/env bash
# Non-interactive verification of the shell bridge, for the coordinator to run.
#
# The demo script is interactive by design, which is right for a human but
# useless as evidence. This drives the same path through expect and asserts on
# the resulting command line, so a claim that the shell rewrite works is
# backed by an observation rather than by a report.
#
# Everything lives in a temp ZDOTDIR: the caller's real ~/.zshrc is never read
# or written.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

cargo build -p shell-bridge >/dev/null 2>&1 || cargo build -p shell-bridge
bridge="$root/target/debug/shell-bridge"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

zdot="$work/zdot"
sockdir="$work/run"
mkdir -p "$zdot" "$sockdir"
socket="$sockdir/shell.sock"

cp "$root/shell/aqua.zsh" "$zdot/aqua.zsh"
{
    echo 'PS1="aqua-demo% "'
    echo "export AQUA_BRIDGE_SOCKET=$socket"
    echo "source $zdot/aqua.zsh"
} > "$zdot/.zshrc"

if ! command -v expect >/dev/null 2>&1; then
    echo "SKIP: expect not installed, cannot drive a real pty"
    exit 0
fi

# The expect script assumes a bridge is already listening, so start one here
# and wait for the socket to appear rather than sleeping a guess.
"$bridge" serve --socket "$socket" &
bridge_pid=$!
cleanup() {
    kill "$bridge_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

for _ in $(seq 50); do
    [ -S "$socket" ] && break
    sleep 0.1
done
[ -S "$socket" ] || { echo "FAIL: bridge never created its socket"; exit 1; }

expect "$root/shell/verify-zsh.exp" "$bridge" "$socket" "$zdot"
