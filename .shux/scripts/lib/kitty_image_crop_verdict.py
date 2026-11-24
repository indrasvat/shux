#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = ["pillow", "numpy"]
# ///
"""Judge a real kitty window showing pictures cropped by a pane's scroll (#190).

    kitty_image_crop_verdict.py --geometry run.json
    kitty_image_crop_verdict.py --same a.png b.png

`run.json` is written by `.shux/scripts/gui_terminal_image_crop.sh`: one pane
pinned wider than its rect, holding two banded pictures, photographed whole,
with its slice scrolled `offset` columns right, and scrolled back. Assertions:

  chrome     the window draws one pane outline
  whole      each picture's bands, in order and at their widths, from its cell
  cropped    each picture's bands that start right of the slice's first
             column, from the pane's left edge: the band cut by that edge at
             the part of its width that is left
  contained  no band colour outside the pane in any frame: not on the border,
             not on the status bar
  restored   the pane draws exactly as it did before the scroll

Exit 0 only if every assertion passes; 1 when one fails; 2 on unusable input.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image

TOLERANCE = 8
LINE_COVERAGE = 0.5
EDGE_PX = 3


def load(path: str) -> np.ndarray:
    return np.asarray(Image.open(path).convert("RGB"), dtype=np.int16)


def mask(img: np.ndarray, rgb: list[int]) -> np.ndarray:
    return (np.abs(img - np.array(rgb, dtype=np.int16)) <= TOLERANCE).all(axis=2)


def outline(region: np.ndarray, border: list[int]) -> tuple[int, int, int, int] | None:
    m = mask(region, border)
    h, w = m.shape
    rows = np.where(m.sum(axis=1) > LINE_COVERAGE * w * 0.5)[0]
    if rows.size < 2:
        return None
    top, bottom = int(rows.min()), int(rows.max())
    cols = np.where(m[top : bottom + 1].sum(axis=0) > LINE_COVERAGE * (bottom - top + 1))[0]
    if cols.size < 2:
        return None
    return int(cols.min()), top, int(cols.max()), bottom


def runs(line: np.ndarray, bands: list[list[int]]) -> list[tuple[int, int, int]]:
    """(band, first x, length) of each run of one band's colour along `line`."""
    which = np.full(line.shape[0], -1)
    for i, rgb in enumerate(bands):
        which[mask(line[None], rgb)[0]] = i
    out: list[tuple[int, int, int]] = []
    for x, b in enumerate(which):
        if b < 0:
            continue
        if out and out[-1][0] == b and out[-1][1] + out[-1][2] == x:
            out[-1] = (b, out[-1][1], out[-1][2] + 1)
        else:
            out.append((int(b), x, 1))
    return out


def expected(pic: dict, first: int, x0: float, cell_w: float) -> list[tuple[int, float, float]]:
    """(band, first x, width) of each band a slice from grid column `first` shows."""
    per = pic["cells_per_band"]
    col = pic["at"][1]
    out = []
    for i in range(len(pic["bands"])):
        lo, hi = max(col + i * per, first), col + (i + 1) * per
        if hi > lo:
            out.append((i, x0 + (lo - first) * cell_w, (hi - lo) * cell_w))
    return out


def compare(name: str, got: list, want: list) -> list[str]:
    if [b for b, _, _ in got] != [b for b, _, _ in want]:
        return [f"{name}: bands {[b for b, *_ in got]} drawn, not {[b for b, *_ in want]} (runs {got})"]
    bad = []
    for (b, x, n), (_, wx, wn) in zip(got, want):
        if abs(x - wx) > EDGE_PX or abs(n - wn) > EDGE_PX:
            bad.append(f"band {b} at {x}+{n} px, not {wx:.1f}+{wn:.1f}")
    return [f"{name}: {'; '.join(bad)}"] if bad else []


def judge(run: dict) -> list[str]:
    win = run["window"]
    frames = {
        k: load(run[k])[win["y"] : win["y"] + win["h"], win["x"] : win["x"] + win["w"]]
        for k in ("whole", "cropped", "restored")
    }
    box = outline(frames["whole"], run["border_rgb"])
    if box is None:
        return ["chrome: no pane outline in the window"]
    left, top, right, bottom = box
    pics = {"A": run["a"], "B": run["b"]}

    whole = frames["whole"]
    a_mask = np.zeros(whole.shape[:2], dtype=bool)
    for rgb in pics["A"]["bands"]:
        a_mask |= mask(whole, rgb)
    ys, xs = np.where(a_mask)
    if ys.size == 0:
        return ["whole: picture A is not drawn"]
    # The cell, from A's whole drawn extent: kitty filters the scaled bitmap,
    # so a band colour itself stops a pixel short of each picture edge.
    drawn = (np.abs(whole) > TOLERANCE).any(axis=2)
    cy, cx = (int(ys.min()) + int(ys.max())) // 2, (int(xs.min()) + int(xs.max())) // 2
    row = np.where(drawn[cy, left + 1 : right])[0] + left + 1
    row = row[(row >= xs.min() - EDGE_PX) & (row <= xs.max() + EDGE_PX)]
    col = np.where(drawn[top + 1 : bottom, cx])[0] + top + 1
    col = col[(col >= ys.min() - EDGE_PX) & (col <= ys.max() + EDGE_PX)]
    cell_w = (row.max() - row.min() + 1) / (len(pics["A"]["bands"]) * pics["A"]["cells_per_band"])
    cell_h = float(col.max() - col.min() + 1)
    x0 = row.min() - pics["A"]["at"][1] * cell_w
    y0 = col.min() - pics["A"]["at"][0] * cell_h
    failures: list[str] = []
    if not (left < x0 <= left + cell_w and top < y0 <= top + cell_h):
        failures.append(f"chrome: the pane's first cell at {x0:.0f},{y0:.0f} is not just inside its outline {box}")

    for frame, clip in (("whole", 0), ("cropped", run["offset"])):
        img = frames[frame]
        for name, pic in pics.items():
            y = int(y0 + (pic["at"][0] + 0.5) * cell_h)
            got = runs(img[y, left + 1 : right], pic["bands"])
            got = [(b, x + left + 1, n) for b, x, n in got]
            failures += compare(f"{frame}: picture {name}", got, expected(pic, clip, x0, cell_w))

    inside = np.zeros(whole.shape[:2], dtype=bool)
    inside[top + 1 : bottom, left + 1 : right] = True
    for frame, img in frames.items():
        for name, pic in pics.items():
            m = np.zeros(img.shape[:2], dtype=bool)
            for rgb in pic["bands"]:
                m |= mask(img, rgb)
            out = m & ~inside
            if out.any():
                oy, ox = np.where(out)
                failures.append(
                    f"contained: {frame} paints {int(out.sum())} px of picture {name} outside its pane, "
                    f"at x {ox.min()}..{ox.max()}, y {oy.min()}..{oy.max()} (pane {box})"
                )

    diff = (frames["whole"][inside] != frames["restored"][inside]).any(axis=1)
    if diff.any():
        failures.append(f"restored: {int(diff.sum())} px of the pane differ from before the scroll")
    return failures


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--geometry")
    ap.add_argument("--same", nargs=2)
    args = ap.parse_args()
    if args.same:
        a, b = (load(p) for p in args.same)
        return 0 if a.shape == b.shape and (a == b).all() else 1
    if not args.geometry:
        ap.print_usage(sys.stderr)
        return 2
    try:
        run = json.loads(Path(args.geometry).read_text())
    except (OSError, ValueError) as e:
        print(f"unreadable {args.geometry}: {e}", file=sys.stderr)
        return 2
    failures = judge(run)
    for f in failures:
        print(f"✗ {f}")
    if not failures:
        print("✓ chrome, whole, cropped, contained, restored")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
