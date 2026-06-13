# Baseline Approval: Task 072 Origin Mode

Post-render baseline review completed after `SHUX_ORIGIN_MODE_PROMOTE=1 make test-vt-origin-mode`.

Reviewed full-resolution PNGs:

- `origin-80x24-actual.png`
- `origin-120x40-actual.png`
- `origin-200x60-actual.png`

Visual acceptance:

- Header is fixed at row 1 and does not receive body or footer text.
- Footer is fixed at the last row and does not receive body text.
- `BODY-TOP` lands at the scroll-region top.
- `BODY-BOTTOM` lands at the scroll-region bottom.
- `CLAMP-UP` lands at the scroll-region top.
- `CLAMP-DOWN` lands at the scroll-region bottom.
- No stale row suffixes or label bleed remain after marker-row clearing.

Verification after promotion:

- `make test-vt-origin-mode` ran without `SHUX_ORIGIN_MODE_PROMOTE`.
- `origin-80x24-pixel.json`: `status=pass`, `pixel_diff_ratio=0.0`, `mean_rgba_channel_delta=0.0`.
- `origin-120x40-pixel.json`: `status=pass`, `pixel_diff_ratio=0.0`, `mean_rgba_channel_delta=0.0`.
- `origin-200x60-pixel.json`: `status=pass`, `pixel_diff_ratio=0.0`, `mean_rgba_channel_delta=0.0`.

The promoted images are accepted as task 072 baselines because they were
visually inspected after rendering and then verified in a separate non-promotion
run with exact zero-diff thresholds.
