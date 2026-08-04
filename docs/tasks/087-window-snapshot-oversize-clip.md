# 087 — window snapshot renders a pane blank when its grid exceeds the layout rect

**Status:** Done
**Priority:** High (core advertised render path; silent content loss — issue #108)
**Milestone:** M3 polish
**Depends On:** 017 (multi-pane compose), 077 (lens render paths)
**Touches:** `crates/shux-ui/src/viewport.rs` (new), `crates/shux-ui/src/composed.rs`,
`crates/shux-ui/src/compositor.rs`, `crates/shux/tests/window_snapshot_oversize.rs` (new)

---

## Problem (issue #108)

`window snapshot` renders a pane **blank** (borders/title/status bar intact, a
lone cursor block in an otherwise empty content area) whenever the pane's grid
is **taller** than the window layout rect it is composited into. At the same
instant `pane snapshot` and `pane capture` on the same pane return its full
content. The documented `pane set-size --cols/--rows` path (advertised for
"wider/taller than the daemon default") therefore silently drops content in one
render path with no error and a structurally valid PNG.

Reproduced with the real binary on `claude/issue-108-fix` HEAD: an
`OVERSIZE-MARKER` printed at the top of a 200×60 pane shows in `pane snapshot`
but the `window snapshot` (default 120×36) content area is blank except a cursor.

### Root cause

Both compose paths — `composed::compose_pane` (snapshot) and
`compositor::compose_pane` (live attach) — pick

```rust
let row_offset = total_rows.saturating_sub(visible_rows);
```

i.e. they **bottom-anchor** the pane grid into the rect (render the last
`visible_rows` rows). But the **cursor** is mapped with a **top-clamp**
(`cur.row.min(rect.height-1)`). The two disagree: with content and cursor at the
top of an oversized grid, the bottom-anchored content window is blank while the
cursor still paints at its clamped top position → "blank with a cursor". The two
render paths are internally inconsistent, and `window snapshot` disagrees with
`pane snapshot`.

## Fix — one cursor-following viewport, shared by both paths

New `shux_ui::pane_view_row_offset(total_rows, visible_rows, cursor_row)`:

- whole grid fits (`total_rows <= visible_rows`) → offset `0`;
- cursor within the first `visible_rows` rows → offset `0` (top-left region — the
  reported case: content at top now shows);
- cursor below that → scroll just enough to keep the cursor on the last visible
  row (`min(cursor_row + 1 - visible_rows, total_rows - visible_rows)`), so a
  shell prompt's most-recent output stays visible.

Both `compose_pane` implementations and both cursor-mapping sites use this single
offset, so content and cursor always agree and the snapshot/attach paths stay
consistent. Columns remain left-anchored (top-**left** region), matching the
issue's stated preference. This strictly dominates the old behavior: identical
when the grid fits or the cursor is at the bottom (the documented "recent output"
case), fixed when content/cursor sit at the top.

## Testing matrix / DoD

- **Unit** (`shux-ui`): `pane_view_row_offset` truth table (fits, oversize+top,
  oversize+bottom, oversize+mid, zero-height, cursor past end).
- **Unit** (`composed.rs`): oversized pane with top content composes its content
  (red-first: blank before fix); cursor-at-bottom keeps recent output; split
  layout with an oversized child shows content.
- **Unit** (`compositor.rs`): `render_multi_pane` with an oversized pane emits the
  top content bytes (red-first).
- **Integration** (`crates/shux/tests`, daemon-backed): the acceptance cross-path
  test — at a geometry where the grid exceeds the rect, `window snapshot` and
  `pane snapshot` **agree on presence of content** (pixel-level), colour-probed;
  the `pane split` variant (rect shrinks below the unchanged grid) is covered by
  the same test; zero leaked daemons.
- Real-target dogfood with the reproduction script; full-resolution PNG
  inspection; visual proof in the PR (Claude Artifact — cloud/headless).

## Acceptance (from #108)

- A pane whose grid exceeds the window layout rect renders its content — clipped,
  not blank — in `window snapshot`.
- A cross-path test asserting `window snapshot` and `pane snapshot` agree on
  presence of content for the same pane at the same revision, at a geometry where
  the grid exceeds the rect.
- The `pane split` case is covered by the same test (same cause).

## Verification record

- **R→G TDD:** every test seen failing first — the three shux-ui unit tests and both
  daemon-backed acceptance tests were run red on the bottom-anchor (the acceptance
  test proven red by temporarily restoring `total_rows - visible_rows`), then green.
- **Full `make test`** green across the workspace; lint clean; zero leaked daemons.
- **Adversarial sweep** (real fixed binary, A/B vs a pre-fix worktree binary), 23/24
  checks across 6 surfaces, no product defects:
  - geometry matrix (grid rows 24→500; grids wider than the window; tiny 4×3 / 6×4
    windows; grid-fits) — `window` vs `pane` snapshot agree on content presence;
  - cursor at top / middle / bottom of an oversized grid (bottom-anchor verified
    end-to-end: `BOTTOM-RECENT-OUTPUT` renders at the window's bottom band, top band
    empty);
  - real apps in an oversized pane — `vim` (alt screen), `less` (alt screen), `top`;
  - horizontal + vertical splits and zoom on an oversized pane;
  - determinism: two `window snapshot`s 3 s apart are byte-identical;
  - A/B: fixed shows content where the pre-fix binary is blank.
  (The single "fail" was a probe-region artifact in the sweep harness, not the product;
  separately re-verified correct.)
- **Visual proof:** before/after evidence matrix captured at full resolution and
  showcased in a Claude Artifact linked from the PR. No screenshots committed
  (`.shux/out` scratch discipline).
