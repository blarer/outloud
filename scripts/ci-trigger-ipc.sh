#!/usr/bin/env bash
# Prove the Linux hotkey trigger CLI can talk to a SEPARATELY RUNNING daemon.
#
# `outloud trigger press|release` is the whole Linux hotkey contract: it is
# literally what the compositor execs on a keypress (see docs/hotkeys.md).
# The unit tests in crates/hotkey/src/backend/linux.rs drive that socket
# in-process, which cannot catch the failure that actually matters here --
# a trigger binary that cannot find, or cannot speak to, another process's
# socket. That failure looks to a user like a dead microphone.
#
# Two processes, one socket. No compositor, no microphone, no GPU.
#
# Run it the same way CI does:
#
#   scripts/ci-trigger-ipc.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

WORK="$(mktemp -d)"
cleanup() {
    [[ -n "${daemon:-}" ]] && kill "$daemon" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

cargo build --locked --bin outloud

export OUTLOUD_HOTKEY_TRIGGER_SOCKET="$WORK/run/outloud.sock"
mkdir -p "$WORK/run"

# 1. With nothing listening, the CLI must FAIL and name the daemon.
#
# A trigger that exits 0 into the void is precisely the bug that makes a
# dead hotkey indistinguishable from a dead microphone, and the compositor's
# exec log is the only place a user would ever see the message.
echo "==> trigger with no daemon must fail loudly"
if ./target/debug/outloud trigger ping 2>"$WORK/ping-err.txt"; then
    echo "ci-trigger-ipc: trigger ping succeeded with no daemon listening" >&2
    exit 1
fi
if ! grep -qi outloud "$WORK/ping-err.txt"; then
    echo "ci-trigger-ipc: the failure does not name the daemon, so a compositor log would be useless:" >&2
    cat "$WORK/ping-err.txt" >&2
    exit 1
fi
echo "    names the problem: $(cat "$WORK/ping-err.txt")"

# 2. With the daemon up, press/release must reach it.
#
# --asr mock because this asserts the SOCKET contract, not transcription
# (scripts/ci-whisper.sh covers that) and a runner has no microphone.
# Display variables unset so a missing compositor cannot be what makes it
# pass or fail.
echo "==> starting the daemon"
env -u DISPLAY -u WAYLAND_DISPLAY \
    ./target/debug/outloud --asr mock --no-overlay >"$WORK/daemon.log" 2>&1 &
daemon=$!

for _ in $(seq 1 50); do
    [[ -S "$OUTLOUD_HOTKEY_TRIGGER_SOCKET" ]] && break
    # A daemon that died early must not be waited on for ten seconds.
    if ! kill -0 "$daemon" 2>/dev/null; then
        echo "ci-trigger-ipc: the daemon exited before binding its socket:" >&2
        cat "$WORK/daemon.log" >&2
        exit 1
    fi
    sleep 0.2
done

if [[ ! -S "$OUTLOUD_HOTKEY_TRIGGER_SOCKET" ]]; then
    echo "ci-trigger-ipc: the daemon never created its trigger socket:" >&2
    cat "$WORK/daemon.log" >&2
    exit 1
fi

echo "==> ping / press / release must all reach it"
./target/debug/outloud trigger ping
./target/debug/outloud trigger press
./target/debug/outloud trigger release

echo "==> ok: the compositor's exec path works across processes"
