#!/usr/bin/env bash
# aqua shell-bridge demo: launch the bridge, open a zsh with the plugin
# installed into a THROWAWAY ZDOTDIR (your real ~/.zshrc is not touched),
# stage an edit, and let you watch the command line rewrite itself.
#
# Usage: shell/demo.sh
#   Then, inside the demo zsh:
#     1. type (do not run):  kubectl get pods --namespace prod-web
#     2. in another terminal: cargo run -p shell-bridge -- intent "change prod-web to staging-web"
#        (or just wait: this script pre-stages that exact intent)
#     3. press Ctrl-X Ctrl-A. The line rewrites in place; Ctrl-X u undoes it.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"

cargo build -p shell-bridge
bridge="$here/target/debug/shell-bridge"

# Isolated socket so the demo never collides with a real daemon.
demo_dir=$(mktemp -d)
socket="$demo_dir/aqua/shell.sock"
trap 'kill $bridge_pid 2>/dev/null || true; rm -rf "$demo_dir"' EXIT

"$bridge" serve --socket "$socket" &
bridge_pid=$!
# Wait for the socket instead of sleeping a guess.
for _ in $(seq 50); do [ -S "$socket" ] && break; sleep 0.1; done
[ -S "$socket" ] || { echo "bridge failed to start" >&2; exit 1; }

# Pre-stage the demo edit so step 2 is optional.
"$bridge" intent "change prod-web to staging-web" --socket "$socket"

# Throwaway zsh config: sources the plugin, points it at the demo socket.
zdot="$demo_dir/zdot"
mkdir -p "$zdot"
cat > "$zdot/.zshrc" <<RC
export AQUA_BRIDGE_SOCKET="$socket"
source "$here/shell/aqua.zsh"
PROMPT='aqua-demo %% '
echo "aqua demo shell. Type: kubectl get pods --namespace prod-web"
echo "Then press Ctrl-X Ctrl-A to apply the staged edit. Ctrl-X u undoes."
RC

ZDOTDIR="$zdot" zsh -i
