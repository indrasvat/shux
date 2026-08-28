#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = ["pillow", "numpy"]
# ///
"""Judge what a real GUI terminal painted from shux's output.

    kitty_frame_verdict.py --geometry run.json

`run.json` is written by `.shux/scripts/gui_terminal_check.sh`, whose `--help`
and `docs/agents/visual-testing.md` say why this rig exists. Per frame:

  chrome               the pane border is a rectangle and the status bar is below it
  clipped              that rectangle is not flush with the frame edge, which would
                       mean the window ran off the screen and the capture is missing
                       the region an overflow lands in
  grid                 the size shux TOLD the pane matches the grid the emulator is
                       drawing, to under one cell of slack in the window
  containment:foreign  nothing but shux's chrome, in any colour, is outside the pane
  containment:image    a known payload is not outside the pane, under its own name so
                       the self-test can require THIS failure and not just a failure
  content:image        an injected payload that was promised is actually there
  content:block        the workload's block covers the area its cell rect implies
  probe                the truecolor, indexed and basic colour probes all rendered
  crosspath            the block sits at the same cells here as in `pane capture`

Exit 0 only if every frame of every phase passes every assertion. Zero frames is
a failure, not an empty pass.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from PIL import Image

# Solid terminal fills are emitted exactly: measured over a real kitty frame,
# matching the border colour at tolerance 0 found 9251 pixels and at tolerance
# 64 found 10224 — the extra 10% is glyph antialiasing, not fill drift. 8 is
# comfortably above zero drift and far below the distance to any other colour in
# the fiducial palette (the closest pair is 0x87 apart per channel).
DEFAULT_TOLERANCE = 8

# A border line is 1-2 px of ink inside a cell that is ~10x19 px, so a row of the
# rectangle's horizontal rule carries ink across most of the width while a row of
# glyphs (the pane title sits ON the top border row, in the border colour) does
# not. Half the span separates them with room to spare.
LINE_COVERAGE = 0.5

# Half a cell of slack on each side of the border rectangle before a pixel counts
# as "outside". The border ink sits at the CENTRE of its cell, so the true
# interior edge is half a cell out from the measured line; 2 px on top of that
# absorbs antialiasing. The defect this catches overflowed by 160-252 px.
EDGE_MARGIN_PX = 2

# How far a pixel outside the pane may sit from the nearest blend of two chrome
# colours before it counts as foreign. Measured across real captures: a correct
# frame has zero foreign pixels at 8 and still zero at 40, so this sits in the
# middle of a wide flat region rather than on a cliff.
FOREIGN_TOLERANCE = 24

# What fraction of a payload block's geometric area must actually be painted.
# Measured 0.90 on real frames — the missing tenth is the `~` glyph ink drawn in
# black over the background run. Half of that leaves room for a different font
# without letting a half-drawn block through.
BLOCK_COVERAGE = 0.5

# How many pixels of each colour probe must survive into the picture. Measured
# on real frames: 103 truecolor, 92 indexed, 50 basic — the probes are one word
# of small text each. A tenth of the smallest catches a render that has lost a
# colour class without failing on a different font's thinner glyphs.
PROBE_FLOOR_PX = 10

# How far the border outline's measured span may sit from the cell count shux
# reports before the two are calling it a different grid. A quarter cell: the
# block-derived cell is exact on a background run, so anything approaching half a
# cell is a real disagreement rather than measurement noise.
GRID_SLACK_CELLS = 0.25


@dataclass
class Phase:
    """One capture phase: a window size, and the frames taken at it."""

    name: str
    window_w: int
    window_h: int
    pane_cols: int
    pane_rows: int
    status_rows: int
    block: dict[str, int]
    frames: list[Path]
    require_image: bool


@dataclass
class GeometryFile:
    border_rgb: tuple[int, int, int]
    status_rgb: tuple[int, int, int]
    content_rgb: tuple[int, int, int]
    image_rgb: tuple[int, int, int]
    # The status bar's foreground, and the terminal's background — the other
    # halves of the two palettes that bound what may appear outside the pane.
    status_fg_rgb: tuple[int, int, int]
    background_rgb: tuple[int, int, int]
    # The truecolor / indexed / basic probes as kitty renders them, which is what
    # makes a lost colour class visible rather than merely printed.
    probe_rgb: list[tuple[int, int, int]]
    scenario: str
    tolerance: int
    phases: list[Phase] = field(default_factory=list)


class BadGeometry(Exception):
    """The geometry file is unusable — a broken input, not a failed assertion."""


def _rgb(raw: object, what: str) -> tuple[int, int, int]:
    """A colour, or a reason it is not one.

    Enumerated rather than left to the catch-all below because these are the
    shapes that PARSE and then fail much later, outside any try: a two-element
    colour broadcasts wrong deep inside a mask, and a channel over 255 matches
    nothing and reports as "no border rectangle".
    """
    if not isinstance(raw, (list, tuple)) or len(raw) != 3:
        raise BadGeometry(f"{what} must be three numbers, got {raw!r}")
    try:
        values = tuple(int(c) for c in raw)
    except (TypeError, ValueError):
        raise BadGeometry(f"{what} must be three integers, got {raw!r}") from None
    if any(c < 0 or c > 255 for c in values):
        raise BadGeometry(f"{what} must be three 0-255 values, got {raw!r}")
    return values


def load_geometry(path: Path) -> GeometryFile:
    """Read `run.json`, and reject anything it cannot honestly judge.

    Every failure raises `BadGeometry`, because exit 1 means shux painted
    outside the box and exit 2 means this file could not be read, and a reader
    has to be able to tell them apart. The catch-all is the point: an enumerated
    validator is only as complete as its author, and the first one shipped here
    left `probe_rgb` unchecked and tracebacked out at exit 1 on it.
    """
    try:
        raw = json.loads(path.read_text())
        phases = []
        for i, phase in enumerate(raw["phases"]):
            where = f"phase {i}"
            window, pane = phase["window"], phase["pane"]
            block = {k: int(phase["block"][k]) for k in ("row0", "row1", "col0", "col1")}
            size = {
                "window.w": int(window["w"]),
                "window.h": int(window["h"]),
                "pane.cols": int(pane["cols"]),
                "pane.rows": int(pane["rows"]),
            }
            for name, value in size.items():
                if value < 1:
                    raise BadGeometry(f"{where}: {name} must be at least 1, got {value}")
            if block["row1"] < block["row0"] or block["col1"] < block["col0"]:
                raise BadGeometry(f"{where}: block rect is inside out: {block}")
            if min(block.values()) < 0:
                raise BadGeometry(f"{where}: block rect has a negative coordinate: {block}")
            frames = phase["frames"]
            if not isinstance(frames, list):
                raise BadGeometry(f"{where}: 'frames' must be a list, got {frames!r}")
            phases.append(
                Phase(
                    name=str(phase["name"]),
                    window_w=size["window.w"],
                    window_h=size["window.h"],
                    pane_cols=size["pane.cols"],
                    pane_rows=size["pane.rows"],
                    status_rows=int(phase.get("status_rows", 1)),
                    block=block,
                    frames=[Path(str(f)) for f in frames],
                    require_image=bool(phase.get("require_image", False)),
                )
            )

        probes = [_rgb(c, f"probe_rgb[{i}]") for i, c in enumerate(raw["probe_rgb"])]
        # All three, or the colour-probe rule CLAUDE.md mandates is disarmed by a
        # one-key edit — `zip` would simply stop early and the frame would pass.
        if len(probes) != 3:
            raise BadGeometry(f"probe_rgb must list three colours, got {len(probes)}")

        return GeometryFile(
            border_rgb=_rgb(raw["border_rgb"], "border_rgb"),
            status_rgb=_rgb(raw["status_rgb"], "status_rgb"),
            content_rgb=_rgb(raw["content_rgb"], "content_rgb"),
            image_rgb=_rgb(raw["image_rgb"], "image_rgb"),
            status_fg_rgb=_rgb(raw["status_fg_rgb"], "status_fg_rgb"),
            background_rgb=_rgb(raw["background_rgb"], "background_rgb"),
            probe_rgb=probes,
            scenario=str(raw.get("scenario", "plain")),
            tolerance=int(raw.get("tolerance", DEFAULT_TOLERANCE)),
            phases=phases,
        )
    except BadGeometry:
        raise
    except (OSError, TypeError, ValueError, KeyError, IndexError, json.JSONDecodeError) as exc:
        raise BadGeometry(f"{type(exc).__name__}: {exc}") from None


def mask_for(arr: np.ndarray, rgb: tuple[int, int, int], tol: int) -> np.ndarray:
    """Pixels within `tol` of `rgb` on every channel."""
    return np.abs(arr - np.array(rgb, dtype=np.int16)).max(axis=2) <= tol


def _rule_pair(counts: np.ndarray, threshold: float) -> tuple[int, int] | None:
    """Centres of the first and last runs of indices at or above `threshold`.

    A border line is 1-2 px of ink, so the run is collapsed to its midpoint and
    the measurement stops depending on the line's thickness.
    """
    hits = np.nonzero(counts >= threshold)[0]
    if hits.size == 0:
        return None
    hit_set = set(int(h) for h in hits)
    lead = int(hits[0])
    while lead + 1 in hit_set:
        lead += 1
    trail = int(hits[-1])
    while trail - 1 in hit_set:
        trail -= 1
    return (int((int(hits[0]) + lead) / 2), int((int(hits[-1]) + trail) / 2))


def find_rect(mask: np.ndarray) -> tuple[int, int, int, int] | None:
    """The (y0, y1, x0, x1) rules of a rectangle drawn in `mask`'s colour.

    The threshold is a fraction of the LONGEST run found, not of the image
    width. The window under photograph is a fraction of the screen and shrinks
    during the run, and the pane's title is drawn ON the top border row in the
    border colour, so both a fixed pixel count and a fraction of the frame get
    it wrong: measured at an 860 px window, the title ate enough of the top rule
    that a half-the-frame threshold saw only the bottom one and called the
    rectangle degenerate.
    """
    row_counts = mask.sum(axis=1)
    if row_counts.max() == 0:
        return None
    rows = _rule_pair(row_counts, max(row_counts.max() * LINE_COVERAGE, 3))
    if rows is None:
        return None
    y0, y1 = rows
    # Vertical rules are only as tall as the box, so they are looked for inside
    # the band the horizontal rules bound rather than down the whole frame.
    band = mask[y0 : y1 + 1, :]
    col_counts = band.sum(axis=0)
    if col_counts.max() == 0:
        return None
    cols = _rule_pair(col_counts, max(col_counts.max() * LINE_COVERAGE, 3))
    if cols is None:
        return None
    x0, x1 = cols
    return y0, y1, x0, x1


class FrameVerdict:
    """Every assertion for one frame, with the numbers behind each."""

    def __init__(self, geom: GeometryFile, phase: Phase, path: Path) -> None:
        self.geom = geom
        self.phase = phase
        self.path = path
        self.reasons: list[str] = []
        self.notes: list[str] = []
        self.status_band: tuple[int, int, int, int] | None = None

    def fail(self, reason: str, detail: str) -> None:
        if reason not in self.reasons:
            self.reasons.append(reason)
        self.notes.append(f"{reason}: {detail}")

    def run(self) -> bool:
        if not self.path.exists():
            self.fail("missing", f"{self.path} does not exist")
            return False
        try:
            img = Image.open(self.path)
            img.load()
            arr = np.asarray(img.convert("RGB")).astype(np.int16)
        except Exception as exc:  # a truncated capture is a failure, not a skip
            self.fail("unreadable", f"{self.path}: {exc}")
            return False
        if arr.size == 0:
            self.fail("unreadable", f"{self.path} has no pixels")
            return False

        rect = self.check_chrome(arr)
        if rect is None:
            return False
        cell = self.check_grid(arr, rect)
        self.check_payloads(arr, rect, cell)
        return not self.reasons

    # ── 2. shux's chrome, located from the frame rather than from font maths ─
    def check_chrome(self, arr: np.ndarray) -> tuple[int, int, int, int] | None:
        tol = self.geom.tolerance
        border = mask_for(arr, self.geom.border_rgb, tol)
        rect = find_rect(border)
        if rect is None:
            self.fail(
                "chrome",
                f"no border rectangle in {self.geom.border_rgb} "
                f"({int(border.sum())} matching pixels)",
            )
            return None
        y0, y1, x0, x1 = rect
        if x1 <= x0 or y1 <= y0:
            self.fail("chrome", f"degenerate border rectangle ({x0},{y0})-({x1},{y1})")
            return None
        self.notes.append(f"border rect: ({x0},{y0})-({x1},{y1})")
        # A rectangle flush with the edge of the frame means the window ran off
        # the screen and X clipped it. The region an overflow would land in was
        # then never photographed, and "no payload pixels outside the pane"
        # becomes true for the wrong reason — a vacuous pass on the one defect
        # this rig exists to catch.
        h, w = arr.shape[:2]
        if x0 <= 0 or y0 <= 0 or x1 >= w - 1 or y1 >= h - 1:
            self.fail(
                "clipped",
                f"border rectangle ({x0},{y0})-({x1},{y1}) touches the edge of the "
                f"{w}x{h} frame — the window is larger than the screen and the "
                f"capture is missing the region an overflow would land in",
            )
            return None

        status = mask_for(arr, self.geom.status_rgb, tol)
        if not status.any():
            self.fail("chrome", f"no status bar pixels in {self.geom.status_rgb}")
        else:
            ys = np.nonzero(status.any(axis=1))[0]
            if ys.min() <= y1:
                self.fail(
                    "chrome",
                    f"status bar starts at y={int(ys.min())}, at or above the "
                    f"pane border's bottom rule y={y1}",
                )
            xs = np.nonzero(status.any(axis=0))[0]
            self.status_band = (int(ys.min()), int(ys.max()), int(xs.min()), int(xs.max()))
            self.notes.append(
                f"status band: rows {ys.min()}..{ys.max()}, cols {xs.min()}..{xs.max()}"
            )
        return x0, y0, x1, y1

    # ── 3. the grid shux believes in vs the grid kitty is drawing ────────────
    def check_grid(
        self, arr: np.ndarray, rect: tuple[int, int, int, int]
    ) -> tuple[float, float] | None:
        """Does the size shux TOLD the pane match the grid kitty is drawing?

        The cell is measured from the payload BLOCK, not from the border rules
        divided by the pane size. Deriving it from `pane_cols` and then checking
        `pane_cols` with it puts the number under test on both sides, and the
        test very nearly cancels: measured that way, lying about `pane.rows` by
        1, 2 or 3 and about `pane.cols` by 1 all PASSED, and `grid` did not fire
        until rows were off by 4 or columns by 12. The block's pixel extent is
        independent of both, because it is a background run that fills whole
        cells — so dividing it by the block's own cell rect gives a cell size
        that `pane_cols` cannot influence, and `pane_cols` becomes falsifiable.
        """
        x0, y0, x1, y1 = rect
        ph = self.phase
        block_cols = ph.block["col1"] - ph.block["col0"] + 1
        block_rows = ph.block["row1"] - ph.block["row0"] + 1
        block = mask_for(arr, self.geom.content_rgb, self.geom.tolerance)
        if not block.any():
            self.fail(
                "grid",
                "cannot measure a cell: the payload block is not in the frame, so "
                "there is nothing to derive the emulator's grid from",
            )
            return None
        ys, xs = np.nonzero(block)
        cell_w = (int(xs.max()) + 1 - int(xs.min())) / block_cols
        cell_h = (int(ys.max()) + 1 - int(ys.min())) / block_rows
        self.notes.append(
            f"cell: {cell_w:.2f}x{cell_h:.2f} px, measured from the "
            f"{block_cols}x{block_rows}-cell payload block"
        )
        if cell_w < 3 or cell_h < 3:
            self.fail("grid", f"implausible cell size {cell_w:.2f}x{cell_h:.2f}")
            return None

        # The rules run down the CENTRE of the border cells, so the distance
        # between opposite rules is one cell short of the outline's full extent.
        span_cols = (x1 - x0) / cell_w
        span_rows = (y1 - y0) / cell_h
        if abs(span_cols - (ph.pane_cols + 1)) > GRID_SLACK_CELLS or abs(
            span_rows - (ph.pane_rows + 1)
        ) > GRID_SLACK_CELLS:
            self.fail(
                "grid",
                f"the border outline spans {span_cols:.2f}x{span_rows:.2f} cells, but "
                f"shux says the pane is {ph.pane_cols}x{ph.pane_rows} — which would make "
                f"it {ph.pane_cols + 1}x{ph.pane_rows + 1}",
            )
            return None

        # The whole grid — pane, its outline, the status bar — must tile the
        # emulator's window with less than one cell spare.
        spare_w = ph.window_w - (ph.pane_cols + 2) * cell_w
        spare_h = ph.window_h - (ph.pane_rows + 2 + ph.status_rows) * cell_h
        self.notes.append(
            f"window {ph.window_w}x{ph.window_h}: {spare_w:.1f}x{spare_h:.1f} px spare"
        )
        if not (-1 < spare_w < cell_w) or not (-1 < spare_h < cell_h):
            self.fail(
                "grid",
                f"{spare_w:.1f}x{spare_h:.1f} px left over in a "
                f"{ph.window_w}x{ph.window_h} window at cell {cell_w:.2f}x{cell_h:.2f} — "
                f"shux's {ph.pane_cols}x{ph.pane_rows} pane does not fit the emulator's grid",
            )
            return None
        # Downstream gets the cell derived from the OUTLINE, not from the block.
        # The span check above has just cross-validated the two to a quarter of a
        # cell, and handing on the block-derived one would make `crosspath` and
        # `check_block_area` circular: both measure the block against a cell
        # measured from the block, so the far edge of its rect could not fail
        # independently of the near one.
        return (x1 - x0) / (ph.pane_cols + 1), (y1 - y0) / (ph.pane_rows + 1)

    # ── 4/5/6. containment, content, cross-path ─────────────────────────────
    def check_payloads(
        self,
        arr: np.ndarray,
        rect: tuple[int, int, int, int],
        cell: tuple[float, float] | None,
    ) -> None:
        x0, y0, x1, y1 = rect
        tol = self.geom.tolerance
        if cell is None:
            return
        cell_w, cell_h = cell
        # Interior of the outline: half a cell in from each rule, because the
        # border ink runs down the CENTRE of its cell. The extra couple of pixels
        # are float slack, not amnesty — `x0 + cell_w / 2` lands on the payload
        # block's exact left edge, so rounding the wrong way would count a column
        # of legitimate content as an overflow. The defect this must catch is a
        # cell wide at minimum (10 px here) and was 160-252 px in the field.
        # `round`, not `int`: truncation goes toward zero and so takes a
        # different side on each edge. Measured, that made the blind band 3 px on
        # the left, 2 on the right, 4 on the top and 1 on the bottom — none of
        # them the constant this names.
        ix0 = round(x0 + cell_w / 2) - EDGE_MARGIN_PX
        ix1 = round(x1 - cell_w / 2) + EDGE_MARGIN_PX
        iy0 = round(y0 + cell_h / 2) - EDGE_MARGIN_PX
        iy1 = round(y1 - cell_h / 2) + EDGE_MARGIN_PX

        inside = np.zeros(arr.shape[:2], dtype=bool)
        inside[max(iy0, 0) : iy1 + 1, max(ix0, 0) : ix1 + 1] = True

        self.check_foreign(arr, inside, (ix0, iy0, ix1, iy1))

        # Only the injected payload is named here. The workload block needs no
        # containment check of its own: any of its pixels outside the pane are
        # 150 units from the nearest chrome colour and `containment:foreign`
        # counts them already, and "the block is missing" is `content:block`,
        # which is the stronger statement. The image keeps its own name because
        # the self-test requires THAT failure — a shared one would let the
        # overflow arm pass on a rig that cannot see images at all, via a
        # mis-measured border tripping it on the workload block.
        mask = mask_for(arr, self.geom.image_rgb, tol)
        total = int(mask.sum())
        outside = int((mask & ~inside).sum())
        self.notes.append(f"image {self.geom.image_rgb}: {total} px, {outside} outside the pane")
        if outside > 0:
            ys, xs = np.nonzero(mask & ~inside)
            self.fail(
                "containment:image",
                f"{outside} image pixel(s) painted outside the pane interior — "
                f"bbox ({int(xs.min())},{int(ys.min())})-({int(xs.max())},{int(ys.max())}) "
                f"vs interior ({int(ix0)},{int(iy0)})-({int(ix1)},{int(iy1)})",
            )
        if self.phase.require_image and total == 0:
            self.fail("content:image", "no image pixels anywhere in the frame")

        # The strict box: the same interior with the margin taken off the other
        # side. The two questions want opposite slack — "is this payload outside"
        # must not count a legitimate edge cell, while "is a landmark loose
        # inside" must not count the border's own ink, whose antialiasing reaches
        # 2 px into the lenient box.
        strict = np.zeros(arr.shape[:2], dtype=bool)
        strict[
            max(iy0 + 2 * EDGE_MARGIN_PX, 0) : iy1 - 2 * EDGE_MARGIN_PX + 1,
            max(ix0 + 2 * EDGE_MARGIN_PX, 0) : ix1 - 2 * EDGE_MARGIN_PX + 1,
        ] = True

        self.check_block_area(arr, cell)
        self.check_fiducials(arr, strict)
        self.check_probes(arr, inside)
        self.check_crosspath(arr, rect, cell)

    def check_foreign(
        self,
        arr: np.ndarray,
        inside: np.ndarray,
        interior: tuple[float, float, float, float],
    ) -> None:
        """Nothing but chrome may be painted outside the pane — ANY colour.

        Counting one hand-picked payload colour only ever catches the payload
        this rig paints for itself; a real emitted image, thumbnail or sixel is
        not one flat colour and would score zero.

        It has to be careful about WHERE, not just what: black and white are
        both chrome, and one palette spanning them admits the whole greyscale
        axis. So each region gets only the palette it can legitimately contain —
        the status bar its own two colours, the border ring and the desktop the
        border colour on the terminal's background — and nothing bridges them.
        """
        outside = ~inside

        # The bar's own bounding box: it is a solid background run across the
        # full terminal width, so that is exact, where deriving the width from
        # the outline overshoots into the desktop beyond the window. The frame
        # region excludes the same rectangle grown by the margin, so the boundary
        # is judged by neither palette rather than by the wrong one; at 2 px it
        # cannot hide an overflow, which is a cell.
        status_region = np.zeros(arr.shape[:2], dtype=bool)
        status_guard = np.zeros(arr.shape[:2], dtype=bool)
        if self.status_band is not None:
            top, bottom, left, right = self.status_band
            status_region[top : bottom + 1, left : right + 1] = True
            status_guard[
                max(top - EDGE_MARGIN_PX, 0) : bottom + EDGE_MARGIN_PX + 1,
                max(left - EDGE_MARGIN_PX, 0) : right + EDGE_MARGIN_PX + 1,
            ] = True
        status_region &= outside

        regions = (
            ("status bar", status_region, (self.geom.status_rgb, self.geom.status_fg_rgb)),
            ("border and desktop", outside & ~status_guard,
             (self.geom.background_rgb, self.geom.border_rgb)),
        )

        ix0, iy0, ix1, iy1 = interior
        for label, region, palette in regions:
            pixels = arr[region].astype(float)
            if pixels.size == 0:
                continue
            a = np.array(palette[0], dtype=float)
            b = np.array(palette[1], dtype=float)
            ab = b - a
            denom = float(ab @ ab) or 1.0
            t = np.clip(((pixels - a) @ ab) / denom, 0.0, 1.0)[:, None]
            dist = np.linalg.norm(pixels - (a + t * ab), axis=1)
            bad = dist > FOREIGN_TOLERANCE
            foreign = int(bad.sum())
            self.notes.append(
                f"{label}: {foreign} of {len(pixels)} px are not "
                f"{palette[0]}..{palette[1]}"
            )
            if foreign > 0:
                ys, xs = np.nonzero(region)
                self.fail(
                    "containment:foreign",
                    f"{foreign} pixel(s) over the {label} are neither "
                    f"{palette[0]} nor {palette[1]} nor a blend of them — bbox "
                    f"({int(xs[bad].min())},{int(ys[bad].min())})-"
                    f"({int(xs[bad].max())},{int(ys[bad].max())}), pane interior "
                    f"({int(ix0)},{int(iy0)})-({int(ix1)},{int(iy1)})",
                )

    def check_block_area(self, arr: np.ndarray, cell: tuple[float, float]) -> None:
        """The payload block must cover the area its cell rect implies.

        "Some payload pixels exist" passes on a block that is half-drawn or on a
        single stray cell. The block is a background run, so its area is
        arithmetic: measured, the glyph ink of the `~` characters costs 10% of
        it, and nothing else does.
        """
        cell_w, cell_h = cell
        b = self.phase.block
        cells = (int(b["col1"]) - int(b["col0"]) + 1) * (int(b["row1"]) - int(b["row0"]) + 1)
        want = cells * cell_w * cell_h * BLOCK_COVERAGE
        got = int(mask_for(arr, self.geom.content_rgb, self.geom.tolerance).sum())
        self.notes.append(f"block: {got} px against a {want:.0f} px floor ({cells} cells)")
        if got < want:
            self.fail(
                "content:block",
                f"the payload block covers {got} px, under the {want:.0f} px floor "
                f"its {cells}-cell rect implies — it is half-drawn or torn",
            )

    def check_fiducials(self, arr: np.ndarray, strict: np.ndarray) -> None:
        """No chrome fiducial may appear INSIDE the pane.

        The fiducials are only landmarks while nothing else paints them, and
        every geometry assertion here is measured from them. It is also the only
        thing that sees an overflow painted in the BORDER's own colour: over the
        border ring that is legitimate chrome by construction, so a red block
        reaching into the pane is caught here or nowhere. Measured — with this
        check absent, a 110x114 px block of #ff0000 covering the pane interior,
        the border and the status bar below it passed every other assertion.
        """
        for label, rgb in (("border", self.geom.border_rgb), ("status", self.geom.status_rgb)):
            hits = int((mask_for(arr, rgb, self.geom.tolerance) & strict).sum())
            if hits > 0:
                ys, xs = np.nonzero(mask_for(arr, rgb, self.geom.tolerance) & strict)
                self.fail(
                    "fiducial",
                    f"{hits} pixel(s) of the {label} fiducial {rgb} are inside the "
                    f"pane — bbox ({int(xs.min())},{int(ys.min())})-"
                    f"({int(xs.max())},{int(ys.max())}). Either something is painting "
                    f"a landmark colour, or an overflow arrived in it; the geometry "
                    f"measured from that landmark cannot be trusted either way",
                )

    def check_probes(self, arr: np.ndarray, inside: np.ndarray) -> None:
        """The truecolor, indexed and basic probes must all be in the picture.

        CLAUDE.md requires all three of any daemon-backed test that captures pane
        output, so that a run which has lost colour cannot pass by drawing the
        right shapes in grey. Printing them is not the requirement — reading them
        back is. Every other assertion here is about geometry or about a
        background fill, so without this one an attach path that dropped indexed
        or basic colour entirely would leave every frame green.
        """
        for label, rgb in zip(("truecolor", "indexed", "basic"), self.geom.probe_rgb):
            hits = int((mask_for(arr, rgb, self.geom.tolerance) & inside).sum())
            self.notes.append(f"{label} probe {rgb}: {hits} px")
            if hits < PROBE_FLOOR_PX:
                self.fail(
                    "probe",
                    f"the {label} probe rendered as {hits} px of {rgb}, under the "
                    f"{PROBE_FLOOR_PX} px floor — that colour class is not reaching "
                    f"the emulator",
                )

    def check_crosspath(
        self,
        arr: np.ndarray,
        rect: tuple[int, int, int, int],
        cell: tuple[float, float],
    ) -> None:
        """The block's cell rect, measured in kitty, vs shux's own capture."""
        x0, y0, x1, y1 = rect
        cell_w, cell_h = cell
        mask = mask_for(arr, self.geom.content_rgb, self.geom.tolerance)
        ys, xs = np.nonzero(mask)
        # Cell coordinates are PANE-local: the outline's top-left rule is the
        # centre of pane cell (-1, -1), so the pane's cell (0,0) starts half a
        # cell past it.
        origin_x = x0 + cell_w / 2
        origin_y = y0 + cell_h / 2
        got = {
            "col0": int(round((xs.min() - origin_x) / cell_w)),
            "col1": int(round((xs.max() + 1 - origin_x) / cell_w)) - 1,
            "row0": int(round((ys.min() - origin_y) / cell_h)),
            "row1": int(round((ys.max() + 1 - origin_y) / cell_h)) - 1,
        }
        want = {k: int(self.phase.block[k]) for k in ("col0", "col1", "row0", "row1")}
        self.notes.append(f"block cells: kitty {got} vs shux {want}")
        if got != want:
            diff = {k: (want[k], got[k]) for k in want if want[k] != got[k]}
            self.fail(
                "crosspath",
                f"payload block sits at different cells in the two render paths "
                f"(shux, kitty): {diff}",
            )


def probe(path: Path, rgb: tuple[int, int, int], tol: int) -> int:
    """Is shux's chrome on the screen yet?

    The rig waits on this before it starts a phase — "require content, then
    settle", because `wait-settled` alone races a slow starter and photographs a
    blank screen that every later assertion then agrees with. It deliberately
    reuses the SAME rectangle detector the assertions use, so the thing being
    waited for is the thing being measured.
    """
    if not path.exists():
        return 1
    try:
        img = Image.open(path)
        img.load()
        arr = np.asarray(img.convert("RGB")).astype(np.int16)
    except Exception:
        return 1
    rect = find_rect(mask_for(arr, rgb, tol))
    if rect is None:
        return 1
    y0, y1, x0, x1 = rect
    h, w = arr.shape[:2]
    # The same two rejections `check_chrome` makes. Without them the rig waits on
    # a rectangle its own assertions would throw out — a degenerate one, or one
    # flush with the frame edge, which is the clipped-window case exactly.
    if x1 <= x0 or y1 <= y0:
        return 1
    if x0 <= 0 or y0 <= 0 or x1 >= w - 1 or y1 >= h - 1:
        return 1
    # Print the rectangle, so the caller can wait for it to STOP MOVING as well
    # as to appear. Measured across a resize, one capture caught the old box with
    # its origin shifted a pixel — mid-flight geometry that satisfies "the chrome
    # is up" and would be measured as a grid mismatch a moment before it settles.
    print(f"{x0} {y0} {x1} {y1}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--geometry", type=Path)
    ap.add_argument(
        "--probe",
        type=Path,
        help="exit 0 if this frame already shows a border rectangle, 1 if not",
    )
    ap.add_argument("--border-rgb", default=None, help="R,G,B for --probe")
    ap.add_argument("--tolerance", type=int, default=DEFAULT_TOLERANCE)
    ap.add_argument(
        "--verbose",
        action="store_true",
        help="print every measurement, not just the ones behind a failure",
    )
    args = ap.parse_args()

    if args.probe is not None:
        if args.border_rgb is None:
            print("✗ --probe requires --border-rgb", file=sys.stderr)
            return 2
        try:
            rgb = _rgb(args.border_rgb.split(","), "--border-rgb")
        except BadGeometry as exc:
            print(f"✗ {exc}", file=sys.stderr)
            return 2
        return probe(args.probe, rgb, args.tolerance)

    if args.geometry is None:
        print("✗ one of --geometry or --probe is required", file=sys.stderr)
        return 2
    if not args.geometry.exists():
        print(f"✗ geometry file {args.geometry} does not exist", file=sys.stderr)
        return 2
    try:
        geom = load_geometry(args.geometry)
    except BadGeometry as exc:
        print(f"✗ geometry file {args.geometry}: {exc}", file=sys.stderr)
        return 2

    total_frames = 0
    failed_frames = 0
    reasons: list[str] = []

    for phase in geom.phases:
        print(f"▶ phase {phase.name}: {len(phase.frames)} frame(s)")
        if not phase.frames:
            print(f"  ✗ phase {phase.name} captured no frames")
            failed_frames += 1
            reasons.append("noframes")
            continue
        for path in phase.frames:
            total_frames += 1
            verdict = FrameVerdict(geom, phase, path)
            ok = verdict.run()
            mark = "\033[32m✓\033[0m" if ok else "\033[31m✗\033[0m"
            print(f"  {mark} {path.name}")
            if ok and not args.verbose:
                continue
            for note in verdict.notes:
                print(f"      {note}")
            if not ok:
                failed_frames += 1
                for reason in verdict.reasons:
                    if reason not in reasons:
                        reasons.append(reason)

    if total_frames == 0:
        print("✗ verdict: FAIL reasons=noframes (nothing was captured)", file=sys.stderr)
        return 1
    if failed_frames:
        print(
            f"✗ verdict: FAIL reasons={','.join(reasons)} "
            f"({failed_frames} of {total_frames} frames)",
            file=sys.stderr,
        )
        return 1
    print(f"✓ verdict: PASS ({total_frames} frames, {len(geom.phases)} phases)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
