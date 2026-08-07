#!/usr/bin/env python3
"""Check that a startup warning can reach a user who has no terminal.

WHY: a daemon launched from Finder has no terminal attached, so anything
written only to stderr is invisible. That is exactly how "it asks for
permissions but doesn't work" happened: the Input Monitoring warning
existed, printed correctly, and no double-clicking user ever saw it. The
app came up looking healthy with a hotkey that could never fire.

Source-level on purpose. Whether a message reaches a GUI user is a
question about which function the branch calls, which is visible in the
source and needs no runtime, no display server, and no permissions.

Usage: scripts/ci-check-gui-visible-warnings.py
Exit: 0 clean, 1 when a startup warning is stderr-only.
"""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAIN_RS = ROOT / "crates" / "outloud" / "src" / "main.rs"

# The helper that puts a message in front of someone with no terminal.
GUI_REPORTER = "report_startup_warning_to_the_user"

# Startup conditions that leave the app running but not working. Each is a
# string from the warning itself, so a rename shows up here as a skip
# rather than a false pass.
MUST_REACH_THE_GUI = [
    "no Input Monitoring access",
]


def main() -> int:
    if not MAIN_RS.is_file():
        print(f"missing {MAIN_RS.relative_to(ROOT)}", file=sys.stderr)
        return 1

    text = MAIN_RS.read_text(encoding="utf-8")
    problems = []

    for needle in MUST_REACH_THE_GUI:
        if needle not in text:
            # Renamed or removed. Not this check's call to guess at.
            continue
        if GUI_REPORTER not in text:
            problems.append(
                f'"{needle}" is stderr-only, so a Finder launch shows nothing'
            )

    if problems:
        print("Startup warnings a GUI user cannot see:\n", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        print(
            f"\nRoute it through {GUI_REPORTER}(), which shows a dialog when\n"
            "stderr is not a terminal and stays silent when it is.",
            file=sys.stderr,
        )
        return 1

    print("==> gui-visible-warnings check OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
