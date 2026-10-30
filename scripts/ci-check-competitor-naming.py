#!/usr/bin/env python3
"""Catch competitor references that a product rename turned into our own name.

THE BUG THIS EXISTS FOR, because it is not hypothetical:

`a4b4f09` ("rename the product from Aqua to Hexavoice") did a global
substitution across the tree. `docs/competitive-analysis.md` is a document
*about the competitor*, which was named Aqua, and all 48 of its references to
Aqua became our own product name. The document then asserted, in our own
repository, that OutLoud is a cloud product with a proprietary ASR model, that
it retains transcripts to train on, and that it has no offline mode. Every one
of those is the opposite of true about us, and each is accurate about the
competitor. Two UX documents picked up the same damage in passing.

Nothing noticed for three months. The rename commit was careful, atomic, and
well-reasoned about the things it knew it was changing; it simply could not
know that one file used the old product name to mean someone else.

WHY A SCRIPT RATHER THAN A NOTE IN THE RENAME RUNBOOK:

A note is read by whoever remembers to look for it. The failure mode here is
specifically a bulk mechanical edit, which is the operation least likely to
involve reading the affected files. So this checks the property directly: our
product name must never appear as the subject of a claim that is only true of
a cloud competitor.

HOW IT DECIDES, and why it is deliberately dumb:

A list of phrases that are false about us and true about a cloud product
("<us> is cloud-based", "<us> caps free users at", "<us>'s own FAQ"). Any hit
is reported. There is no cleverness and no model: the check has to be
explicable in one sentence or nobody will trust its output enough to act on
it, and a false positive here costs ten seconds of reading.

It greps for the product name from Cargo.toml rather than hardcoding it, so
the next rename does not quietly disable the check that exists because of the
last one.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Claims that are false about a local, offline, MIT-licensed tool and true
# about a hosted competitor. Each is a regex fragment placed after the product
# name. Keep them specific: "<us> is" alone would match a hundred true
# sentences.
FALSE_ABOUT_US = [
    r"is (a )?cloud[- ]based",
    r"is (a )?cloud product",
    r"caps (free )?users",
    r"retains (audio|transcripts)",
    r"'s own (FAQ|guide|changelog|advice|support)",
    r"'s (headline|flagship) (claim|feature)",
    r"explicitly says",
    r"teaches \*?no\*? punctuation",
    r"relies on an LLM",
    r"mirroring \w+'s",
    r"needs a connection",
    r"is unavailable in (Realtime|Instant) mode",
    r"'s equivalent is",
    r"allows \d+ bindings",
]

# Files where our own name legitimately appears constantly, and where a
# competitor was never discussed. Skipping them keeps the signal clean.
SKIP = {
    "CHANGELOG.md",
    "Cargo.lock",
}


def product_name() -> str:
    """Our product's display name, read from the workspace rather than fixed.

    A hardcoded name would mean the next rename turns this check into a
    no-op that still exits 0, which is the exact failure shape the check is
    supposed to prevent.
    """
    cargo = (ROOT / "crates/outloud/Cargo.toml").read_text()
    match = re.search(r'^name\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
    if not match:
        sys.exit("could not read the package name from crates/outloud/Cargo.toml")
    # The crate is lowercase; the display name used in prose is not. Both
    # spellings matter, so the caller searches case-insensitively.
    return match.group(1)


def tracked_text_files() -> list[Path]:
    """Git-tracked docs and source. Untracked scratch files are not our problem."""
    out = subprocess.run(
        ["git", "ls-files", "*.md", "*.rs", "*.sh", "*.toml"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return [ROOT / line for line in out.stdout.split() if Path(line).name not in SKIP]


def main() -> int:
    name = product_name()
    patterns = [
        re.compile(rf"\b{re.escape(name)}\b\s*{frag}", re.IGNORECASE)
        for frag in FALSE_ABOUT_US
    ]
    print(f"==> checking for competitor claims attributed to '{name}'")

    hits: list[tuple[Path, int, str]] = []
    for path in tracked_text_files():
        try:
            lines = path.read_text().splitlines()
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        for lineno, line in enumerate(lines, start=1):
            if any(p.search(line) for p in patterns):
                hits.append((path.relative_to(ROOT), lineno, line.strip()))

    if not hits:
        print("OK: no competitor claims are attributed to us")
        return 0

    print(f"\nFAIL: {len(hits)} line(s) state something about '{name}' that is")
    print("only true of a hosted competitor:\n")
    for path, lineno, text in hits:
        shown = text if len(text) <= 100 else text[:97] + "..."
        print(f"  {path}:{lineno}")
        print(f"    {shown}")

    print(
        f"\nThis is the signature of a bulk rename that renamed the competitor\n"
        f"too. `a4b4f09` did exactly this to docs/competitive-analysis.md and it\n"
        f"went unnoticed for three months.\n\n"
        f"Check whether each line means us or the competitor. If a document is\n"
        f"about a competitor, say so in a note at the top so the next rename has\n"
        f"a chance of noticing, and refer to ourselves as 'we' rather than by\n"
        f"name inside it.\n\n"
        f"If a hit is a genuine true statement about {name}, add the phrase to\n"
        f"this script's exceptions with a comment rather than loosening the\n"
        f"pattern.\n"
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
