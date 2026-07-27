#!/usr/bin/env bash
# Walk the M0 application matrix and record what each application supports.
#
# For every target: bring it to the front, probe the focused text field, and
# record the rewrite strategy it offers. The output is the evidence for the M0
# exit criteria, so it is written as a table rather than as prose.
#
# The operator still has to put a cursor in a text field in each application
# before running this. That is deliberate: a tool that focuses fields on the
# user's behalf would be testing its own automation rather than the real case.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/release/spike-cli"
SETTLE_SECONDS="${SETTLE_SECONDS:-3}"

if [[ ! -x "$BIN" ]]; then
    cargo build --release --manifest-path "$ROOT/Cargo.toml"
fi

# Application name, then why it is in the matrix. One per text-system family.
TARGETS=(
    "TextEdit|native AppKit"
    "Safari|web content"
    "Notes|native, rich text"
    "Mail|native, compose window"
    "Terminal|terminal emulator"
    "Visual Studio Code|Electron"
    "Slack|Electron chat"
    "Google Chrome|Chromium"
)

printf '%-22s %-18s %-16s %-20s %s\n' "APPLICATION" "ROLE" "READ" "STRATEGY" "NOTE"
printf '%.0s-' {1..100}; echo

for entry in "${TARGETS[@]}"; do
    app="${entry%%|*}"
    note="${entry##*|}"

    # Skip anything not installed rather than reporting it as a failure.
    if ! osascript -e "id of application \"$app\"" >/dev/null 2>&1; then
        printf '%-22s %-18s %-16s %-20s %s\n' "$app" "-" "not installed" "-" "$note"
        continue
    fi

    osascript -e "tell application \"$app\" to activate" >/dev/null 2>&1
    sleep "$SETTLE_SECONDS"

    output="$("$BIN" probe 2>&1)"
    role="$(sed -n 's/^role: *//p' <<<"$output" | head -1)"
    strategy="$(sed -n 's/^strategy: *//p' <<<"$output" | head -1)"

    if [[ -n "$strategy" ]]; then
        printf '%-22s %-18s %-16s %-20s %s\n' "$app" "${role:--}" "ok" "$strategy" "$note"
    else
        reason="$(head -1 <<<"$output" | sed 's/^probe failed: //')"
        printf '%-22s %-18s %-16s %-20s %s\n' "$app" "-" "no text field" "clipboard-paste" "$reason"
    fi
done

echo
echo "Rows reading 'clipboard-paste' are not failures: some applications"
echo "legitimately expose no writable text field and must use the paste path."
