#!/usr/bin/env bash
# Validate the GitHub Actions workflow files before pushing them.
#
# WHY this exists: a workflow file that GitHub cannot parse fails in a
# uniquely useless way. The run appears, ends in 0 seconds, creates no jobs,
# and reports only "This run likely failed because of a workflow file issue"
# with no line number. Nothing local catches it, because nothing local reads
# these files at all, so the loop is: push, wait, see red, guess, repeat.
#
# It has already happened twice here:
#   - `if: ... && !matrix.use-cross` — a bare `!` is a YAML tag indicator.
#   - `run: echo "::error title=x::y. Something: else"` — an unquoted scalar
#     containing ": " parses as a nested mapping.
# Both look completely ordinary to a human reader.
#
# js-yaml via npx rather than a Python dependency: Node is already required
# for scripts/render-diagrams.sh, and this needs no virtualenv on a machine
# with an externally-managed Python.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v npx >/dev/null 2>&1; then
    echo "SKIP: npx not found, cannot validate workflow YAML" >&2
    exit 0
fi

shopt -s nullglob
files=(.github/workflows/*.yml .github/workflows/*.yaml .github/ISSUE_TEMPLATE/*.yml)
if [ ${#files[@]} -eq 0 ]; then
    echo "no workflow files found" >&2
    exit 1
fi

failed=0
for f in "${files[@]}"; do
    if err=$(npx --yes js-yaml "$f" 2>&1 >/dev/null); then
        echo "  ok   $f"
    else
        echo "  FAIL $f" >&2
        # The parser names the line and column; that is the whole value here.
        echo "$err" | head -5 | sed 's/^/       /' >&2
        failed=1
    fi
done

if [ "$failed" -ne 0 ]; then
    echo >&2
    echo "A workflow that does not parse produces a 0-second run with no jobs" >&2
    echo "and no line number in the GitHub UI. Fix it here instead." >&2
    exit 1
fi

echo "==> workflow YAML OK (${#files[@]} files)"
