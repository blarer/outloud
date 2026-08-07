#!/usr/bin/env python3
"""Fail if a rasterized icon is blank.

Quick Look writes a PNG even when it fails to PARSE the SVG: near-blank
canvas, exit status 0, plausible file size. That shipped an empty app icon,
and every downstream check agreed with it because they were all looking at
the same blank PNG. `[ -f "$master" ]` is not evidence of an image.

The cause that time was a double hyphen inside the leading XML comment,
which XML forbids (it came from a `cargo run --bin` example in the comment).
Quick Look reports nothing and renders nothing.

The metric is dark-pixel COVERAGE, not luma range.

Range was the obvious choice and it does not work: a blank Quick Look
canvas still carries anti-aliased artifacts (a stray glyph, a border) that
push max-minus-min to 169 on a completely empty icon, comfortably past any
threshold a real icon also clears. Verified by measuring both.

Coverage separates them cleanly. This mark is a light skull on a dark
rounded field that fills the canvas, so a correct rasterization is mostly
DARK: roughly 60-80% of pixels below mid-grey. A blank render is
overwhelmingly white, under 10%.

Usage: ci-check-icon-not-blank.py <png>   (exit 1 when blank)
"""

import struct
import subprocess
import sys
import zlib

# Well below a real icon (measured ~0.75) and well above a blank one
# (measured ~0.02).
MIN_DARK_FRACTION = 0.25


def dark_fraction(png_path: str) -> float:
    small = "/tmp/_iconcheck_small.png"
    subprocess.run(
        ["sips", "-s", "format", "png", "-Z", "64", png_path, "--out", small],
        capture_output=True,
        check=False,
    )
    d = open(small, "rb").read()
    pos, w, h, idat, ct = 8, 0, 0, b"", 0
    while pos < len(d):
        ln = struct.unpack(">I", d[pos : pos + 4])[0]
        typ = d[pos + 4 : pos + 8]
        body = d[pos + 8 : pos + 8 + ln]
        if typ == b"IHDR":
            w, h, _bd, ct = struct.unpack(">IIBB", body[:10])
        elif typ == b"IDAT":
            idat += body
        pos += 12 + ln

    raw = zlib.decompress(idat)
    ch = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ct]
    stride = w * ch
    rows, prev, i = [], bytearray(stride), 0
    for _y in range(h):
        f = raw[i]
        i += 1
        line = bytearray(raw[i : i + stride])
        i += stride
        for x in range(stride):
            a = line[x - ch] if x >= ch else 0
            b = prev[x]
            c = prev[x - ch] if x >= ch else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 255
        rows.append(bytes(line))
        prev = line

    vals = []
    for y in range(h):
        for x in range(w):
            px = rows[y][x * ch : x * ch + ch]
            r, g, b = (px[0], px[1], px[2]) if ch >= 3 else (px[0],) * 3
            # Composite over the dark field: a fully transparent pixel is
            # NOT a bright one, and treating it as bright is what made an
            # earlier version of this check pass on a blank icon.
            alpha = px[3] / 255 if ch == 4 else 1.0
            vals.append((0.299 * r + 0.587 * g + 0.114 * b) * alpha)
    dark = sum(1 for v in vals if v < 128)
    return dark / len(vals)


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: ci-check-icon-not-blank.py <png>", file=sys.stderr)
        return 2
    frac = dark_fraction(sys.argv[1])
    if frac < MIN_DARK_FRACTION:
        print(
            f"FAIL: the rasterized icon is nearly blank "
            f"({frac:.0%} dark, expected at least {MIN_DARK_FRACTION:.0%}).\n"
            "\n"
            "qlmanage wrote a PNG but rendered nothing. The usual cause is\n"
            "invalid XML in docs/assets/logo.svg, most often a double hyphen\n"
            "inside the leading comment, which XML forbids and Quick Look\n"
            "ignores silently.",
            file=sys.stderr,
        )
        return 1
    print(f"    icon rasterized ({frac:.0%} of the canvas is the dark field)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
