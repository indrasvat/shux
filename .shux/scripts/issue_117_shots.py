#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow"]
# ///
"""Prepare issue #117 evidence shots for publication.

Upscales each rasterized pane PNG with nearest-neighbour sampling (so the glyph
edges stay crisp rather than being blurred by interpolation) and emits a
manifest of base64 data URIs for embedding in a self-contained page.

Also asserts on CONTENT, not on "the file exists": every shot must have more
than one distinct colour, and the `after` shots that are supposed to be the
alignment pattern must actually be dense with ink.

    .shux/scripts/issue_117_shots.py --scale 3 --out .shux/out/issue-117/shots
"""

from __future__ import annotations

import argparse
import base64
import json
from pathlib import Path

from PIL import Image

SCENES = [
    "alignment-pattern",
    "conformance-run",
    "scroll-region",
    "alt-recycle",
    "richtui-vim",
]

# Rich-TUI shots live in their own directory and are already 900x570 native, so
# they are upscaled less than the small evidence panes.
RICH_TUIS = ["vim", "nvim", "htop", "btop", "lazygit", "less"]
RICH_SCALE = 2


def ink_ratio(img: Image.Image) -> float:
    """Fraction of pixels that are not the single most common colour."""
    rgb = img.convert("RGB")
    colors = rgb.getcolors(maxcolors=1 << 24) or []
    total = rgb.width * rgb.height
    background = max(count for count, _ in colors) if colors else total
    return 1.0 - background / total


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=".shux/out/issue-117")
    ap.add_argument("--out", default=".shux/out/issue-117/shots")
    ap.add_argument("--scale", type=int, default=3)
    args = ap.parse_args()

    root = Path(args.root)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    manifest: dict[str, dict[str, object]] = {}
    failures: list[str] = []

    for label in ("before", "after"):
        for scene in SCENES:
            src = root / label / f"{scene}.png"
            if not src.exists():
                failures.append(f"missing {src}")
                continue
            img = Image.open(src)
            big = img.resize(
                (img.width * args.scale, img.height * args.scale),
                Image.Resampling.NEAREST,
            )
            dst = out / f"{label}-{scene}.png"
            big.save(dst, optimize=True)

            ratio = ink_ratio(img)
            if ratio < 0.01:
                failures.append(f"{label}/{scene}: blank shot (ink {ratio:.4f})")

            manifest[f"{label}-{scene}"] = {
                "src": str(src),
                "png": str(dst),
                "size": [big.width, big.height],
                "bytes": dst.stat().st_size,
                "ink": round(ratio, 4),
                "data_uri": "data:image/png;base64,"
                + base64.b64encode(dst.read_bytes()).decode("ascii"),
            }
            print(f"{label:6s} {scene:20s} {big.width}x{big.height} "
                  f"{dst.stat().st_size:8d}B ink={ratio:.3f}")

    # Rich TUIs, from their own directory.
    rich_root = root / "richtui"
    for tui in RICH_TUIS:
        src = rich_root / f"{tui}.png"
        if not src.exists():
            failures.append(f"missing {src}")
            continue
        img = Image.open(src)
        big = img.resize(
            (img.width * RICH_SCALE, img.height * RICH_SCALE),
            Image.Resampling.NEAREST,
        )
        dst = out / f"richtui-{tui}.png"
        big.save(dst, optimize=True)
        # A rich-TUI shot is NOT judged by ink alone. `vim` showing a two-line
        # file on a 100x30 pane is legitimately sparse (0.6% ink) while `btop`
        # is dense; an ink threshold tuned for one calls the other blank. The
        # real question is whether the TUI drew, so the sibling capture the
        # harness wrote is checked for content, and ink only has to be nonzero.
        ratio = ink_ratio(img)
        text = (rich_root / f"{tui}.txt")
        drawn = [ln for ln in text.read_text().splitlines() if ln.strip()] if text.exists() else []
        if not drawn:
            failures.append(f"richtui/{tui}: capture has no content")
        if ratio <= 0.0:
            failures.append(f"richtui/{tui}: shot is entirely one colour")
        manifest[f"richtui-{tui}"] = {
            "content_lines": len(drawn),
            "src": str(src),
            "png": str(dst),
            "size": [big.width, big.height],
            "bytes": dst.stat().st_size,
            "ink": round(ratio, 4),
            "data_uri": "data:image/png;base64,"
            + base64.b64encode(dst.read_bytes()).decode("ascii"),
        }
        print(f"richtui {tui:20s} {big.width}x{big.height} "
              f"{dst.stat().st_size:8d}B ink={ratio:.3f}")

    # The `after` alignment shots are meant to be a wall of glyphs. A pattern
    # that renders as almost-nothing would still be a valid PNG of the right
    # size, so assert the ink is actually there.
    for scene in ("alignment-pattern", "conformance-run", "scroll-region"):
        key = f"after-{scene}"
        if key in manifest and float(manifest[key]["ink"]) < 0.20:  # type: ignore[arg-type]
            failures.append(f"{key}: too little ink for an alignment pattern")
    # And the `before` counterparts must NOT be, or the A/B proves nothing.
    for scene in ("alignment-pattern", "scroll-region"):
        key = f"before-{scene}"
        if key in manifest and float(manifest[key]["ink"]) > 0.20:  # type: ignore[arg-type]
            failures.append(f"{key}: base build shot is unexpectedly dense")

    (out / "manifest.json").write_text(json.dumps(manifest, indent=2))
    print(f"\nmanifest: {out / 'manifest.json'}")

    if failures:
        print("\nFAILURES:")
        for f in failures:
            print(f"  {f}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
