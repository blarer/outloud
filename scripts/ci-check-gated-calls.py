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

Reading the source is the fallback. It is not a type checker and does not
pretend to be: it reports a *suspicion*, with file:line, that a human or a
follow-up CI run resolves. False positives are possible (a caller inside a
macro, a name shared with an ungated item); silence is not proof.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

MACOS_GATE = re.compile(r'#\[cfg\((?!not\()[^)]*target_os\s*=\s*"macos"')
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
        if not MACOS_GATE.search(line) or NOT_MACOS_GATE.search(line):
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


def gated_line_numbers(path: Path) -> set[int]:
    """Lines inside a `#[cfg(target_os = "macos")] { ... }` block or a
    `#[cfg(...)] mod`/`fn` body, approximated by brace depth."""
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    inside: set[int] = set()
    i = 0
    while i < len(lines):
        if MACOS_GATE.search(lines[i]) and not NOT_MACOS_GATE.search(lines[i]):
            depth = 0
            started = False
            j = i
            while j < len(lines):
                depth += lines[j].count("{") - lines[j].count("}")
                if "{" in lines[j]:
                    started = True
                inside.add(j + 1)
                if started and depth <= 0:
                    break
                j += 1
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

    if not gated:
        print("    no macOS-gated functions found")
        return 0

    problems: list[str] = []
    for path in sources:
        text = path.read_text(encoding="utf-8", errors="replace")
        lines = text.splitlines()
        protected = gated_line_numbers(path)
        own = set(gated_functions(path))
        for n, line in enumerate(lines, start=1):
            if n in protected or line.lstrip().startswith("//"):
                continue
            for name, (decl_path, decl_line) in gated.items():
                if name in own and decl_path == path:
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
