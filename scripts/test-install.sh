#!/usr/bin/env bash
# Exercise scripts/install.sh end to end against a local file server.
#
# WHY: the installer's first real run would otherwise be on the machine of the
# person least able to diagnose it, over a link the developer cannot see. Every
# check here corresponds to a way that run could fail silently or destructively.
#
# The pgrep name bug was found this way: the installer looked for a process
# named `outloud` while the bundled executable is `OutLoud`, so the "stop the
# running copy" step never fired and an update would have appeared to do
# nothing until a reboot.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/scripts/install.sh"
ASSET="OutLoud-macos-arm64.tar.gz"

pass=0
fail=0
check() {
    local what="$1"
    shift
    if "$@"; then
        printf '  PASS  %s\n' "$what"
        pass=$((pass + 1))
    else
        printf '  FAIL  %s\n' "$what"
        fail=$((fail + 1))
    fi
}

[[ -f "$ROOT/dist/$ASSET" ]] || {
    echo "no dist/$ASSET; run scripts/release-macos.sh --dry-run first" >&2
    exit 1
}

work="$(mktemp -d)"
cleanup() {
    [[ -n "${server_pid:-}" ]] && kill "$server_pid" 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work/served" "$work/Apps" "$work/bin"
cp "$ROOT/dist/$ASSET" "$work/served/"
# A deliberately corrupt archive: a download that dies partway is the most
# likely real-world failure, and it must not destroy a working install.
head -c 40000 "$ROOT/dist/$ASSET" >"$work/served/truncated.tar.gz"

# A port unlikely to collide, but verified below rather than assumed.
port=8918
(cd "$work/served" && exec python3 -m http.server "$port" >/dev/null 2>&1) &
server_pid=$!
for _ in $(seq 1 40); do
    curl -fsS -o /dev/null "http://127.0.0.1:$port/$ASSET" 2>/dev/null && break
    sleep 0.1
done
curl -fsS -o /dev/null "http://127.0.0.1:$port/$ASSET" || {
    echo "the local file server did not come up on $port" >&2
    exit 1
}

# A stub sw_vers, so the version guard can be driven to both sides of its
# boundary on a machine that already satisfies it.
cat >"$work/bin/sw_vers" <<'STUB'
#!/bin/bash
if [[ "$1" == "-productVersion" ]]; then
    echo "${FAKE_MACOS_VERSION:-26.0}"
else
    /usr/bin/sw_vers "$@"
fi
STUB
chmod +x "$work/bin/sw_vers"

run_installer() {
    OUTLOUD_INSTALL_DIR="$work/Apps" \
    OUTLOUD_INSTALL_URL="http://127.0.0.1:$port/$1" \
    PATH="$work/bin:$PATH" \
    FAKE_MACOS_VERSION="${FAKE_MACOS_VERSION:-26.0}" \
        bash "$INSTALLER" 2>&1
}

echo "==> installer end to end"

# --- The happy path, and what it must leave behind ------------------------
out="$(run_installer "$ASSET" || true)"
check "a clean install reports success" \
    grep -q "Installed" <<<"$out"
check "the app lands in the install directory" \
    test -d "$work/Apps/OutLoud.app"
check "the executable is present and runnable" \
    test -x "$work/Apps/OutLoud.app/Contents/MacOS/OutLoud"
# Without the helper the app installs, runs, and never transcribes a word.
check "the speech helper survives the round trip" \
    test -x "$work/Apps/OutLoud.app/Contents/MacOS/outloud-speech-helper"
# The whole reason this is a curl installer rather than a .dmg.
check "the installed copy carries no quarantine flag" \
    bash -c "! xattr -p com.apple.quarantine '$work/Apps/OutLoud.app' 2>/dev/null"
# An ad-hoc signature that did not survive archiving would fail only for the
# user, with "damaged and can't be opened" and no way to tell why.
check "the signature still verifies after install" \
    codesign --verify --deep --strict "$work/Apps/OutLoud.app"

# --- Failure paths must not destroy a working install ---------------------
before="$(codesign -dv "$work/Apps/OutLoud.app" 2>&1 | grep -c . || true)"

out="$(run_installer "does-not-exist.tar.gz" || true)"
check "a missing release is reported, not ignored" \
    grep -q "Could not download" <<<"$out"
check "a failed download leaves the existing install in place" \
    test -x "$work/Apps/OutLoud.app/Contents/MacOS/OutLoud"

out="$(run_installer "truncated.tar.gz" || true)"
check "a truncated download is caught" \
    grep -q "download was incomplete" <<<"$out"
check "a truncated download leaves the existing install in place" \
    test -x "$work/Apps/OutLoud.app/Contents/MacOS/OutLoud"
check "the surviving install still verifies" \
    codesign --verify --deep --strict "$work/Apps/OutLoud.app"

# --- The quarantine strip, against an archive that actually carries one ---
#
# The happy-path check above passes trivially: curl sets no quarantine flag, so
# nothing was ever there to strip, and the `xattr -dr` line could be deleted
# without failing a single assertion. That makes it a check that vouches for
# code it never runs.
#
# tar preserves the attribute, so an archive built from a quarantined bundle
# reproduces the real case: a user who already has a downloaded copy, or who
# re-runs the installer over one that arrived by AirDrop.
quarantined="$work/quarantined"
mkdir -p "$quarantined"
cp -R "$work/Apps/OutLoud.app" "$quarantined/"
xattr -w com.apple.quarantine "0081;00000000;Safari;" "$quarantined/OutLoud.app"
tar -czf "$work/served/quarantined.tar.gz" -C "$quarantined" OutLoud.app
check "the fixture really is quarantined (else the next check proves nothing)" \
    bash -c "tar -xzf '$work/served/quarantined.tar.gz' -C '$work' \
             && xattr -p com.apple.quarantine '$work/OutLoud.app' >/dev/null 2>&1"
rm -rf "$work/OutLoud.app"

out="$(run_installer "quarantined.tar.gz" || true)"
check "a quarantined archive still installs" \
    test -x "$work/Apps/OutLoud.app/Contents/MacOS/OutLoud"
# Without the strip, macOS refuses to open an ad-hoc bundle with
# "OutLoud is damaged and can't be opened", which has no override button.
check "the quarantine flag is stripped on install" \
    bash -c "! xattr -p com.apple.quarantine '$work/Apps/OutLoud.app' 2>/dev/null"

# --- The version guard, on both sides of its boundary ---------------------
# Below the boundary the app would install, launch, show its icon, and never
# transcribe: SpeechTranscriber is simply absent. Refusing early is the only
# place that failure can be explained.
out="$(FAKE_MACOS_VERSION=25.9 run_installer "$ASSET" || true)"
check "macOS 25 is refused" \
    grep -q "needs macOS 26 or newer" <<<"$out"
check "the refusal names the version the machine actually has" \
    grep -q "25.9" <<<"$out"

out="$(FAKE_MACOS_VERSION=26.0 run_installer "$ASSET" || true)"
check "macOS 26.0 is accepted (the boundary is not off by one)" \
    grep -q "Installed" <<<"$out"

# --- The URL a human is actually told to paste -----------------------------
#
# Everything above tests the installer's behaviour once it is running. None of
# it tests whether the one-liner reaches it. The advertised URL points at a
# branch, so an installer that lives only on a feature branch 404s for the
# person pasting the command, and the failure is a bare "404" with no mention
# of OutLoud at all.
#
# Found exactly that way: the one-liner named main/scripts/install.sh while the
# script existed only on the cat branch.
#
# Skipped without network. An offline developer should still get the rest.
advertised="$(grep -o 'https://raw.githubusercontent.com/[^ ]*install.sh' "$INSTALLER" | head -1)"
check "the installer advertises a URL at all" \
    test -n "$advertised"

if curl -fsS -o /dev/null --max-time 10 https://raw.githubusercontent.com 2>/dev/null \
    || [[ "${OUTLOUD_REQUIRE_NETWORK:-0}" == "1" ]]; then
    check "the advertised one-liner URL resolves ($advertised)" \
        curl -fsS -o /dev/null --max-time 20 "$advertised"
else
    echo "  SKIP  the advertised one-liner URL (no network)"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
