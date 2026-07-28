#!/usr/bin/env bash
# Render the README's mermaid diagrams to committed SVGs.
#
# WHY this exists: GitHub renders ```mermaid fenced blocks on the website, but
# not everywhere. The GitHub mobile app, most third-party clients, npm/crates
# mirrors, and anything that renders README.md with a plain Markdown library
# all show the raw mermaid source instead: a wall of arrows and node ids where
# a diagram should be. The first two diagrams here carry the explanation of
# what the product does, so the readers who see them broken are exactly the
# ones deciding whether to keep reading.
#
# An <img> tag pointing at an SVG renders in all of those, and stays crisp on
# retina displays.
#
# The source of truth stays the .mmd files in docs/assets/diagrams/, so the
# diagrams are still diffable text in review, not opaque binaries.
#
# Requires network on first run (npx fetches mermaid-cli). Regenerate with:
#   ./scripts/render-diagrams.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/docs/assets/diagrams"
OUT="$ROOT/docs/assets"

if ! command -v npx >/dev/null 2>&1; then
    echo "npx not found; install Node to regenerate diagrams" >&2
    exit 1
fi

shopt -s nullglob
sources=("$SRC"/*.mmd)
if [ ${#sources[@]} -eq 0 ]; then
    echo "no .mmd sources in $SRC" >&2
    exit 1
fi

for src in "${sources[@]}"; do
    name="$(basename "$src" .mmd)"
    dest="$OUT/$name.svg"
    echo "==> $name.mmd -> $name.svg"
    # -b transparent so the diagram sits on whatever background the reader's
    # theme provides; a baked-in white block looks broken in dark mode.
    #
    # The config file is not optional. By default mermaid puts node labels in
    # <foreignObject> elements, and GitHub's SVG sanitizer strips those, so the
    # diagram arrives as a set of correctly drawn, completely empty boxes.
    # htmlLabels:false emits real <text> instead, which survives sanitizing.
    npx --yes @mermaid-js/mermaid-cli \
        --input "$src" \
        --output "$dest" \
        --configFile "$SRC/mermaid.json" \
        --backgroundColor transparent
done

# Verify what we just produced, because the failure mode here is silent: a
# diagram whose labels were stripped still looks like a valid SVG to every
# tool in this pipeline, and only shows up as empty boxes on github.com.
failed=0
for src in "${sources[@]}"; do
    name="$(basename "$src" .mmd)"
    dest="$OUT/$name.svg"
    if grep -q "foreignObject" "$dest"; then
        echo "FAIL: $name.svg contains foreignObject; GitHub will strip the labels" >&2
        failed=1
    fi
done
[ "$failed" -eq 0 ] || exit 1

echo
echo "Rendered ${#sources[@]} diagram(s) into $OUT"
echo "Commit the .svg files alongside their .mmd sources."
