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
    ap.add_argument(
        "--min-chroma-ratio",
        type=float,
        default=None,
        help=(
            "fraction of pixels that must carry actual colour, i.e. where "
            "max(R,G,B) - min(R,G,B) exceeds --chroma-threshold. Counting "
            "DISTINCT COLOURS does not discriminate colour from greyscale: "
            "antialiasing alone produces hundreds of distinct greys, so a "
            "screen with every colour stripped still scores in the hundreds. "
            "Only chroma separates them."
        ),
    )
    ap.add_argument("--chroma-threshold", type=int, default=16)
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
        chroma_ratio = None
        if args.min_chroma_ratio is not None:
            hi = arr.max(axis=1).astype(np.int16)
            lo = arr.min(axis=1).astype(np.int16)
            chroma_ratio = float((hi - lo > args.chroma_threshold).mean())

        ok = n_colors >= args.min_colors and ink_ratio >= args.min_ink_ratio
        if chroma_ratio is not None and chroma_ratio < args.min_chroma_ratio:
            ok = False
        status = "ok  " if ok else "FAIL"
        detail = (
            f"    {status} {path.name}: {n_colors} distinct colours, "
            f"{ink_ratio:.3%} non-background"
        )
        if chroma_ratio is not None:
            detail += f", {chroma_ratio:.3%} chromatic"
        print(detail)
        if not ok:
            failed = True

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
