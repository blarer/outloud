#!/usr/bin/env python3
"""Rename Hexavoice -> OutLoud, protecting the competitor's name.

Same shape as the previous rename, and the same reason for the masking pass:
`Aqua Voice` is a DIFFERENT product our docs analyse by name. It survived the
last rename because it was masked; it must survive this one too.

Also protects the strings that are deliberately frozen history rather than
current naming: the legacy config directory list, the `aqua-speech-helper`
filename that crates/asr looks for literally, the `~/.aqua-oss/models` cache
path, and the old bundle ids that uninstall/upgrade paths must still clear.
"""
import pathlib
import re
import sys

SENTINEL = "\x00KEEP\x00"

# Longest first so a prefix cannot match before the full string.
PROTECTED = [
    "Aqua Voice's",
    "Aqua Voice",
    "aquavoice.com",
    "withaqua.com",
    "aquavoice",
    # Frozen history: renaming these breaks upgraders or the helper lookup.
    "aqua-speech-helper",
    "aqua-oss",
    "dev.aquaoss.aquad",
    "dev.aquaoss.doctor",
    "dev.aquaoss.spike",
    "dev.hexavoice.hexad",
    "dev.hexavoice.doctor",
    "dev.hexavoice.spike",
    'LEGACY_DIRS: &[&str] = &["hexavoice", "aqua"]',
    'LEGACY_ENV_PREFIX: &str = "AQUA_"',
]

RULES = [
    ("bundle-hexad-macos.sh", "bundle-outloud-macos.sh"),
    ("crates/hexad", "crates/outloud"),
    ("Hexavoice.app/Contents/MacOS/Hexavoice", "OutLoud.app/Contents/MacOS/OutLoud"),
    ("dist/Hexavoice.app", "dist/OutLoud.app"),
    ("Hexavoice.app", "OutLoud.app"),
    ("HexavoiceDoctor", "OutLoudDoctor"),
    ("HexavoiceSpike", "OutLoudSpike"),
    ("hexavoice-spike", "outloud-spike"),
    ("HEXA_SPIKE_LOG", "OUTLOUD_SPIKE_LOG"),
    ("HEXA_KEEP_TCC", "OUTLOUD_KEEP_TCC"),
    ("HEXA_", "OUTLOUD_"),
    ("hexavoice", "outloud"),
    ("Hexavoice", "OutLoud"),
    ("hexad", "outloud"),
    (r"\bhexa\b", "outloud"),
]


def convert(text: str) -> tuple[str, int]:
    masked = text
    kept = 0
    for name in PROTECTED:
        n = masked.count(name)
        if n:
            masked = masked.replace(name, f"{SENTINEL}{name}{SENTINEL}")
            kept += n

    parts = masked.split(SENTINEL)
    for i in range(0, len(parts), 2):
        for old, new in RULES:
            if old.startswith("\\b"):
                parts[i] = re.sub(old, new, parts[i])
            else:
                parts[i] = parts[i].replace(old, new)
    return "".join(parts), kept


def main(paths: list[str]) -> int:
    changed = kept_total = 0
    for name in paths:
        path = pathlib.Path(name)
        original = path.read_text()
        updated, kept = convert(original)
        kept_total += kept
        if updated != original:
            path.write_text(updated)
            changed += 1
    print(f"rewrote {changed} files; protected {kept_total} frozen strings")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
