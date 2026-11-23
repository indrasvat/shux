#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = ["pillow", "numpy"]
# ///
"""Judge two real kitty windows attached to one split shux session (#190).

    kitty_two_clients_verdict.py --geometry run.json
    kitty_two_clients_verdict.py --same a.png b.png

`run.json` is written by `.shux/scripts/gui_terminal_two_clients.sh`: a large
window that drives and a small one that views, photographed before and after
the small one attached. Assertions:

  chrome      each window draws an outline with a vertical separator
  unchanged   the large window's panes are pixel-identical before and after
  block       each pane's block is drawn whole in the small window, at its
              pane's top-left, the same size it is in the large window
  contained   no pane's colour is painted outside that pane in either window
  label       the small window's left border holds ` ⤡ W×H` more than the large
              window's, which holds its title alone

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
MARGIN_PX = 2


def load(path: str) -> np.ndarray:
    return np.asarray(Image.open(path).convert("RGB"), dtype=np.int16)


def mask(img: np.ndarray, rgb: list[int]) -> np.ndarray:
    return (np.abs(img - np.array(rgb, dtype=np.int16)) <= TOLERANCE).all(axis=2)


def crop(img: np.ndarray, win: dict) -> np.ndarray:
    return img[win["y"] : win["y"] + win["h"], win["x"] : win["x"] + win["w"]]


def chrome(region: np.ndarray, border: list[int]) -> tuple[int, int, int, int, int] | None:
    """(left, top, right, bottom, separator) of the outline and its split."""
    m = mask(region, border)
    h, w = m.shape
    rows = np.where(m.sum(axis=1) > LINE_COVERAGE * w * 0.5)[0]
    if rows.size < 2:
        return None
    top, bottom = int(rows.min()), int(rows.max())
    span = m[top : bottom + 1]
    cols = np.where(span.sum(axis=0) > LINE_COVERAGE * (bottom - top + 1))[0]
    if cols.size < 3:
        return None
    left, right = int(cols.min()), int(cols.max())
    inner = cols[(cols > left + MARGIN_PX) & (cols < right - MARGIN_PX)]
    if inner.size == 0:
        return None
    return left, top, right, bottom, int(np.median(inner))


def bbox(m: np.ndarray) -> tuple[int, int, int, int] | None:
    ys, xs = np.where(m)
    if ys.size == 0:
        return None
    return int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())


def inside(box: tuple[int, int, int, int], rect: tuple[int, int, int, int]) -> bool:
    return (
        box[0] >= rect[0] - MARGIN_PX
        and box[1] >= rect[1] - MARGIN_PX
        and box[2] <= rect[2] + MARGIN_PX
        and box[3] <= rect[3] + MARGIN_PX
    )


def title_extent(region: np.ndarray, border: list[int], c: tuple) -> int:
    """Pixels from the first to the last cell the left pane's top rule gives up."""
    left, top, _, _, sep = c
    row = mask(region[top : top + 1, left + 1 : sep], border)[0]
    gaps = np.where(~row)[0]
    return 0 if gaps.size == 0 else int(gaps.max() - gaps.min() + 1)


def judge(run: dict) -> list[str]:
    failures: list[str] = []
    before, after = load(run["before"]), load(run["after"])
    border = run["border_rgb"]
    large_before, large, small = crop(before, run["large"]), crop(after, run["large"]), crop(after, run["small"])

    cl, cs = chrome(large, border), chrome(small, border)
    if cl is None or cs is None:
        return [f"chrome: no split outline found (large {cl}, small {cs})"]

    # The status bar below the outline changes ("2 attached"); the panes must not.
    cut = cl[3] + 1
    changed = int((large_before[:cut] != large[:cut]).any(axis=2).sum())
    if changed:
        failures.append(f"unchanged: {changed} px of the driving window's panes changed when the viewer attached")

    lblock = bbox(mask(large, run["left_rgb"]))
    if lblock is None:
        return failures + ["block: the large window draws no left block"]
    cell_w = (lblock[2] - lblock[0] + 1) / run["block_cols"]
    cell_h = (lblock[3] - lblock[1] + 1) / run["block_rows"]

    for name, region, c in (("large", large, cl), ("small", small, cs)):
        left, top, right, bottom, sep = c
        panes = {
            "left": (left + 1, top + 1, sep - 1, bottom - 1),
            "right": (sep + 1, top + 1, right - 1, bottom - 1),
        }
        for pane, rect in panes.items():
            box = bbox(mask(region, run[f"{pane}_rgb"]))
            if box is None:
                failures.append(f"block: the {name} window draws no {pane} block")
                continue
            if not inside(box, rect):
                failures.append(f"contained: the {name} window paints the {pane} pane's colour at {box}, outside its pane {rect}")
            width, height = box[2] - box[0] + 1, box[3] - box[1] + 1
            want = (round(cell_w * run["block_cols"]), round(cell_h * run["block_rows"]))
            if abs(width - want[0]) > MARGIN_PX or abs(height - want[1]) > MARGIN_PX:
                failures.append(f"block: the {name} window's {pane} block is {width}x{height} px, not the whole {want[0]}x{want[1]}")
            off = ((box[0] - rect[0]) / cell_w, (box[1] - rect[1]) / cell_h)
            if not all(0 <= o < 1.5 for o in off):
                failures.append(f"block: the {name} window's {pane} block starts {off[0]:.1f},{off[1]:.1f} cells into its pane, not at its top-left")

    # Measured from the rule's first break, inside the corner's cell, so the two
    # windows are compared with each other: the viewer's border holds the
    # driver's title and ` ⤡ W×H` more.
    grid = run["left_grid"]
    label = f" ⤡ {grid['cols']}×{grid['rows']}"
    title = f" {run['title']} "
    big = title_extent(large, border, cl) / cell_w
    little = title_extent(small, border, cs) / cell_w
    if abs(big - len(title)) > 2:
        failures.append(f"label: the large window's left border gives {big:.1f} cells to {title!r}, so it carries more than its title")
    if abs((little - big) - len(label)) > 1:
        failures.append(f"label: the small window's left border gives {little - big:.1f} cells more than the large one's, not the {len(label)} of {label!r}")
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
        print("✓ chrome, unchanged, block, contained, label")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
