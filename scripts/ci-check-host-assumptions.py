#!/usr/bin/env python3
"""Flag tests that assume a resource the CI runner may not have.

WHY THIS EXISTS: a test of mine called `ClipboardTarget::new().expect(...)`,
which needs a real clipboard tool (pbcopy, wl-copy, xclip). Every developer
machine has one; a headless Linux runner does not. It passed locally and
failed the Linux CI job with "no clipboard tool found".

`scripts/ci-check-linux.sh` exists to catch exactly that class of
macOS-blind-spot, and could not: it runs clippy, and clippy never executes
tests. A cross-compile lint proves a thing builds, not that it passes.

Running the whole suite under Linux would need a Linux machine, which is
what CI is for. What CAN be checked from here, cheaply and without a
runtime, is the source-level shape of the mistake: a test that unwraps a
constructor documented to fail when a system tool is absent.

Deliberately narrow. It matches constructors whose own error path says the
resource may be missing, so it fires on the real hazard rather than on
every `expect` in the test suite. Broad heuristics here produce dozens of
false reports, which train people to ignore the check -- worse than no
check.

Usage: scripts/ci-check-host-assumptions.py
Exit: 0 clean, 1 on a finding.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Constructors that fail when an external tool or display server is absent.
# Extend this list when a new one appears, and say why in the comment.
ENVIRONMENT_DEPENDENT = [
    # Needs pbcopy / wl-copy / xclip. Absent on a headless runner.
    "ClipboardTarget::new",
]

# `.expect(` / `.unwrap()` applied to one of the above, on the same line.
PATTERN = re.compile(
    r"(?P<ctor>" + "|".join(re.escape(c) for c in ENVIRONMENT_DEPENDENT) + r")"
    r"\s*\([^)]*\)\s*\.\s*(?P<how>expect|unwrap)\s*\("
)


def in_test_code(lines: list[str], index: int) -> bool:
    """Whether the line sits under a #[cfg(test)] module or a #[test] fn.

    Scans backwards rather than parsing: a real parse would need syn, and
    the question is only "is this test-only code", which the attributes
    answer well enough for a lint.
    """
    for line in reversed(lines[:index]):
        stripped = line.strip()
        if stripped.startswith("#[cfg(test)]") or stripped.startswith("#[test]"):
            return True
        # A non-test item boundary at column 0 ends the search.
        if stripped.startswith("pub fn ") or stripped.startswith("impl "):
            return False
    return False


def main() -> int:
    findings = []
    for path in sorted(ROOT.glob("crates/**/*.rs")):
        if "/target/" in str(path):
            continue
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (UnicodeDecodeError, OSError):
            continue
        for i, line in enumerate(lines):
            match = PATTERN.search(line)
            if not match:
                continue
            if not in_test_code(lines, i):
                continue
            findings.append(
                (path.relative_to(ROOT), i + 1, match.group("ctor"), match.group("how"))
            )

    if not findings:
        print("==> host-assumption check OK")
        return 0

    print("Tests that assume a resource the CI runner may not have:\n", file=sys.stderr)
    for rel, lineno, ctor, how in findings:
        print(f"  {rel}:{lineno}", file=sys.stderr)
        print(f"      {ctor}(...).{how}(...)", file=sys.stderr)
    print(
        "\nThese pass on a developer machine and fail on a headless runner.\n"
        "Skip instead of panicking when the resource is absent:\n\n"
        "    let Ok(target) = ClipboardTarget::new() else { return };\n",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
