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


# A definition gated as "not this platform", e.g. #[cfg(not(target_os = "macos"))].
NEGATIVE_GATE = re.compile(r'#\[cfg\(not\([^)]*target_os\s*=\s*"([a-z0-9_]+)"')
# A caller gated as "is this platform", e.g. #[cfg(all(target_os = "windows", ...))].
POSITIVE_GATE = re.compile(r'#\[cfg\((?!not\()[^)]*target_os\s*=\s*"([a-z0-9_]+)"')


def gate_stack_map(lines: list[str]) -> dict[int, tuple]:
    """1-based line number -> the tuple of cfg attribute lines enclosing it.

    Deliberately keeps the raw attribute text rather than trying to
    interpret it. Deciding whether one cfg implies another
    (`all(windows, display)` implies `windows`) is a solver, and every
    approximation of it I tried produced false reports on healthy code:
    first "is it gated at all" (36 wrong), then nesting depth (4 wrong).
    A checker that cries wolf gets deleted, and this one already has a
    precedent in ci-check-gated-calls.py's own docstring saying so.

    The caller decides what to do with the raw stacks, and only acts on the
    one comparison that needs no implication reasoning.
    """
    out: dict[int, tuple] = {}
    stack: list[tuple[str, int]] = []
    brace = 0
    pending: str | None = None
    for n, raw in enumerate(lines, start=1):
        code = _code_only(raw)
        if ANY_CFG_GATE.search(raw) and not CFG_TEST.search(raw):
            pending = raw.strip()
        # The gate's line is not the line with the brace: a multi-line
        # signature puts them six lines apart, and losing `pending` in
        # between made a #[cfg(target_os = "macos")] function look ungated.
        # `pending` therefore survives until a brace actually opens.
        out[n] = tuple(c for c, _ in stack) + ((pending,) if pending else ())
        opens = code.count("{")
        closes = code.count("}")
        if pending and opens:
            stack.append((pending, brace))
            pending = None
            out[n] = tuple(c for c, _ in stack)
        brace += opens - closes
        while stack and brace <= stack[-1][1]:
            stack.pop()
    return out


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
        gates = gate_stack_map(lines)

        # Private functions, whether or not they sit under a platform gate.
        #
        # An earlier version skipped gated definitions, reasoning that they
        # are "already platform-scoped". That misses the nested case, which
        # is the one that actually happens: a `cfg(not(macos))` function
        # whose only caller lives in a `cfg(all(windows, display))` block
        # inside it is dead on Linux, which compiles the outer gate and not
        # the inner one. `write_literal_via_tiers` shipped exactly that and
        # broke the Linux job while this script reported clean.
        #
        # A definition line is not itself a caller, so including gated
        # definitions costs nothing.
        candidates: dict[str, int] = {}
        for n, line in enumerate(lines, start=1):
            if n in in_test:
                continue
            m = PRIVATE_FN.match(line.lstrip()) if line[:1].isspace() else PRIVATE_FN.match(line)
            if not m:
                continue
            # Trait methods (`fn drop(&mut self)` in `impl Drop`) are invoked
            # by the compiler and never by name, so "no callers" says
            # nothing about them.
            if "self" in line:
                continue
            candidates[m.group(1)] = n

        if not candidates:
            continue

        for name, decl_line in candidates.items():
            decl_stack = gates.get(decl_line, ())
            # Only judge definitions gated as "NOT platform X". Anything
            # else needs cfg implication reasoning this script cannot do.
            decl_negative = next(
                (g for g in decl_stack if NEGATIVE_GATE.search(g)), None
            )
            if decl_negative is not None:
                # "not X": a caller gated to any platform other than X
                # leaves builds that are neither holding a dead definition.
                excluded = NEGATIVE_GATE.search(decl_negative).group(1)
            elif not decl_stack:
                # UNGATED definition: every platform compiles it, so a
                # caller under ANY positive platform gate leaves the rest
                # of them with no caller at all. This is the shape
                # `resolve_undo` had when it broke the Linux job: shared by
                # the macOS and Windows backends, gated to neither.
                excluded = None
            else:
                # Positively gated definitions need cfg implication
                # reasoning to judge; out of scope, see the module
                # docstring.
                continue
            reachable_callers = 0
            deeper_callers = 0
            for n, line in enumerate(lines, start=1):
                if n == decl_line or line.lstrip().startswith("//"):
                    continue
                # Tests do not keep a function alive in the build CI lints.
                if n in in_test:
                    continue
                if not re.search(rf"(?<![.\w]){re.escape(name)}\s*\(", line):
                    continue
                caller_stack = gates.get(n, ())
                # A caller under a POSITIVE gate for some other platform
                # disappears on every build that is neither that platform
                # nor the excluded one. `not(macos)` + caller in `windows`
                # leaves Linux holding the definition alone.
                positive = next(
                    (
                        m.group(1)
                        for g in caller_stack
                        for m in [POSITIVE_GATE.search(g)]
                        if m and (excluded is None or m.group(1) != excluded)
                    ),
                    None,
                )
                if positive is not None:
                    deeper_callers += 1
                else:
                    reachable_callers += 1

            if deeper_callers and not reachable_callers:
                rel = path.relative_to(root)
                problems.append(
                    f"{rel}:{decl_line} `{name}` is private and every caller "
                    f"({deeper_callers}) is gated to another platform, so a "
                    f"build that is neither has the definition and no caller"
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
