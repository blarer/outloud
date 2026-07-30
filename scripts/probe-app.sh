#!/usr/bin/env bash
# What does injection actually see in a given app?
#
# WHY: "it does not work in Discord" and "it does not work in iMessage" are
# the same sentence describing potentially different faults. The write can
# fail at four distinct points, and they need different fixes:
#
#   1. The app is not identified          -> the per-app rules never apply
#   2. No focused text field is exposed   -> nothing to write into
#   3. The field is read-only             -> AX refused, fallback should run
#   4. The write reports success and the  -> the silent-drop case, which is
#      text never appears                    the worst because nothing errors
#
# This reports which one, without dictating anything.
#
# Usage: scripts/probe-app.sh
#   Click into the app's message box first, then run it.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DELAY="${1:-5}"

cargo build --release -p spike-cli >/dev/null 2>&1 || cargo build --release -p spike-cli

echo "Click into the target app's text field now. Probing in ${DELAY}s..."
echo

./target/release/spike-cli probe --after "$DELAY"

echo
echo "----------------------------------------------------------------"
echo "How to read this:"
echo
echo "  app:       must name the real app. If it says the wrong thing,"
echo "             the per-app rules in text-target/src/targets/keys.rs"
echo "             never fire, because they match on this string."
echo
echo "  role:      AXTextArea / AXTextField means a writable field was"
echo "             found. Anything else (AXGroup, AXWebArea, unknown)"
echo "             means the app hides its editor from accessibility."
echo
echo "  writable:  value=false AND selectedText=false means AX cannot"
echo "             write at all, so injection must fall back to typing."
echo
echo "  strategy:  what the code decided to do. clipboard-paste means it"
echo "             gave up on AX entirely."
