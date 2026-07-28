#!/usr/bin/env bash
# Remove Aqua from this Mac: processes, app bundle, TCC grants, shell plugin,
# and (optionally) the user's configuration.
#
# Why this script exists at all: a beta tester who decides the tool is not for
# them must be able to get their machine back. Without this, removing Aqua by
# hand means knowing about four separate locations, two of which (the TCC
# grant and the `.zshrc` line) are invisible in Finder and neither of which a
# user would think to look for. "How do I uninstall it?" going unanswered is
# how a beta earns a reputation for being invasive.
#
# Deliberately conservative: it removes only paths this project creates, it
# never touches anything it did not put there, and it leaves the user's
# configuration alone unless explicitly asked with --purge. Uninstalling is
# not the same as wanting your settings destroyed, and a tester who reinstalls
# tomorrow should not have to redo their setup.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PURGE=0
DRY_RUN=0
for arg in "$@"; do
    case "$arg" in
        --purge)   PURGE=1 ;;
        --dry-run) DRY_RUN=1 ;;
        -h|--help)
            cat <<'USAGE'
usage: uninstall-macos.sh [--purge] [--dry-run]

  --purge     also delete ~/.config/aqua (your settings and vocabulary)
  --dry-run   print what would happen, change nothing

Without --purge your configuration is kept, so reinstalling restores your
setup. With --dry-run nothing is removed; use it to see the plan first.
USAGE
            exit 0
            ;;
        *) echo "unknown argument $arg (try --help)" >&2; exit 2 ;;
    esac
done

# Every mutating action goes through this, so --dry-run cannot be forgotten at
# a call site and the output doubles as documentation of what was done.
act() {
    if [[ "$DRY_RUN" == 1 ]]; then
        echo "would: $*"
    else
        echo "==> $*"
        "$@"
    fi
}

say() { echo "$*"; }

say "Uninstalling Aqua from this Mac."
[[ "$DRY_RUN" == 1 ]] && say "(dry run: nothing will be changed)"
say ""

# 1. Stop anything that is running. The daemon holds the microphone and an
#    event tap, so leaving it alive would make every later step look like it
#    silently failed: the files go away and the tool keeps working until
#    reboot.
say "1. Stopping running processes"
STOPPED=0
for pattern in 'Aqua.app/Contents/MacOS/Aqua' 'target/release/aquad' 'aqua-speech-helper' 'shell-bridge'; do
    # pgrep exits 1 when nothing matches, which is the common case and not an
    # error; `|| true` keeps `set -e` from treating a clean machine as failure.
    pids="$(pgrep -f "$pattern" || true)"
    if [[ -n "$pids" ]]; then
        for pid in $pids; do
            # Never kill this script or its own process group.
            [[ "$pid" == "$$" ]] && continue
            act kill "$pid"
            STOPPED=1
        done
    fi
done
[[ "$STOPPED" == 0 ]] && say "    nothing was running"
say ""

# 2. The app bundles. Only the ones this repo builds into dist/, by exact
#    name: a glob here could match something a user put there themselves.
say "2. Removing built app bundles"
REMOVED_BUNDLE=0
for app in Aqua.app AquaSpike.app AquaDoctor.app; do
    if [[ -d "$ROOT/dist/$app" ]]; then
        act rm -rf "$ROOT/dist/$app"
        REMOVED_BUNDLE=1
    fi
done
if [[ -d "/Applications/Aqua.app" ]]; then
    act rm -rf "/Applications/Aqua.app"
    REMOVED_BUNDLE=1
fi
[[ "$REMOVED_BUNDLE" == 0 ]] && say "    no app bundles found"
say ""

# 3. TCC grants. These are the reason a hand uninstall is not enough: the
#    Accessibility and Microphone entries survive deleting the app, so the
#    stale entry sits in System Settings forever, and a later reinstall
#    inherits a grant pinned to a cdhash that no longer exists, which presents
#    as "the toggle is on and nothing works". Resetting is the only way to
#    leave a clean slate.
say "3. Resetting permission grants"
for bundle_id in dev.aquaoss.aquad dev.aquaoss.spike dev.aquaoss.doctor; do
    # tccutil fails when there is no grant to reset, which is fine.
    act tccutil reset Accessibility "$bundle_id" || true
    act tccutil reset Microphone "$bundle_id" || true
done
say ""

# 4. The shell plugin line. `shell-bridge install` appends a guarded line to
#    the user's rc file and has no uninstall of its own, so a stale line
#    pointing at a deleted repo would print an error on every new shell.
#    Matched on the exact marker comment the installer writes.
say "4. Removing the shell plugin line"
REMOVED_RC=0
for rc in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.config/fish/config.fish"; do
    [[ -f "$rc" ]] || continue
    if grep -q 'aqua shell-bridge' "$rc" 2>/dev/null; then
        if [[ "$DRY_RUN" == 1 ]]; then
            say "would: remove the aqua shell-bridge lines from $rc"
        else
            # Keep a backup: editing someone's rc file is the one step here
            # that could cost them work that is not ours.
            cp "$rc" "$rc.aqua-uninstall-backup"
            grep -v 'aqua shell-bridge' "$rc" | grep -v 'shell/aqua\.\(zsh\|bash\|fish\)' > "$rc.tmp"
            mv "$rc.tmp" "$rc"
            say "==> cleaned $rc (backup at $rc.aqua-uninstall-backup)"
        fi
        REMOVED_RC=1
    fi
done
[[ "$REMOVED_RC" == 0 ]] && say "    no shell plugin line found"
say ""

# 5. Runtime leftovers that are not configuration: the bridge socket and the
#    downloaded-model directory. Removed unconditionally because neither is
#    something the user authored.
say "5. Removing runtime state"
REMOVED_STATE=0
for path in "${TMPDIR:-/tmp}/aqua-shell-bridge.sock" "$HOME/.aqua-oss/models"; do
    if [[ -e "$path" ]]; then
        act rm -rf "$path"
        REMOVED_STATE=1
    fi
done
[[ "$REMOVED_STATE" == 0 ]] && say "    no runtime state found"
say ""

# 6. Configuration, only on request. See the header: uninstalling is not the
#    same as consenting to lose your settings.
say "6. Configuration"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/aqua"
if [[ "$PURGE" == 1 ]]; then
    if [[ -d "$CONFIG_DIR" ]]; then
        act rm -rf "$CONFIG_DIR"
    else
        say "    nothing at $CONFIG_DIR"
    fi
else
    if [[ -d "$CONFIG_DIR" ]]; then
        say "    keeping $CONFIG_DIR (pass --purge to delete your settings too)"
    else
        say "    nothing at $CONFIG_DIR"
    fi
fi
say ""

say "Done."
say ""
say "One thing this script cannot do for you: macOS keeps a stale entry in"
say "System Settings > Privacy & Security > Accessibility until you remove it"
say "by hand. Select 'Aqua' there and press the minus button if it is still"
say "listed. Apple provides no API to remove a row, only to reset the grant"
say "behind it, which step 3 already did."
