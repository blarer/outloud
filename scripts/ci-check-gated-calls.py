#!/usr/bin/env python3
"""Find macOS-gated definitions that ungated code calls.

WHY: a `#[cfg(target_os = "macos")]` function compiles fine on a Mac and
vanishes on every other target. If the call site is not gated the same way,
the build fails only on CI, ten minutes after a push. That happened with
`inject::app_identity`, which broke the Linux, Windows and msrv jobs at
once while `cargo check` on the host stayed green.

The proper check is a cross-compile of the whole workspace. That is not
possible on this machine (ring's build script needs a C toolchain for the
target), so `ci-check-cfg.sh` skips `crates/outloud` -- which is precisely
where this bug landed.

Test code counts. A `#[cfg(test)] mod tests` is NOT gated by the platform
gate on its enclosing file's functions, so a test that calls a macOS-only
helper compiles on a Mac and breaks the Linux job like any other caller.
This script missed one of those, because it skipped the whole file a gated
function was defined in.

Reading the source is the fallback. It is not a type checker and does not
pretend to be: it reports a *suspicion*, with file:line, that a human or a
follow-up CI run resolves. False positives are possible (a caller inside a
macro, a name shared with an ungated item); silence is not proof.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# `#[cfg(any(target_os = "macos", test))]` compiles in every test build, so
# a test calling it is fine on Linux. Treating those as macOS-only produced
# 38 false reports against correct code -- and the noise is what would have
# buried the one real failure among them.
MACOS_GATE = re.compile(r'#\[cfg\((?!not\()[^)]*target_os\s*=\s*"macos"')
TEST_ESCAPE = re.compile(r'#\[cfg\(any\([^)]*\btest\b')
NOT_MACOS_GATE = re.compile(r'#\[cfg\(not\([^)]*target_os\s*=\s*"macos"')
# Free functions only, at module scope (no leading indentation).
#
# A trait or inherent METHOD is resolved through its receiver's type, so an
# ungated `x.insert(..)` elsewhere in the workspace is a different function
# entirely. Matching those produced 39 false positives from the single name
# `insert`, which is exactly the "cries wolf" failure that makes a checker
# get ignored and then deleted.
ITEM = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-z_][a-z0-9_]*)")


def gated_functions(path: Path) -> dict[str, int]:
    """Function names defined directly under a macOS-only cfg."""
    out: dict[str, int] = {}
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    for i, line in enumerate(lines):
        if (
            not MACOS_GATE.search(line)
            or NOT_MACOS_GATE.search(line)
            or TEST_ESCAPE.search(line)
        ):
            continue
        # The gate applies to the next item; skip intervening attributes
        # and doc comments.
        j = i + 1
        while j < len(lines) and (
            lines[j].lstrip().startswith("#[") or lines[j].lstrip().startswith("///")
        ):
            j += 1
        if j < len(lines):
            m = ITEM.match(lines[j])
            if m:
                out[m.group(1)] = j + 1
    return out


def defined_more_than_once(path: Path) -> set[str]:
    """Names with several cfg-gated definitions in one file.

    A function written once per platform (`overlay_main` has a macOS, a
    Windows, and a catch-all `cfg(not(any(...)))` variant) resolves on every
    target, so an ungated call is CORRECT.

    Counting DEFINITIONS rather than pattern-matching the gates is what makes
    this robust: an earlier version looked only for `cfg(not(macos))` and
    still reported all three `overlay_main` call sites, because the catch-all
    variant is spelled `not(any(macos, windows))`. Enumerating the ways a
    gate can be written is a losing game; counting definitions is not.
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    counts: dict[str, int] = {}
    for line in text.splitlines():
        m = ITEM.match(line.lstrip())
        if m:
            counts[m.group(1)] = counts.get(m.group(1), 0) + 1
    return {name for name, n in counts.items() if n > 1}


# Braces inside string, char, and comment literals are not code structure.
# Counting them made one gated test whose body contains `{` in a string
# swallow every line to end of file as "gated", which silently disabled this
# check for the whole tail of inject.rs -- including the ungated test call
# that broke the Linux build and prompted this fix.
_STRING_OR_COMMENT = re.compile(
    r'"(?:[^"\\]|\\.)*"'  # double-quoted string, escapes honoured
    r"|'(?:[^'\\]|\\.)'"  # char literal
    r"|r#*\"(?:.|\n)*?\"#*"  # raw string
    r"|//.*$"  # line comment
)


def _code_only(line: str) -> str:
    """`line` with string, char, and comment contents blanked out."""
    return _STRING_OR_COMMENT.sub(lambda m: " " * len(m.group(0)), line)


def gated_line_numbers(path: Path) -> set[int]:
    """Lines inside a `#[cfg(target_os = "macos")] { ... }` block or a
    `#[cfg(...)] mod`/`fn` body, approximated by brace depth.

    Approximate on purpose (see module docstring), but the approximation
    must fail SAFE: over-counting hides real bugs, which is worse than a
    false positive a human dismisses in ten seconds.
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    inside: set[int] = set()
    i = 0
    while i < len(lines):
        if (
            MACOS_GATE.search(lines[i])
            and not NOT_MACOS_GATE.search(lines[i])
            and not TEST_ESCAPE.search(lines[i])
        ):
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
                # Ran off the end without closing: the brace counting is
                # wrong, so trusting it would blind the check for the rest
                # of the file. Protect nothing rather than everything.
                for k in range(i, len(lines)):
                    inside.discard(k + 1)
                inside.add(i + 1)
            i = j
        i += 1
    return inside


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    sources = sorted(root.glob("crates/*/src/**/*.rs"))

    # Every macOS-gated function in the workspace, by name.
    gated: dict[str, tuple[Path, int]] = {}
    for path in sources:
        for name, line in gated_functions(path).items():
            gated[name] = (path, line)

    # Names that also exist on non-macOS targets are not a problem.
    twins: set[str] = set()
    for path in sources:
        twins |= defined_more_than_once(path)
    for name in twins:
        gated.pop(name, None)

    if not gated:
        print("    no macOS-gated functions found")
        return 0

    problems: list[str] = []
    for path in sources:
        text = path.read_text(encoding="utf-8", errors="replace")
        lines = text.splitlines()
        protected = gated_line_numbers(path)
        for n, line in enumerate(lines, start=1):
            if n in protected or line.lstrip().startswith("//"):
                continue
            for name, (decl_path, decl_line) in gated.items():
                # Skip the DECLARATION line only, not the whole file.
                #
                # This used to skip every line of the defining file, on the
                # theory that a file defining a gated function is aware of
                # the gate. It is not: `#[cfg(test)] mod tests` in that same
                # file compiles on every target, so a test calling the gated
                # function built fine on macOS and failed the Linux job.
                # That is exactly the bug this script exists to catch, and
                # it was blind to it in the one file most likely to hit it.
                if decl_path == path and n == decl_line:
                    continue
                # A method call (`.name(`) cannot be this free function.
                if re.search(rf"(?<![.\w]){re.escape(name)}\s*\(", line):
                    rel = path.relative_to(root)
                    decl_rel = decl_path.relative_to(root)
                    problems.append(
                        f"{rel}:{n} calls `{name}`, which is macOS-gated at "
                        f"{decl_rel}:{decl_line}"
                    )

    if problems:
        print("FAIL: macOS-gated function called from ungated code:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        print(
            "\nThis compiles on a Mac and fails on every other target.\n"
            "Either gate the call site the same way, or remove the gate from\n"
            "the definition and make its body handle non-macOS.",
            file=sys.stderr,
        )
        return 1

    print(f"    {len(gated)} macOS-gated fns, no ungated callers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
