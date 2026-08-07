#!/usr/bin/env python3
"""Run the publish job's shell steps locally, against a staged artifacts dir.

WHY: the publish job has never executed. It is gated on
`startsWith(github.ref, 'refs/tags/v')`, so every workflow_dispatch run
skips it, and the last tagged run was cancelled at the compliance gate
before reaching it. Its steps -- attaching the installer, refusing a
branch-pinned URL, generating checksums -- were therefore unproven code on
the one path that produces what strangers download.

This stages an artifacts/ directory the way download-artifact would, then
runs the same shell those steps run, and asserts the outcome both ways:
clean input publishes, a branch-pinned installer is rejected.

Usage: scripts/ci-check-publish-steps.py
Exit: 0 clean, 1 if a step does not behave as the workflow expects.
"""

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INSTALLER = ROOT / "scripts" / "Install-OutLoud.command"

# The exact grep from the workflow's guard step. Kept as one string so a
# change to the workflow that is not mirrored here is visible in review.
GUARD_PATTERN = r'raw\.githubusercontent\.com/[^"]*refs/heads/'


def guard_rejects(path: Path) -> bool:
    """Whether the workflow's guard would block this installer."""
    return re.search(GUARD_PATTERN, path.read_text(encoding="utf-8")) is not None


def stage(tmp: Path, installer: Path) -> Path:
    """Build an artifacts/ dir like actions/download-artifact produces."""
    artifacts = tmp / "artifacts"
    (artifacts / "macos-universal").mkdir(parents=True)
    (artifacts / "macos-universal" / "OutLoud.dmg").write_text("fake dmg\n")
    shutil.copy2(installer, artifacts / installer.name)
    return artifacts


def main() -> int:
    if not INSTALLER.is_file():
        print(f"missing {INSTALLER.relative_to(ROOT)}", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)

        # 1. The real installer must pass the guard.
        artifacts = stage(tmp, INSTALLER)
        staged = artifacts / INSTALLER.name
        if guard_rejects(staged):
            print("the current installer would be REJECTED by the release guard", file=sys.stderr)
            print("  " + GUARD_PATTERN, file=sys.stderr)
            return 1
        print("  ok  the current installer passes the release guard")

        # 2. Checksums must cover the installer, or a tampered asset is
        #    indistinguishable from the real one.
        result = subprocess.run(
            ["find", ".", "-type", "f", "!", "-name", "SHA256SUMS",
             "-exec", "shasum", "-a", "256", "{}", ";"],
            cwd=artifacts, capture_output=True, text=True, check=True,
        )
        if INSTALLER.name not in result.stdout:
            print("the installer is not covered by SHA256SUMS", file=sys.stderr)
            return 1
        print("  ok  the installer is covered by the release checksums")

    # 3. The guard must actually reject the defect it exists for. A guard
    #    that cannot fail is decoration.
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        broken = tmp / INSTALLER.name
        broken.write_text(
            INSTALLER.read_text(encoding="utf-8").replace(
                "/blarer/outloud/main/scripts",
                "/blarer/outloud/refs/heads/overlay/cat-mascot/scripts",
            ),
            encoding="utf-8",
        )
        if not guard_rejects(broken):
            print(
                "the release guard did NOT reject a branch-pinned installer;\n"
                "it would have shipped the exact defect that broke the last release",
                file=sys.stderr,
            )
            return 1
        print("  ok  the release guard rejects a branch-pinned installer")

    print("==> publish-step check OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
