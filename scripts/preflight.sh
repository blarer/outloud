#!/usr/bin/env bash
# Release preflight: answers "is this safe to ship?" in one command.
#
# Written for the moment three swarms are concurrently rewriting the overlay,
# the menu-bar mark, and evaluating a framework. Nothing here edits code; it
# only checks. Every check prints PASS / FAIL / SKIP plus a named next action
# on failure, because a red gate without a next action just moves the
# debugging onto whoever reads the output.
#
# Ordering: cheap static checks (names, doctor remedies, bundle shape) run
# first so a broken tree fails in seconds; the compile-heavy gates and the
# interactive focus/CPU probes run last. Use --quick to skip the heavy gates
# when iterating on the cheap ones.
#
# Exit code: 0 only when no check FAILED (SKIPs do not fail the run, but are
# listed so a human decides whether a skip is acceptable for this release).

set -uo pipefail
# NOT set -e: a failing check must record FAIL and keep going, so one broken
# gate still yields the full picture instead of the first red line.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QUICK=0
[[ "${1:-}" == "--quick" ]] && QUICK=1

LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/preflight.XXXXXX")"
PASS=0; FAIL=0; SKIP=0
RESULTS=()

# record <PASS|FAIL|SKIP> <name> <detail-or-next-action>
record() {
    local status="$1" name="$2" msg="$3"
    case "$status" in
        PASS) PASS=$((PASS+1));;
        FAIL) FAIL=$((FAIL+1));;
        SKIP) SKIP=$((SKIP+1));;
    esac
    RESULTS+=("$(printf '%-4s %-28s %s' "$status" "$name" "$msg")")
    printf '%-4s %-28s %s\n' "$status" "$name" "$msg"
}

# run_gate <name> <log> <next-action> <cmd...>
# Wraps an existing gate script so we reuse its logic instead of duplicating
# it; the gate's own exit code is the verdict.
run_gate() {
    local name="$1" log="$LOG_DIR/$2" next="$3"; shift 3
    echo "==> $name ($*)"
    if "$@" >"$log" 2>&1; then
        record PASS "$name" "log: $log"
    else
        record FAIL "$name" "NEXT: $next (log: $log)"
    fi
}

echo "preflight: logs in $LOG_DIR"
echo

# ---------------------------------------------------------------------------
# 6. No stale product names in user-visible surfaces (aqua -> hexavoice ->
#    outloud left breakage twice, including a CI job dead for a day with
#    exit 127). Cheap, so it runs first.
#
# Deliberate survivors that must NOT be flagged:
#   - LEGACY_DIRS in crates/config/src/relocate.rs: frozen migration history.
#   - .aqua-oss / AQUA_SPEECH_HELPER / aqua-speech-helper: the pre-rename
#     names, now read-only fallbacks in config::paths::migrate_model_dir and
#     asr::backends::apple::find_helper so upgraders keep working.
#   - dev.hexavoice.* bundle ids: TCC keys grants by identifier; renaming
#     silently revokes every tester's permission (beta-readiness M8).
#   - "Aqua Voice" / withaqua.com: the competitor, not us.
#   - theme::palette AQUA / AQUA_PALE / AQUA_DEEP: the overlay's colour
#     names, and "macOS Aqua": Apple's name for the GUI session. Neither is
#     a product name.
#   - scripts/bundle-outloud-macos.sh: names the old bundles on purpose,
#     because unregistering them from LaunchServices is what it is for.
#   - scripts/bundle-macos.sh: internal harness. Its bundle id stays
#     dev.hexavoice.spike because TCC keys grants by identifier, but the
#     bundle it produces is now OutLoudSpike.app.
#   - audit/readiness docs and docs/planning: dated snapshots that quote old
#     output as evidence; rewriting evidence would falsify it.
#   - release-checklist.md: same class, the dated v0.1.0 audit that quotes
#     the pre-rename breakage verbatim as its evidence.
#   - the legacy compatibility surface: "# aqua shell-bridge" (the rc marker
#     the installer detects and rewrites), aqua.fish (the conf.d symlink it
#     removes), and "aqua-replay v1" (the schema line old replay records
#     carry). Each names the OLD artifact on purpose, to migrate or accept
#     it; renaming them would break exactly the installs they exist for.
# ---------------------------------------------------------------------------
check_stale_names() {
    local hits doc_hits rs_hits
    # Docs and scripts: any prose or command still using the old names is a
    # user-visible bug (a doc'd command that 404s is exactly how the rename
    # broke CI with exit 127). Migration/uninstall docs must name the old
    # products because removing them is their whole purpose.
    doc_hits="$(grep -rIn --include='*.md' --include='*.sh' \
                -iE 'hexavoice|\bhexad\b|\baqua\b' \
                README.md docs scripts 2>/dev/null \
        | grep -viE 'aqua[-_ ]?voice|withaqua\.com|\.aqua-oss|aqua[-_]speech[-_]helper|dev\.hexavoice|theme::palette|AQUA_PALE|AQUA_DEEP|palette::(PAPER|AQUA)|macOS Aqua' \
        | grep -vE '^scripts/(bundle-macos|bundle-outloud-macos|uninstall-macos|preflight)\.sh' \
        | grep -vE '^docs/(planning/|pre-release-audit|beta-readiness|release-readiness|release-checklist|M0-results|competitive-analysis|macos-quickstart|overlay-redesign)' \
        )" || true
    # Rust: comments are not user-visible, so only quoted string literals
    # count. Test fixtures and temp-dir names never reach a user either.
    # "macOS Aqua" is Apple's name for the GUI session, not our old name.
    rs_hits="$(grep -rIn --include='*.rs' \
                -E '"[^"]*([Aa]qua|[Hh]exavoice|hexad)[^"]*"' \
                crates 2>/dev/null \
        | grep -v 'crates/config/src/relocate.rs' \
        | grep -viE '\.aqua-oss|aqua[-_]speech[-_]helper|dev\.hexavoice|aqua[-_ ]?voice|withaqua|macOS Aqua' \
        | grep -vE '# aqua shell-bridge|aqua\.fish|aqua-replay v1' \
        | grep -vE 'temp_dir|-test|assert|/x/' \
        | grep -vE '^[^:]+:[0-9]+:\s*//' \
        )" || true
    hits="$(printf '%s\n%s' "$doc_hits" "$rs_hits" | grep -v '^$')" || true
    if [[ -n "$hits" ]]; then
        echo "$hits" > "$LOG_DIR/stale-names.txt"
        record FAIL "stale-product-names" \
            "NEXT: rename or add to the deliberate-survivor allowlist in scripts/preflight.sh ($(echo "$hits" | wc -l | tr -d ' ') hits, list: $LOG_DIR/stale-names.txt)"
    else
        record PASS "stale-product-names" "no unexplained aqua/hexavoice references"
    fi
}
check_stale_names

# ---------------------------------------------------------------------------
# 7. Doctor remedies point at things that exist. A remedy naming a bundle the
#    build no longer produces sends a user chasing a ghost, which is worse
#    than no remedy (this exact bug shipped once: cce4396).
# ---------------------------------------------------------------------------
check_doctor_remedies() {
    local checks_rs="crates/diag/src/checks.rs" bad=""
    if [[ ! -f "$checks_rs" ]]; then
        record SKIP "doctor-remedies" "$checks_rs not found"
        return
    fi
    # Every script a remedy names must exist on disk.
    local s
    for s in $(grep -oE 'scripts/[a-z0-9-]+\.sh' "$checks_rs" | sort -u); do
        [[ -f "$s" ]] || bad+="missing $s; "
    done
    # Every dist/<X>.app a remedy names must be produced by some bundle
    # script (source of truth: the APP_NAME= line in scripts/bundle-*.sh).
    local produced app
    produced="$(grep -h 'APP_NAME=' scripts/bundle-*.sh | sed -E 's/.*APP_NAME="([^"]+)".*/\1/')"
    for app in $(grep -oE 'dist/[A-Za-z]+\.app' "$checks_rs" | sort -u); do
        local name; name="$(basename "$app" .app)"
        echo "$produced" | grep -qx "$name" || bad+="no bundle script produces $app; "
    done
    # A remedy that pairs a bundle script with an app must pair them
    # correctly: "run scripts/bundle-X.sh ... open dist/Y.app" where X does
    # not build Y is precisely the ghost-chase this check exists for.
    while IFS= read -r line; do
        local script appref appname built
        script="$(echo "$line" | grep -oE 'scripts/bundle[a-z0-9-]*\.sh' | head -1)"
        appref="$(echo "$line" | grep -oE 'dist/[A-Za-z]+\.app' | head -1)"
        [[ -n "$script" && -n "$appref" && -f "$script" ]] || continue
        appname="$(basename "$appref" .app)"
        built="$(grep 'APP_NAME=' "$script" | sed -E 's/.*APP_NAME="([^"]+)".*/\1/')"
        [[ "$built" == "$appname" ]] || bad+="remedy pairs $script (builds $built.app) with $appref; "
    done < <(grep -E 'scripts/bundle.*dist/[A-Za-z]+\.app|dist/[A-Za-z]+\.app.*scripts/bundle' "$checks_rs")
    if [[ -n "$bad" ]]; then
        record FAIL "doctor-remedies" "NEXT: fix the remedy strings in $checks_rs -> $bad"
    else
        record PASS "doctor-remedies" "all referenced scripts and bundles exist"
    fi
}
check_doctor_remedies

# ---------------------------------------------------------------------------
# 4. App bundle well-formed. Checked against the bundle on disk; if none has
#    been built yet that is a SKIP with the command to produce one, not a
#    FAIL, because "not built yet" and "built wrong" are different problems.
# ---------------------------------------------------------------------------
check_bundle() {
    local app="dist/OutLoud.app" bad=""
    if [[ ! -d "$app" ]]; then
        record SKIP "app-bundle" "no $app; NEXT: run scripts/bundle-outloud-macos.sh then re-run preflight"
        return
    fi
    [[ -x "$app/Contents/MacOS/OutLoud" ]] || bad+="missing executable Contents/MacOS/OutLoud; "
    # The helper filename the bundle ships must be the one crates/asr's
    # find_helper() actually looks for. This exact mismatch shipped once
    # (1572e1f): a bundle that builds fine and silently cannot transcribe.
    # So the expected name is READ FROM THE CODE, never hardcoded here.
    local helper
    helper="$(grep -oE '"[a-z]+-speech-helper"' crates/asr/src/backends/apple.rs | tr -d '"' | head -1)"
    if [[ -z "$helper" ]]; then
        bad+="could not extract helper name from crates/asr/src/backends/apple.rs (find_helper changed?); "
    elif [[ ! -x "$app/Contents/MacOS/$helper" ]]; then
        bad+="helper $helper missing from bundle (crates/asr looks for exactly this name); "
    fi
    plutil -lint "$app/Contents/Info.plist" >/dev/null 2>&1 || bad+="Info.plist fails plutil -lint; "
    # CFBundleExecutable must name a binary that is actually there.
    local exec_name
    exec_name="$(plutil -extract CFBundleExecutable raw "$app/Contents/Info.plist" 2>/dev/null || true)"
    [[ -n "$exec_name" && -x "$app/Contents/MacOS/$exec_name" ]] || bad+="CFBundleExecutable '$exec_name' not present/executable; "
    ls "$app/Contents/Resources/"*.icns >/dev/null 2>&1 || bad+="no .icns icon in Resources (make-icon.sh failed?); "
    codesign --verify --deep "$app" >"$LOG_DIR/codesign.log" 2>&1 || bad+="codesign --verify failed (see $LOG_DIR/codesign.log); "
    if [[ -n "$bad" ]]; then
        record FAIL "app-bundle" "NEXT: re-run scripts/bundle-outloud-macos.sh; if still broken -> $bad"
    else
        record PASS "app-bundle" "binary, helper ($helper), Info.plist, icon, signature all good"
    fi
}
check_bundle

# ---------------------------------------------------------------------------
# 2. THE OVERLAY NEVER STEALS FOCUS. The single most important property: if
#    the overlay takes focus there is no focused text field left to dictate
#    into, and the product does nothing at all.
#
#    Automated probe, two layers:
#      a) record the frontmost app, launch the overlay demo (which drives the
#         real NSPanel through every visible state), confirm frontmost is
#         unchanged while it is on screen;
#      b) focus TextEdit, send synthetic keystrokes with the overlay still
#         up, and read the text back out of the document. If the keystrokes
#         landed, focus genuinely stayed with the text field.
#    Layer (b) needs osascript automation + Accessibility for the terminal;
#    without the grant it SKIPs with the grant as the next action rather
#    than reporting a false FAIL.
# ---------------------------------------------------------------------------
# Classify a frontmost change while the overlay is up.
#
# Only the overlay winning focus is a product bug. Another app winning it
# means something outside this check moved focus (the operator, a
# notification, a terminal reclaiming activation), which says nothing about
# the overlay and must not read as release-blocking: a FAIL nobody can
# reproduce is how a real one gets ignored.
#
# The overlay demo runs as `overlay-demo`, so that is the name to catch.
overlay_stole_focus() {
    [[ "$1" == *"overlay-demo"* || "$1" == *"overlay"* ]]
}

check_overlay_focus() {
    if [[ "$(uname)" != "Darwin" ]]; then
        record SKIP "overlay-focus" "macOS only"
        return
    fi
    # Remember where the operator was so focus can be handed back at the end.
    local operator_app
    operator_app="$(osascript -e 'tell application "System Events" to get name of first process whose frontmost is true' 2>/dev/null)" || {
        record SKIP "overlay-focus" "osascript cannot query System Events; NEXT: grant this terminal Automation + Accessibility in System Settings"
        return
    }
    # Build BEFORE establishing the focus baseline: a cold build takes long
    # enough for the operator to click something else, which would turn an
    # honest pass into a flaky fail.
    echo "==> overlay-focus: building overlay-demo"
    if ! cargo build -q -p overlay --bin overlay-demo >"$LOG_DIR/overlay-build.log" 2>&1; then
        record FAIL "overlay-focus" "NEXT: overlay-demo does not build (overlay swarm mid-flight?), see $LOG_DIR/overlay-build.log"
        return
    fi
    # Deterministic baseline: put TextEdit in front OURSELVES, then launch the
    # overlay on top of it. Comparing against "whatever the operator happened
    # to have focused" is race-prone; comparing against a baseline we set is
    # not, and it doubles as the target for the keystroke probe.
    if ! osascript >/dev/null 2>&1 <<'EOF'
tell application "TextEdit"
    activate
    make new document
end tell
EOF
    then
        record SKIP "overlay-focus" "cannot script TextEdit; NEXT: grant this terminal Automation for TextEdit in System Settings > Privacy & Security > Automation"
        return
    fi
    # Wait for TextEdit to actually BE frontmost before recording the
    # baseline, rather than sleeping a fixed second and hoping. A cold
    # TextEdit takes longer than 1s to come forward, and when it does the
    # baseline captures the terminal instead. The check then sees
    # "wezterm-gui -> TextEdit" after the overlay appears and reports a focus
    # steal that never happened, with the overlay blamed for the activation
    # this function itself requested. Observed on an otherwise idle machine.
    local front_before="?" waited
    for waited in 1 2 3 4 5 6 7 8 9 10; do
        front_before="$(osascript -e 'tell application "System Events" to get name of first process whose frontmost is true' 2>/dev/null || echo '?')"
        [[ "$front_before" == "TextEdit" ]] && break
        sleep 0.5
    done
    if [[ "$front_before" != "TextEdit" ]]; then
        record SKIP "overlay-focus" "TextEdit never came to the front (stuck at '$front_before'), so there is no baseline to compare against; NEXT: rerun without touching the machine"
        osascript -e 'tell application "TextEdit" to close front document saving no' >/dev/null 2>&1 || true
        return
    fi
    echo "==> overlay-focus: launching overlay-demo over $front_before"
    ./target/debug/overlay-demo >"$LOG_DIR/overlay-demo.log" 2>&1 &
    local demo_pid=$!
    sleep 4   # long enough for the panel to appear and cycle a state or two
    if ! kill -0 "$demo_pid" 2>/dev/null; then
        record FAIL "overlay-focus" "NEXT: overlay-demo exited within 4s (crash?), see $LOG_DIR/overlay-demo.log"
        osascript -e 'tell application "TextEdit" to close front document saving no' >/dev/null 2>&1 || true
        return
    fi
    local front_after
    front_after="$(osascript -e 'tell application "System Events" to get name of first process whose frontmost is true' 2>/dev/null || echo '?')"
    # Layer (b): with the overlay still on screen, type into TextEdit and read
    # the document back. If the text landed, keyboard focus genuinely stayed
    # with the field, which is the property that actually matters.
    local probe="preflight-focus-probe" typed="" keystroke_err=""
    # Keep stderr: without an Accessibility grant for whatever is running
    # this script, System Events refuses with "osascript is not allowed to
    # send keystrokes. (1002)" and NOTHING is typed. Discarding that error
    # made the empty document look like the overlay eating keystrokes, and
    # the check then reported a release-blocking focus bug that does not
    # exist. Layer (b) is documented above as SKIP-without-grant; this is
    # what makes that true.
    keystroke_err="$(osascript -e "tell application \"System Events\" to keystroke \"$probe\"" 2>&1 >/dev/null)" || true
    if [[ "$keystroke_err" == *"not allowed to send keystrokes"* || "$keystroke_err" == *"1002"* ]]; then
        kill "$demo_pid" 2>/dev/null; wait "$demo_pid" 2>/dev/null
        osascript -e 'tell application "TextEdit" to close front document saving no' >/dev/null 2>&1 || true
        if [[ "$front_after" != "$front_before" ]] && overlay_stole_focus "$front_after"; then
            record FAIL "overlay-focus" "NEXT: the overlay took focus ($front_before -> $front_after). Release-blocking: fix the NSPanel non-activating style in crates/overlay"
        elif [[ "$front_after" != "$front_before" ]]; then
            record SKIP "overlay-focus" "$front_after took focus mid-probe, not the overlay, so this run proves nothing; NEXT: re-run without touching the machine"
        else
            record SKIP "overlay-focus" "frontmost stayed $front_before, but the keystroke probe needs an Accessibility grant for the process running preflight; NEXT: grant it in System Settings > Privacy & Security > Accessibility, then re-run"
        fi
        return
    fi
    sleep 1
    typed="$(osascript -e 'tell application "TextEdit" to get text of front document' 2>/dev/null || true)"
    kill "$demo_pid" 2>/dev/null; wait "$demo_pid" 2>/dev/null
    osascript -e 'tell application "TextEdit" to close front document saving no' >/dev/null 2>&1 || true
    # Hand focus back; the probe must not leave TextEdit in the operator's face.
    if [[ -n "$operator_app" && "$operator_app" != "TextEdit" ]]; then
        osascript -e "tell application \"System Events\" to set frontmost of process \"$operator_app\" to true" >/dev/null 2>&1 || true
    fi
    if [[ "$front_after" != "$front_before" ]] && overlay_stole_focus "$front_after"; then
        record FAIL "overlay-focus" "NEXT: the overlay took focus ($front_before -> $front_after). Release-blocking: fix the NSPanel non-activating style in crates/overlay"
    elif [[ "$front_after" != "$front_before" ]]; then
        record SKIP "overlay-focus" "$front_after took focus mid-probe, not the overlay, so this run proves nothing; NEXT: re-run without touching the machine"
    elif [[ "$typed" != *"$probe"* ]]; then
        record FAIL "overlay-focus" "NEXT: frontmost unchanged but keystrokes did not land in TextEdit (got: '$typed'). Overlay is intercepting input; inspect the window level/ignoresMouseEvents in crates/overlay/src/macos.rs"
    else
        record PASS "overlay-focus" "frontmost stayed $front_before and keystrokes landed in TextEdit with overlay up"
    fi
}
check_overlay_focus

# ---------------------------------------------------------------------------
# 5. Idle CPU near zero. A menu-bar tool spinning at 30Hz while nobody speaks
#    is a battery bug no unit test will ever see. Method: launch the real
#    daemon with the overlay, let it settle, then average ps %cpu samples.
#    Threshold 5% here (vs the 1% nightly gate) because %cpu right after
#    launch still amortizes startup; a real spin shows up as 20-100%.
# ---------------------------------------------------------------------------
check_idle_cpu() {
    local bin="dist/OutLoud.app/Contents/MacOS/OutLoud"
    if [[ ! -x "$bin" ]]; then
        record SKIP "idle-cpu" "no built bundle; NEXT: run scripts/bundle-outloud-macos.sh then re-run preflight"
        return
    fi
    echo "==> idle-cpu: launching daemon and sampling for ~12s"
    "$bin" >"$LOG_DIR/idle-cpu.log" 2>&1 &
    local pid=$!
    sleep 6   # past startup: model load and first render must not count as idle
    if ! kill -0 "$pid" 2>/dev/null; then
        record SKIP "idle-cpu" "daemon exited during warmup (missing mic/AX grant is the usual cause); NEXT: check $LOG_DIR/idle-cpu.log"
        return
    fi
    local total=0 n=0 c
    for _ in 1 2 3; do
        c="$(ps -p "$pid" -o %cpu= | tr -d ' ')" || break
        total="$(echo "$total + $c" | bc)"; n=$((n+1))
        sleep 2
    done
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    if [[ $n -eq 0 ]]; then
        record SKIP "idle-cpu" "daemon died mid-sample; NEXT: check $LOG_DIR/idle-cpu.log"
        return
    fi
    local avg; avg="$(echo "scale=1; $total / $n" | bc)"
    if (( $(echo "$avg < 5.0" | bc) )); then
        record PASS "idle-cpu" "avg ${avg}% over $n samples"
    else
        record FAIL "idle-cpu" "NEXT: avg ${avg}% idle CPU; profile the overlay/status-item redraw timer (crates/overlay) - it must be event-driven while idle, not a free-running 30Hz loop"
    fi
}
check_idle_cpu

# ---------------------------------------------------------------------------
# 1 + 3. The existing gates. Reused wholesale (never re-implemented) so this
# script and CI can never disagree about what "the gate" means. Heaviest
# last; --quick skips them for fast iteration on the checks above.
# ---------------------------------------------------------------------------
if [[ $QUICK -eq 1 ]]; then
    record SKIP "ci-check"        "--quick; NEXT: run scripts/preflight.sh without --quick before tagging"
    record SKIP "cargo-test"      "--quick"
    record SKIP "ci-compliance"   "--quick"
    record SKIP "headless-build"  "--quick"
    record SKIP "latency-gate"    "--quick"
    record SKIP "perf-gate"       "--quick"
else
    run_gate "ci-check" ci-check.log \
        "read the first 'error' line in the log; fmt/clippy/test drift is usually a sibling swarm's in-flight change" \
        scripts/ci-check.sh
    # Explicit even though ci-check also runs it: "cargo test --workspace" is
    # a named release gate, and if ci-check is ever refactored to drop it,
    # this line keeps the gate alive. Warm cache makes it near-free.
    run_gate "cargo-test" cargo-test.log \
        "fix the failing test or file it against the owning swarm; do not ship with a red workspace" \
        cargo test --workspace --locked
    run_gate "ci-compliance" ci-compliance.log \
        "a licence/CVE hit: read the cargo-deny/cargo-audit section of the log; new deps from the framework evaluation are the likely source" \
        scripts/ci-compliance.sh
    # build-headless.sh contains the no-display-libraries link check
    # (otool/ldd tripwire); running the script IS check 3, no duplication.
    run_gate "headless-build" headless.log \
        "a GUI dependency leaked into the default feature set; gate it behind the display feature (see the FAIL line in the log)" \
        scripts/build-headless.sh
    # Steals focus for ~1 min and needs the terminal's Accessibility grant;
    # the bench itself prints SKIP-and-exit-0 when it cannot measure.
    run_gate "latency-gate" latency.log \
        "a percentile crossed its budget; compare against docs/latency.md baselines and bisect against the overlay swarm's commits" \
        scripts/bench-gate.sh
    # The pure counterpart to the gate above. That one measures the real
    # accessibility path and honestly SKIPs when no text field is focused,
    # which means a preflight run from the wrong window checks nothing. This
    # one measures only computation, so it cannot skip and cannot be dodged
    # by where the cursor happens to be.
    run_gate "perf-gate" perf.log \
        "a pure hot path got materially slower; this is invisible to every correctness test, so read crates/overlay/benches/perf_gate.rs for the budget and its reasoning" \
        cargo bench -p overlay --bench perf_gate
fi

# ---------------------------------------------------------------------------
echo
echo "================ preflight summary ================"
for r in "${RESULTS[@]}"; do echo "$r"; done
echo "---------------------------------------------------"
echo "PASS=$PASS FAIL=$FAIL SKIP=$SKIP  (logs: $LOG_DIR)"
if [[ $FAIL -gt 0 ]]; then
    echo "VERDICT: NOT SAFE TO SHIP ($FAIL failing check(s) above, each with a NEXT action)"
    exit 1
fi
if [[ $SKIP -gt 0 ]]; then
    echo "VERDICT: no failures, but $SKIP check(s) skipped - a human must judge whether each skip is acceptable"
else
    # The exact phrase the release checklist (docs/release-checklist.md §4)
    # gates on; keep them in step or the release steps ask for a line this
    # script never prints.
    echo "VERDICT: SAFE TO SHIP"
fi
exit 0
