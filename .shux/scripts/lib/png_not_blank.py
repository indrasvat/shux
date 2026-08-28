#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = ["pillow", "numpy"]
# ///
"""Refuse to let a blank PNG stand in as evidence.

`pixel_verify.py` answers "are these two images the same?". Two blank images
are the same, so a rasterizer regression that renders nothing on both sides of
an A/B produces a confident `status: pass` that means nothing. This answers the
other half: "is there anything in this image at all?"

    png_not_blank.py a.png b.png --min-colors 8 --min-ink-ratio 0.01

Fails, loudly and non-zero, if any image has fewer than `--min-colors` distinct
colours or less than `--min-ink-ratio` of its pixels differing from its own
most common colour. A missing file is a failure, not a skip.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
from PIL import Image


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("images", nargs="+", type=Path)
    ap.add_argument("--min-colors", type=int, default=8)
    ap.add_argument("--min-ink-ratio", type=float, default=0.01)
    args = ap.parse_args()

    failed = False
    for path in args.images:
        if not path.exists():
            print(f"    FAIL — {path} does not exist")
            failed = True
            continue
        arr = np.asarray(Image.open(path).convert("RGB")).reshape(-1, 3)
        if arr.size == 0:
            print(f"    FAIL — {path} has no pixels")
            failed = True
            continue
        colors, counts = np.unique(arr, axis=0, return_counts=True)
        n_colors = len(colors)
        # "Ink" is everything that is not the image's own background, whatever
        # that happens to be -- a light theme is not blank just because its
        # dominant colour is white.
        ink_ratio = 1.0 - (counts.max() / len(arr))
        ok = n_colors >= args.min_colors and ink_ratio >= args.min_ink_ratio
        status = "ok  " if ok else "FAIL"
        print(
            f"    {status} {path.name}: {n_colors} distinct colours, "
            f"{ink_ratio:.3%} non-background"
        )
        if not ok:
            failed = True

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
