#!/usr/bin/env python3
"""Fail if any dependency declares a rust-version above our MSRV.

WHY THIS EXISTS, and why the `msrv` CI job is not enough on its own:

The `msrv` job runs `cargo +1.86 build --workspace --locked` on Linux, which
is the authoritative check and catches everything this script does. But it
catches it *after a push*, costs a full toolchain install plus a cold build,
and reports the failure as a wall of cargo output. Every contributor hits it
the same way: run `cargo update`, get a green local build on macOS, push, and
learn twenty minutes later that a patch release of a crate nobody named
raised its own `rust-version`.

This script answers the same question from the lockfile in about a second,
with no MSRV toolchain installed, and names the offending crates directly.
It is the cheap guard; the build is the real one.

It also covers a gap the local build cannot: platform-gated dependencies. The
`zbus` tree reaches the graph only on Linux, so `cargo build` on a Mac never
compiles it and never sees its MSRV. We therefore check each shipped target's
graph, not the host's.

Features: default only, deliberately. `--all-features` pulls in
`whisper-rs-sys`, which declares 1.88 and builds whisper.cpp from source
through cmake. The `msrv` job does not build it either, so flagging it here
would report a failure that no CI job actually enforces.

Usage:
    scripts/ci-check-msrv-graph.py            # reads MSRV from ci.yml
    scripts/ci-check-msrv-graph.py 1.86.0     # or state it explicitly
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Every target the project ships, matching deny.toml's list. A dependency that
# is MSRV-incompatible only on Linux is still a broken promise to a Debian
# packager, and a macOS-only check would never see it.
TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
]


def parse_version(text: str) -> tuple[int, ...]:
    """`"1.86.0"` -> `(1, 86, 0)`. Missing components read as zero.

    Cargo accepts a bare `1.87` as a `rust-version`, so a plain string
    comparison would order "1.9" after "1.86". Comparing integer tuples is
    the only thing that gets that right.
    """
    parts = [int(p) for p in re.findall(r"\d+", text)[:3]]
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts)


def msrv_from_workflow() -> str:
    """Read the MSRV out of ci.yml, so this script cannot disagree with CI.

    Hardcoding the version here would create a third place the number lives
    (after ci.yml and docs/build-and-release.md), and the docs were already
    out of sync with CI by a full minor version for months before 2026-10-30.
    """
    workflow = ROOT / ".github/workflows/ci.yml"
    match = re.search(r'MSRV:\s*"([\d.]+)"', workflow.read_text())
    if not match:
        sys.exit(
            f"could not find an `MSRV: \"x.y.z\"` line in {workflow}.\n"
            "If the msrv job was renamed or restructured, update this script "
            "to read the new location rather than hardcoding a version."
        )
    return match.group(1)


def offenders(target: str, limit: tuple[int, ...]) -> list[tuple[str, str, str]]:
    """Crates in `target`'s default-feature graph needing more than `limit`."""
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            target,
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        sys.exit(f"cargo metadata failed for {target}:\n{result.stderr}")

    found = []
    for pkg in json.loads(result.stdout)["packages"]:
        declared = pkg.get("rust_version")
        if declared and parse_version(declared) > limit:
            found.append((pkg["name"], pkg["version"], declared))
    return found


def main() -> int:
    msrv = sys.argv[1] if len(sys.argv) > 1 else msrv_from_workflow()
    limit = parse_version(msrv)
    print(f"==> MSRV {msrv}, checking {len(TARGETS)} shipped targets")

    # Keyed by crate so one offender pulled in by six targets reports once,
    # with the targets that reach it. A per-target list would print the zbus
    # tree four times and bury the one crate that matters.
    all_bad: dict[tuple[str, str, str], list[str]] = {}
    for target in TARGETS:
        for entry in offenders(target, limit):
            all_bad.setdefault(entry, []).append(target)

    if not all_bad:
        print(f"OK: every dependency builds on Rust {msrv}")
        return 0

    print(f"\nFAIL: {len(all_bad)} crate(s) require more than Rust {msrv}:\n")
    for (name, version, needs), targets in sorted(all_bad.items()):
        where = "all targets" if len(targets) == len(TARGETS) else ", ".join(targets)
        print(f"  {name} {version} needs {needs}  [{where}]")

    print(
        "\nFix one of these ways, in order of preference:\n"
        "  1. Pin the crate back to a release that still supports the MSRV:\n"
        "       cargo update <crate> --precise <version>\n"
        "     Then record it in docs/build-and-release.md under the pinned\n"
        "     dependencies table, with the reason. An unexplained pin gets\n"
        "     undone by the next person running `cargo update`.\n"
        "  2. Drop the dependency, if it arrived with a feature we can live\n"
        "     without.\n"
        "  3. Raise the MSRV. This is a compatibility break for downstream\n"
        "     packagers: it needs a CHANGELOG entry and edits to ci.yml,\n"
        "     docs/build-and-release.md, and rust-toolchain.toml's comment.\n"
        "     Doing it for a transitive dependency of an optional feature is\n"
        "     the case the MSRV policy explicitly argues against.\n"
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
