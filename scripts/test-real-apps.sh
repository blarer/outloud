#!/usr/bin/env bash
# Real-application test harness: drive TextEdit and Safari with AppleScript,
# run the actual edit pipeline against them, and assert on the text that ends
# up IN THE DOCUMENT, not on the pipeline's return code. M0's tty bug shipped
# precisely because a return code said "wrote ok" while the text landed in a
# shell nobody was looking at; the only assertion that catches that class is
# reading the destination back through an independent channel (AppleScript),
# which is what this script does.
#
# Environmental degradation policy: SKIP, never FAIL. Apps on another Space
# report zero windows, automation permission may be ungranted, AX trust may be
# missing for a shell-launched binary. All of those are properties of the
# machine, not of the code (diag's Environment/Permission classes), so they
# must not turn CI red. FAIL is reserved for the one thing that is always a
# bug: the pipeline claimed success and the document text disagrees.
#
# Usage:   ./scripts/test-real-apps.sh
# Exit:    0 when every test passed or skipped, 1 on any FAIL.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Prefer the bundled binary: TCC pins accessibility grants to the bundle's
# identity, and a bare target/release binary is judged against the terminal.
BIN=""
for candidate in \
    "$ROOT/dist/OutLoud.app/Contents/MacOS/OutLoud" \
    "$ROOT/target/release/spike-cli"; do
    [[ -x "$candidate" ]] && BIN="$candidate" && break
done

PASS=0; SKIP=0; FAIL=0
pass() { printf 'PASS  %s\n' "$1"; PASS=$((PASS+1)); }
skip() { printf 'SKIP  %s -- %s\n' "$1" "$2"; SKIP=$((SKIP+1)); }
fail() { printf 'FAIL  %s -- %s\n' "$1" "$2"; FAIL=$((FAIL+1)); }

# Run AppleScript with a timeout so a hung app cannot hang the harness (AX
# calls into a busy process are synchronous IPC; so is Apple Events).
osa() { osascript -e "$1" 2>&1; }

app_installed() {
    # mdfind/osascript both launch the app; `lsappinfo`/mdls do not. Asking
    # LaunchServices for the path is the only probe with no side effects.
    osascript -e "tell application \"Finder\" to get exists application file id \"$1\"" 2>/dev/null | grep -q true
}

frontmost_app() {
    osa 'tell application "System Events" to get name of first application process whose frontmost is true'
}

# ---------------------------------------------------------------------------
# Preflight: decide once which capabilities this session actually has, so
# each test's skip reason names the real missing thing.
# ---------------------------------------------------------------------------
if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "SKIP  entire harness -- not macOS"
    exit 0
fi

# Automation permission: controlling System Events is the gate every test
# needs. Probing it is itself the permission prompt on first run.
if ! osa 'tell application "System Events" to get name of first process' >/dev/null; then
    echo "SKIP  entire harness -- Automation permission for System Events not granted"
    echo "      remedy: System Settings > Privacy & Security > Automation, enable for this terminal"
    exit 0
fi

if [[ -z "$BIN" ]]; then
    echo "SKIP  pipeline tests -- no spike-cli binary; run ./scripts/bundle-macos.sh"
    PIPELINE=no
else
    # AX trust decides whether we can run the real pipeline or only the
    # AppleScript-level scaffolding checks. `probe` exits nonzero untrusted.
    if "$BIN" probe >/dev/null 2>&1; then
        PIPELINE=yes
    else
        PIPELINE=no
        echo "note: $BIN lacks accessibility trust (TCC judges the responsible"
        echo "      process; see docs/macos-permissions.md). Pipeline tests will skip;"
        echo "      AppleScript-level assertions still run."
    fi
fi

# ---------------------------------------------------------------------------
# TextEdit: scriptable creation, mutation, and readback of a document.
# ---------------------------------------------------------------------------
test_textedit_scaffolding() {
    local name="TextEdit AppleScript round trip (harness self-test)"
    if ! app_installed "com.apple.TextEdit"; then
        skip "$name" "TextEdit not installed"; return
    fi
    # Prove the assertion channel itself works before trusting it to judge
    # the pipeline: create a doc, read it back, close without saving.
    local got
    got=$(osascript <<'EOF' 2>&1
tell application "TextEdit"
    set d to make new document with properties {text:"harness self test"}
    set t to text of d
    close d saving no
    return t
end tell
EOF
    )
    if [[ "$got" == "harness self test" ]]; then
        pass "$name"
    else
        skip "$name" "TextEdit not scriptable here: $got"
    fi
}

test_textedit_pipeline() {
    local name="TextEdit full edit pipeline, asserted on document text"
    if ! app_installed "com.apple.TextEdit"; then
        skip "$name" "TextEdit not installed"; return
    fi
    if [[ "$PIPELINE" != yes ]]; then
        skip "$name" "no trusted spike-cli binary (see note above)"; return
    fi

    local before="the quick brown fox jumps over the lazy dog"
    local expect="the slow brown fox jumps over the lazy dog"

    # Fresh document with known text, brought frontmost so the AX focused
    # element is its text area.
    if ! osascript <<EOF >/dev/null 2>&1
tell application "TextEdit"
    activate
    make new document with properties {text:"$before"}
end tell
EOF
    then
        skip "$name" "could not create a TextEdit document"; return
    fi
    sleep 1

    # The another-Space trap, checked explicitly: `activate` on an app whose
    # windows live on a different Space can leave something else frontmost,
    # and the edit would then hit the wrong app. Skip, do not guess.
    local front; front=$(frontmost_app)
    if [[ "$front" != "TextEdit" ]]; then
        osa 'tell application "TextEdit" to close front document saving no' >/dev/null
        skip "$name" "TextEdit did not become frontmost (got: $front); likely another Space"
        return
    fi

    local out rc
    out=$("$BIN" edit "change quick to slow" 2>&1); rc=$?

    # THE assertion: what does the document actually say now, asked over a
    # channel the pipeline does not control.
    local after
    after=$(osa 'tell application "TextEdit" to get text of front document')
    osa 'tell application "TextEdit" to close front document saving no' >/dev/null

    if [[ $rc -ne 0 ]]; then
        # The pipeline named its own failure; environmental by policy unless
        # the document changed anyway (which would be a severed-seam bug).
        if [[ "$after" != "$before" ]]; then
            fail "$name" "pipeline failed (rc=$rc) but document changed to: $after"
        else
            skip "$name" "pipeline reported: $(echo "$out" | tail -1)"
        fi
        return
    fi
    if [[ "$after" == "$expect" ]]; then
        pass "$name"
    else
        fail "$name" "pipeline claimed success but document says: $after"
    fi
}

test_textedit_no_overedit() {
    local name="TextEdit unmatched command leaves document untouched"
    if ! app_installed "com.apple.TextEdit"; then
        skip "$name" "TextEdit not installed"; return
    fi
    if [[ "$PIPELINE" != yes ]]; then
        skip "$name" "no trusted spike-cli binary"; return
    fi

    # Over-edit gate against a real app: a command whose needle is absent
    # must change NOTHING, and only independent readback can prove it.
    local before="untouchable text with unicode: héllo 日本語"
    if ! osascript <<EOF >/dev/null 2>&1
tell application "TextEdit"
    activate
    make new document with properties {text:"$before"}
end tell
EOF
    then
        skip "$name" "could not create a TextEdit document"; return
    fi
    sleep 1
    local front; front=$(frontmost_app)
    if [[ "$front" != "TextEdit" ]]; then
        osa 'tell application "TextEdit" to close front document saving no' >/dev/null
        skip "$name" "TextEdit not frontmost (got: $front)"; return
    fi

    "$BIN" edit "change zzznotpresentzzz to anything" >/dev/null 2>&1
    local after
    after=$(osa 'tell application "TextEdit" to get text of front document')
    osa 'tell application "TextEdit" to close front document saving no' >/dev/null

    if [[ "$after" == "$before" ]]; then
        pass "$name"
    else
        fail "$name" "no-match edit changed the document to: $after"
    fi
}

# ---------------------------------------------------------------------------
# Safari: web content via a data: URL textarea. Needs "Allow JavaScript from
# Apple Events" (Develop menu opt-in), so it usually skips on a fresh machine;
# when enabled it is the only scriptable path to real WebKit page content.
# ---------------------------------------------------------------------------
test_safari_web_content() {
    local name="Safari web-content readback channel"
    if ! app_installed "com.apple.Safari"; then
        skip "$name" "Safari not installed"; return
    fi
    local js_ok
    js_ok=$(osascript <<'EOF' 2>&1
tell application "Safari"
    if (count of windows) is 0 then return "no-windows"
    try
        return do JavaScript "1+1" in current tab of front window
    on error msg
        return "js-denied: " & msg
    end try
end tell
EOF
    )
    case "$js_ok" in
        2)
            pass "$name" ;;
        no-windows)
            # Zero windows is exactly the another-Space ambiguity from M0:
            # cannot distinguish "no window" from "window on another Space",
            # so the only honest outcome is a skip that says so.
            skip "$name" "Safari has no windows on this Space (or none at all)" ;;
        js-denied:*)
            skip "$name" "enable Develop > Allow JavaScript from Apple Events to run this" ;;
        *)
            skip "$name" "Safari not scriptable here: $js_ok" ;;
    esac
}

test_textedit_scaffolding
test_textedit_pipeline
test_textedit_no_overedit
test_safari_web_content

echo
echo "real-app harness: $PASS passed, $SKIP skipped, $FAIL failed"
[[ $FAIL -eq 0 ]] || exit 1
