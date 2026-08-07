#!/usr/bin/env bash
# Render docs/assets/logo.svg into a macOS .icns app icon.
#
# WHY this exists: the bundle previously shipped with no CFBundleIconFile at
# all, so Finder, the Dock's app switcher, System Settings' Accessibility list,
# and the permission prompts all showed the generic blank-page icon. That is
# not cosmetic here: users identify the app in the Accessibility list by its
# icon, and "the nameless blank app is asking to control my computer" is a
# reasonable thing to refuse.
#
# WHY it uses only system tools: qlmanage, sips, and iconutil all ship with
# macOS. Requiring rsvg-convert or cairosvg would mean a contributor cannot
# build a complete app without first installing a graphics toolchain, and an
# icon is exactly the sort of thing that then silently gets skipped.
#
# WHY the SVG stays the source of truth: it is the same asset the README
# renders, so the app icon and the GitHub page can never drift apart.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SVG="$ROOT/docs/assets/logo.svg"
OUT="${1:-$ROOT/dist/OutLoud.icns}"

[ -f "$SVG" ] || { echo "missing $SVG" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# qlmanage renders through Quick Look, which understands SVG. It insists on
# writing <name>.png into a directory rather than to a path we choose, hence
# the fixed-up move below.
qlmanage -t -s 1024 -o "$work" "$SVG" >/dev/null 2>&1 || true

master="$work/logo.svg.png"
if [ ! -f "$master" ]; then
    echo "qlmanage could not rasterize the SVG" >&2
    echo "install librsvg (brew install librsvg) and re-run, or commit a PNG master" >&2
    exit 1
fi

# A file is not proof of an image.
#
# Quick Look writes a PNG even when it fails to PARSE the SVG: near-blank
# canvas, exit status 0, plausible file size. That shipped an empty app icon
# and every check downstream agreed, because they were all looking at the
# same blank PNG.
"$ROOT/scripts/ci-check-icon-not-blank.py" "$master"

# An iconset is a directory of fixed-name sizes; iconutil refuses anything else.
# Both 1x and 2x are required or macOS picks a blurry scale on Retina displays.
iconset="$work/OutLoud.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$master" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$master" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p "$(dirname "$OUT")"
iconutil --convert icns "$iconset" --output "$OUT"

echo "wrote $OUT"
