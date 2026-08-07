#!/usr/bin/env python3
"""Refuse to build if the cat mascot ever reaches `main`.

WHY: the cat mascot lives on `overlay/cat-mascot` and was explicitly ruled
out of `main`. The product's mark is the skull. That instruction is a fact
about the repository, not a preference someone should have to remember
during a merge, so it is enforced here rather than trusted.

This is not hypothetical. Two OutLoud icons appeared in the menu bar at
once, and the reasonable first assumption was that cat-branch code had been
merged. It had not -- the second icon was an older build predating the
skull -- but nothing in the repo could have proved that quickly, and a real
merge would have looked identical.

Narrow on purpose: it matches the cat mascot's own module and glyph names,
not the word "cat", which legitimately appears inside words like
"relocate", "concatenate", and "category". A check with false positives
gets disabled, and then it protects nothing.

Usage: scripts/ci-check-no-cat-mascot.py
Exit: 0 clean, 1 if cat mascot code is present.
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Files that exist only on the cat branch. Their presence on `main` means a
# merge or cherry-pick brought the mascot across.
FORBIDDEN_FILES = [
    "crates/overlay/src/cat.rs",
    "crates/overlay/src/bin/cat-svg.rs",
]

# Identifiers from the cat mascot's geometry and animator. Word-bounded so
# "relocate" and "concatenate" cannot trip them.
FORBIDDEN_SYMBOLS = [
    r"\bcat_mascot\b",
    r"\bCatMascot\b",
    r"\bdraw_cat\b",
    r"\bcat_glyph\b",
    r"\bwhisker\b",
    r"\bpupil_glint\b",
]

PATTERN = re.compile("|".join(FORBIDDEN_SYMBOLS))


def tracked_source_files() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "crates", "scripts", "assets"],
        cwd=ROOT, capture_output=True, text=True, check=True,
    ).stdout.split()
    return [ROOT / p for p in out if p.endswith((".rs", ".svg", ".sh", ".py"))]


def main() -> int:
    problems: list[str] = []

    for rel in FORBIDDEN_FILES:
        if (ROOT / rel).exists():
            problems.append(f"{rel} exists; it belongs only on overlay/cat-mascot")

    for path in tracked_source_files():
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for i, line in enumerate(text.splitlines(), 1):
            # A comment naming the branch (to explain why it is excluded) is
            # fine; cat mascot CODE is not.
            if PATTERN.search(line) and not line.lstrip().startswith(("//", "#")):
                rel = path.relative_to(ROOT)
                problems.append(f"{rel}:{i}: {line.strip()}")

    if problems:
        print("Cat mascot code found on this branch:\n", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        print(
            "\nThe cat mascot must not touch `main`. The product's mark is the\n"
            "skull (crates/overlay/src/mark.rs). If a merge brought this across,\n"
            "revert it rather than adjusting this check.",
            file=sys.stderr,
        )
        return 1

    print("==> no-cat-mascot check OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
