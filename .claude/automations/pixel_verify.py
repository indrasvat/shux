#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = ["pillow", "numpy"]
# ///
"""Pixel-level PNG verification helper for shux visual gates.

Compares two PNGs and emits JSON metrics suitable for hard-gating a task.
It intentionally fails on size mismatch unless --allow-size-mismatch is set.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageChops


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Compare two PNGs at pixel level.")
    parser.add_argument("actual", type=Path, help="actual screenshot PNG")
    parser.add_argument("expected", type=Path, help="expected/baseline PNG")
    parser.add_argument("--diff", type=Path, help="optional output diff PNG")
    parser.add_argument("--max-pixel-diff-ratio", type=float, default=0.0)
    parser.add_argument("--max-mean-channel-delta", type=float, default=0.0)
    parser.add_argument("--allow-size-mismatch", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    actual_path = args.actual
    expected_path = args.expected

    if not actual_path.exists():
        print(json.dumps({"status": "fail", "reason": "actual_missing", "actual": str(actual_path)}))
        return 2
    if not expected_path.exists():
        print(json.dumps({"status": "fail", "reason": "expected_missing", "expected": str(expected_path)}))
        return 2

    actual = Image.open(actual_path).convert("RGBA")
    expected = Image.open(expected_path).convert("RGBA")

    size_mismatch = actual.size != expected.size
    if size_mismatch and not args.allow_size_mismatch:
        print(
            json.dumps(
                {
                    "status": "fail",
                    "reason": "size_mismatch",
                    "actual": str(actual_path),
                    "expected": str(expected_path),
                    "actual_size": actual.size,
                    "expected_size": expected.size,
                },
                sort_keys=True,
            )
        )
        return 2

    if size_mismatch:
        compare_size = (min(actual.width, expected.width), min(actual.height, expected.height))
        actual = actual.crop((0, 0, compare_size[0], compare_size[1]))
        expected = expected.crop((0, 0, compare_size[0], compare_size[1]))

    # RGBA comparison is intentional: alpha differences are visual state too.
    actual_arr = np.array(actual, dtype=np.int16)
    expected_arr = np.array(expected, dtype=np.int16)
    diff_arr = np.abs(actual_arr - expected_arr)

    total_pixels = actual.width * actual.height
    changed_pixels = int(np.sum(np.any(diff_arr > 0, axis=-1)))
    pixel_diff_ratio = changed_pixels / total_pixels if total_pixels else 0.0
    mean_rgba_channel_delta = float(np.mean(diff_arr))

    if args.diff:
        args.diff.parent.mkdir(parents=True, exist_ok=True)
        # Amplify nonzero deltas for human review while keeping exact geometry.
        diff = ImageChops.difference(actual, expected)
        diff.point(lambda value: 255 if value else 0).save(args.diff)

    passed = (
        pixel_diff_ratio <= args.max_pixel_diff_ratio
        and mean_rgba_channel_delta <= args.max_mean_channel_delta
    )
    payload = {
        "status": "pass" if passed else "fail",
        "actual": str(actual_path),
        "expected": str(expected_path),
        "size": actual.size,
        "changed_pixels": changed_pixels,
        "total_pixels": total_pixels,
        "pixel_diff_ratio": pixel_diff_ratio,
        "mean_rgba_channel_delta": mean_rgba_channel_delta,
        "max_pixel_diff_ratio": args.max_pixel_diff_ratio,
        "max_mean_channel_delta": args.max_mean_channel_delta,
        "diff": str(args.diff) if args.diff else None,
    }
    print(json.dumps(payload, sort_keys=True))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
