#!/usr/bin/env python3
"""Build the kitty-graphics bytes the GUI-terminal rig's self-test injects.

This is issue #175's defect in a bottle: an image emitted with a destination box
in CELLS (`c=`/`r=`) is bounded by the pane whatever its natural pixel size, and
one emitted without it is not.

    kitty_image_payload.py --rgb 0,255,135 --px 320x240 --at 20,70 --out over.esc
    kitty_image_payload.py --rgb 0,255,135 --px 320x240 --at 8,6 --cell-box 10x5 \
        --out contained.esc

Raw RGB (`f=24`) rather than PNG, so this needs nothing outside the standard
library: one fewer tool that can be missing from a rig whose job includes failing
honestly when a tool is missing.
"""

from __future__ import annotations

import argparse
import base64
import sys

# The kitty graphics protocol's own limit on the payload of one APC escape.
CHUNK = 4096


def parse_pair(text: str, sep: str, what: str) -> tuple[int, int]:
    parts = text.split(sep)
    if len(parts) != 2:
        raise SystemExit(f"✗ {what}: expected A{sep}B, got {text!r}")
    try:
        return int(parts[0]), int(parts[1])
    except ValueError:
        raise SystemExit(f"✗ {what}: expected integers, got {text!r}") from None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--rgb", required=True, help="fill colour as R,G,B")
    ap.add_argument("--px", required=True, help="source image size as WxH pixels")
    ap.add_argument("--at", required=True, help="cursor position as ROW,COL (1-based)")
    ap.add_argument(
        "--cell-box",
        default=None,
        help=(
            "destination box as COLSxROWS cells. Present: kitty scales the image "
            "into that many cells and it cannot leave them. Absent: kitty draws it "
            "at its natural pixel size from the cursor, which is the defect."
        ),
    )
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rgb = args.rgb.split(",")
    if len(rgb) != 3:
        raise SystemExit(f"✗ --rgb: expected R,G,B, got {args.rgb!r}")
    try:
        colour = bytes(int(c) for c in rgb)
    except ValueError:
        raise SystemExit(f"✗ --rgb: expected integers, got {args.rgb!r}") from None
    if len(colour) != 3 or any(c > 255 for c in colour):
        raise SystemExit(f"✗ --rgb: expected three 0-255 values, got {args.rgb!r}")

    width, height = parse_pair(args.px, "x", "--px")
    row, col = parse_pair(args.at, ",", "--at")
    if width < 1 or height < 1:
        raise SystemExit(f"✗ --px: expected a positive size, got {args.px!r}")
    if row < 1 or col < 1:
        raise SystemExit(f"✗ --at: cursor positions are 1-based, got {args.at!r}")

    # `C=1` keeps the cursor where it was. Without it kitty advances the cursor
    # past the image and SCROLLS the screen to make room, which throws away the
    # very content the frame is supposed to be judged against: measured, an
    # unclamped 320x240 payload near the bottom of the pane scrolled the payload
    # block and the pane's top border clean off the screen, so the rig failed on
    # missing chrome rather than on the overflow it was pointed at.
    control = f"a=T,C=1,f=24,s={width},v={height}"
    if args.cell_box is not None:
        box_cols, box_rows = parse_pair(args.cell_box, "x", "--cell-box")
        if box_cols < 1 or box_rows < 1:
            raise SystemExit(f"✗ --cell-box: expected a positive box, got {args.cell_box!r}")
        control += f",c={box_cols},r={box_rows}"

    payload = base64.standard_b64encode(colour * (width * height))

    out = bytearray()
    out += f"\033[{row};{col}H".encode()
    for start in range(0, len(payload), CHUNK):
        chunk = payload[start : start + CHUNK]
        more = 1 if start + CHUNK < len(payload) else 0
        head = f"{control},m={more}" if start == 0 else f"m={more}"
        out += b"\033_G" + head.encode() + b";" + chunk + b"\033\\"

    with open(args.out, "wb") as fh:
        fh.write(bytes(out))
    print(
        f"✓ {args.out}: {width}x{height} px in {args.rgb} at row {row} col {col}"
        f"{'' if args.cell_box is None else ', cell box ' + args.cell_box}"
        f" ({len(out)} bytes, {len(payload) // CHUNK + 1} chunks)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
