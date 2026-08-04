#!/usr/bin/env python3
"""Find functions whose only callers are platform-gated.

WHY: a private function called only from `#[cfg(target_os = "macos")]` and
`#[cfg(target_os = "windows")]` code compiles on both of those, and is DEAD
on Linux. CI runs clippy with `-D warnings`, so "function is never used"
there is a build failure, ten minutes after a push.

That is not hypothetical. `resolve_undo` was factored out so macOS and
Windows could share one undo decision, both callers sat behind platform
gates, and the Linux job failed while every check on this machine passed.
`cargo clippy` on a Mac cannot see it, because on a Mac the function IS
used. `--no-default-features` does not reach it either: `crates/outloud`
cannot build that way (see ci-check-cfg.sh), which is exactly the crate
where this happens.

The fix in each case is to make the function `pub` (it is a real part of the
crate's surface, used by whichever backend is compiled) rather than to add a
third cfg gate enumerating the platforms that happen to have a caller today.

Approximate, like ci-check-gated-calls.py, and for the same reason: reading
source is the only option when the toolchain cannot cross-compile. It
reports a suspicion with file:line. False positives are possible; silence is
not proof.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# A private free function at module scope. `pub` ones are exempt: they are
# crate surface, and that is precisely the fix this check recommends.
PRIVATE_FN = re.compile(r"^fn\s+([a-z_][a-z0-9_]*)\s*[(<]")
ANY_CFG_GATE = re.compile(r'#\[cfg\([^)]*target_os\s*=\s*"[a-z]+"')
CFG_TEST = re.compile(r"#\[cfg\(test\)\]|#\[cfg\(any\([^)]*\btest\b")


def test_regions(lines: list[str]) -> set[int]:
    """1-based line numbers inside a `#[cfg(test)]` item.

    A test is not a caller for this purpose. `#[cfg(test)] mod tests` does
    not exist in the build CI lints, so a function called ONLY from tests
    and from platform-gated code is still dead there. Missing this made the
    first version of this script report success against a tree whose Linux
    job was failing, which is worse than having no script.
    """
    inside: set[int] = set()
    i = 0
    while i < len(lines):
        if CFG_TEST.search(lines[i]):
            depth = 0
            started = False
            j = i
            while j < len(lines):
                code = _code_only(lines[j])
                depth += code.count("{") - code.count("}")
                if "{" in code:
                    started = True
                inside.add(j + 1)
                if started and depth <= 0:
                    break
                j += 1
            else:
                for k in range(i, len(lines)):
                    inside.discard(k + 1)
            i = j
        i += 1
    return inside


def gated_regions(lines: list[str]) -> set[int]:
    """1-based line numbers inside any `target_os`-gated item.

    Brace depth, with string and comment contents blanked so a `{` inside a
    literal cannot run the region to end of file. Failing to close means the
    counting is wrong, so the region is discarded rather than trusted: over-
    reporting a region hides callers and produces false alarms.
    """
    inside: set[int] = set()
    i = 0
    while i < len(lines):
        if ANY_CFG_GATE.search(lines[i]) and not CFG_TEST.search(lines[i]):
            depth = 0
            started = False
            j = i
            while j < len(lines):
                code = _code_only(lines[j])
                depth += code.count("{") - code.count("}")
                if "{" in code:
                    started = True
                inside.add(j + 1)
                if started and depth <= 0:
                    break
                j += 1
            else:
                for k in range(i, len(lines)):
                    inside.discard(k + 1)
            i = j
        i += 1
    return inside


_STRING_OR_COMMENT = re.compile(
    r'"(?:[^"\\]|\\.)*"' r"|'(?:[^'\\]|\\.)'" r"|//.*$"
)


def _code_only(line: str) -> str:
    return _STRING_OR_COMMENT.sub(lambda m: " " * len(m.group(0)), line)


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    problems: list[str] = []

    for path in sorted(root.glob("crates/*/src/**/*.rs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        lines = text.splitlines()
        gated = gated_regions(lines)
        in_test = test_regions(lines)

        # Private functions defined OUTSIDE any platform gate. A gated
        # definition is already platform-scoped and is not this bug.
        candidates: dict[str, int] = {}
        for n, line in enumerate(lines, start=1):
            if n in gated or n in in_test:
                continue
            m = PRIVATE_FN.match(line)
            if m:
                candidates[m.group(1)] = n

        if not candidates:
            continue

        for name, decl_line in candidates.items():
            ungated_callers = 0
            gated_callers = 0
            for n, line in enumerate(lines, start=1):
                if n == decl_line or line.lstrip().startswith("//"):
                    continue
                # Tests do not keep a function alive in the build CI lints.
                if n in in_test:
                    continue
                if not re.search(rf"(?<![.\w]){re.escape(name)}\s*\(", line):
                    continue
                if n in gated:
                    gated_callers += 1
                else:
                    ungated_callers += 1

            # Gated callers but no ungated one: dead wherever no gate matches.
            if gated_callers and not ungated_callers:
                rel = path.relative_to(root)
                problems.append(
                    f"{rel}:{decl_line} `{name}` is private and every caller "
                    f"is platform-gated ({gated_callers} of them)"
                )

    if problems:
        print(
            "FAIL: private function reachable only from platform-gated code:",
            file=sys.stderr,
        )
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        print(
            "\nThis is dead code on any platform whose gate does not match, and\n"
            "CI runs clippy with -D warnings, so it fails the build there.\n"
            "Make it `pub` (it is real crate surface for whichever backend is\n"
            "compiled) rather than adding another cfg gate listing today's\n"
            "platforms.",
            file=sys.stderr,
        )
        return 1

    print("    no privately-gated-only functions")
    return 0


if __name__ == "__main__":
    sys.exit(main())
