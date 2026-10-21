#!/usr/bin/env bash
# Expand just the NSIS heredoc from build-windows.sh with test values, so
# the generated installer.nsi can be eyeballed (and parsed by makensis when
# present) without running a full release build.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=0.1.0
TARGET=x86_64-pc-windows-msvc
OUT="${TMPDIR:-/tmp}/nsitest"
mkdir -p "$OUT"
# shellcheck disable=SC1090
source <(sed -n '/cat > "\$OUT\/installer.nsi"/,/^NSIS$/p' scripts/build-windows.sh)
echo "--- generated $OUT/installer.nsi ---"
grep -E 'OutFile|InstallDir |FINISHPAGE_RUN|CreateShortCut|DisplayVersion|taskkill|DeleteRegValue|SMPROGRAMS' "$OUT/installer.nsi"
