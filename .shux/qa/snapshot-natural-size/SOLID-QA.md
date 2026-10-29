VERDICT: PASS

# shux-vt-solid-qa — snapshot-natural-size

## Change under audit
- Branch claude/project-thread-58qznu. Audited SHA: **373dd97**.
- Commits in scope:
  - 40878bf: frame a window at its panes' real size.
  - 0edab07: fitted tree, no 1000 clamp, dims validation, cap hint.
  - 9ba0b65: fit the defaulted axis when only one dim is given.
  - 84307b6: raster clips a glyph's ink to its own cell row.
  - 373dd97: one recursion for the fitted ratios and sizes; `natural_frame` always returns a tree, which is composed only when an axis is defaulted.
- Out of scope, gated by the TUI gate: e890a72 and 49a3bdf (attach cursor handling under copy mode and overlays; `compositor.rs` `restore_cursor`). Neither touches a snapshot path.
- No PR or issue was named. The DoD below was derived from the commit messages and the parent's charter.

| Round | Commit audited | Verdict | Findings |
|---|---|---|---|
| 1 | 40878bf (at HEAD 1c17968) | FAIL | F1-F5 |
| 2 | 21de4cf | FAIL | F6, F7 |
| 3 | 84307b6, then 373dd97 | PASS | none; every layout/snapshot cell and the raster A/B were re-run on 373dd97 |

- Each round used its own `XDG_RUNTIME_DIR` and `XDG_CONFIG_HOME`.

## DoD matrix (at 373dd97, real binary from `make build`)
Every pane ran a colour probe:
- truecolor background on row 0;
- indexed-256 background on the body rows;
- basic-SGR background on the last row;
- white edge columns, so clipping or a dead band shows as a missing edge column.

"Exact" means `pane.snapshot` equals the pane's box cropped from `session.snapshot` under `pixel_verify.py` at 0/0. Every metric listed is in this folder and passes 0/0.

| Promise | Result | Metric(s) |
|---|---|---|
| Single pane, no dims | 82x27 frame, pane exact | pixel-single-80x24 |
| F1: 3-way vertical chain at detached defaults | 244x27, no dead band, A, B and C exact; window snapshot equals session snapshot | pixel-chain3-{A,B,C}, pixel-chain3-window-vs-session |
| F1: `pane set-size` 100x30 beside 80x24 | 183x33, both exact; 6 slack rows under B are background only | pixel-setsize-A-100x30, pixel-setsize-B-80x24 |
| F1: `pane resize --delta 0.2` | 163x27, both exact | pixel-resize-{A,B} |
| F2: V(A 80x24, H(B 80x12, C 80x5)) | All three exact, nothing cropped; 4 slack rows under B and 2 under C, background only, inside each pane's rect | pixel-nested-{A,B,C} |
| F3: 1000x80 pane, no dims | 1002x83 frame, whole pane exact | pixel-wide-1000x80 |
| F4: RPC cols/rows `"80"`, `80.5`, `-5`, `true`, `-1`, `"x"` | InvalidParams ("'cols' must be a whole number of cells") on session.snapshot and window.snapshot; `null`, `{}` and integers accepted | transcript (12 rejections) |
| F5: over the pixel cap with no dims | Error names `--cols/--rows`; with explicit dims, no hint | transcript |
| F6: `--rows 40` on the chain | 244x40, fitted, A, B and C exact; 14 slack rows under each, background only | pixel-f6-rows40-{A,B,C} |
| F6: `--cols 300` on the chain | Fitted offsets 1/101/201, all exact | pixel-f6-cols300-{A,B,C} |
| F6: `--cols 200` on a 2-pane vertical split | Both exact | pixel-f6-two-cols200-{A,B} |
| F6: RPC `{"rows":30}` | 244 cols, pane C exact | pixel-f6-rpc-rows30-C |
| F7: `border_style = "none"` single pane, cwd in a repo so the status bar draws U+E0A0 under the pane | Exact, including the pane's last pixel row. The same cell at 21de4cf: 3 px differ | pixel-f7-repo-cwd-none-single, pixel-none-single, pixel-f7-none-single |
| F7: none, split pane A and chain A/B/C | Exact, including the last row | pixel-f7-repo-cwd-none-split-A, pixel-none-chain3-{A,B,C}, pixel-f7-none-chain3-{A,B,C} |
| F7: zoomed 90x30 | Exact, including the last row. At 21de4cf: 3 px differ | pixel-f7-repo-cwd-zoom-90x30, pixel-zoom-90x30, pixel-f7-zoom-90x30 |
| Real TUI: vim with syntax colours in a split, focused | Exact | pixel-vim-split-focused |
| Both dims explicit (`--cols 120 --rows 36`) | Live layout at 120x36, unchanged behaviour | transcript |
| Border styles none/single/double/rounded/ascii, hot-reloaded | Ring 0 or 2 as expected (round 1); none re-checked at 373dd97 | as above |

## F2 re-scope (design, stated in the PR)
- Detached panes keep their own grid sizes, so no tiling of unequal grids avoids slack on the short side across a split.
- Verified at 373dd97 that the slack is:
  - the only remaining gap;
  - drawn inside the pane's own rect, below its content, never as a frame larger than the layout;
  - background only.
- No pane is cropped.
- Agents that want no slack size their panes with `pane set-size`.

## Raster A/B (same bytes, base 21de4cf vs head 373dd97; `pane.snapshot` at 100x30 or 100x40)
- **Fixture:** rich.txt contains:
  - box drawing (light, heavy, double, rounded) and block elements;
  - powerline and nerd glyphs (U+E0A0, U+E0B0, U+E0B2, U+E0B4, U+E0B6, U+F015, U+F07C, U+E7A8, U+F113);
  - CJK, emoji (ZWJ, skin tone, flag, VS16);
  - combining marks;
  - bold, italic overhang, underline (including curly coloured), strikethrough, reverse, dim and overline;
  - tall and descender glyphs next to a red background row;
  - wide glyphs on a background.
- **vim with syntax colours (probe.sh):** 0 changed pixels (pixel-raster-ab-vim-syntax-21de4cf-vs-373dd97).
- **Dense fixture:** 175 changed pixels. **vim on the plain glyph file:** 158 changed pixels.
  - Every changed pixel is in the bottom 1-2 pixel rows of a cell.
  - In each case the glyph in the row below is a box-drawing, block, powerline, or tall/integral glyph.
  - At base, that glyph's ink overran its own cell top and was painted over the cell above.
  - At head, each cell shows only its own ink. Line continuity across rows is kept: the vertical line changes colour exactly at the cell boundary instead of 2 px early (checked on 8x magnified crops).
- **Spaced fixture (a blank row between every content row):** 175 changed pixels, all of them in blank rows, all now that row's background. Content rows are pixel-identical.
  - So the change removes bleed into neighbour rows and nothing else: no own-cell ink changed, horizontal italic overhang is unchanged, and underline and strikethrough are unchanged.
- **Pre-existing, identical in both builds, outside this change:** CJK and some emoji render as tofu in the bundled font.

## Testing matrix (at 373dd97)
| Layer | Result |
|---|---|
| Unit | shux-core `fitted` 3/3; shux-raster 31/31, including `a_glyph_never_inks_the_row_above_or_below`; shux-ui 286/286 |
| Integration | `make test-focus PKG=shux FILTER=snapshot` 24/24, including the chain, one-dim and size tests; `a_wide_window_is_not_clipped_to_the_explicit_dims_limit` 1/1; `a_window_over_the_pixel_cap_names_the_dims_that_get_under_it` 1/1 |
| Raw byte / replay | Same fixture bytes and vim files through both builds (A/B above) |
| shux automation | Real daemon, real panes, colour probes, vim |
| Visual inspection | Every PNG asserted on was opened, plus magnified base/head crops |
| Pixel metrics | 37 metrics, all exact 0/0 |
| Negative controls | Crop shifted one cell: fails (13486 px). F7 cells at 21de4cf: fail (3 px) |
| DootSabha | Unavailable; substitution recorded in council-record.md |

## Residual risk
- No committed pixel baseline exists for this feature and none was minted. Metrics are cross-path (pane vs window) and base-vs-head.
- Only vim is installed; nvim, htop, btop and lazygit are absent here.
- The transient while an attached client's split is still resizing was not exercised. Detached splits reproduce the same disagreement permanently and are covered.

## Cleanup
- Every audit daemon was stopped and verified gone by pidfile.
- `ps` shows no process from this checkout's binaries, the audit runtime dirs, or the probe script.
