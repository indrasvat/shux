# Task 067 — shux-vt Resize Reflow: Conservative Design

Design review produced before coding, per the task's Mandatory Process and the
Feature Protocol (council-first). This is the design input to DootSabha; it is
not a substitute for the council run.

## 1. Current behavior (what we are replacing)

`Grid::resize()` (`crates/shux-vt/src/grid.rs:255`) does two independent things:

1. **Columns:** `row.resize(new_cols, Cell::default())` per row — a naive
   truncate (shrink) or pad (grow). On shrink, every cell beyond `new_cols` is
   **dropped with no reflow**. This is the bug: a soft-wrapped logical line that
   spanned rows N and N+1 loses the overflow instead of re-flowing it.
2. **Rows:** shrink pops blank rows off the bottom; grow reclaims scrollback or
   appends blanks.

`VirtualTerminal::resize()` (`lib.rs:183`) calls `Grid::resize` on **both** the
primary grid and `alt_grid`, resizes the frozen `sync_present.grid`, resets the
scroll region, and `cursor.clamp()`s (min-clamp only).

`Row.wrapped` is the only wrap signal and is written in exactly one place:
`parser.rs:122`, `self.grid.visible_row_mut(self.cursor.row).wrapped = true`,
executed at wrap time **while `cursor.row` is still the row that just filled to
the last column**, before the cursor advances.

## 2. BLOCKER — wrapped-flag semantic mismatch (resolve first)

The flag is set on the **source** row (the row that overflowed) and means
"this physical row continues onto the next one" — the *wraps-forward* /
alacritty `WRAPLINE` convention.

Two artifacts disagree with the code:

- `Row.wrapped` doc comment: *"wrapped from the previous line"* — implies the
  flag lives on the **destination** row.
- Task 067 Implementation Notes: *"row `N+1.wrapped == true` … one logical
  line"* — also destination-row phrasing.

The reflow algorithm's line-segmentation is entirely determined by this
convention, so it must be nailed down before any code is written.

**Recommendation:** adopt the code's existing *wraps-forward on source row*
semantic (no change to the hot parser path), and:

- Fix the `Row.wrapped` doc comment to: *"true if this row soft-wrapped into
  the following row (no hard line break after it)."*
- Correct the task wording from "`N+1.wrapped`" to "row `N.wrapped`".
- Add a pinning unit test: write `cols+1` chars into a fresh VT, assert
  `visible_row(0).wrapped == true` and `visible_row(1).wrapped == false`.

Everything below uses the source-row "wraps-forward" convention.

## 3. Conservative algorithm: logical-line reconstruction + re-wrap

Reflow runs over the **entire `raw` buffer** (scrollback ++ visible) because a
wrapped run can straddle the scrollback/visible boundary.

### 3.1 Gating — when we do NOT reflow

- **`new_cols == self.cols`** → no wrapping can change; keep today's row
  grow/shrink path verbatim. (Width-unchanged resize is the common case and
  carries zero reflow risk.)
- **Alternate screen** → never reflow. Alt-screen is a fixed fullscreen canvas;
  vim/htop/btop redraw themselves on `SIGWINCH`. Reflowing it corrupts the next
  redraw. Alt grid keeps the simple truncate/pad + row-clamp path. This also
  trivially preserves the "alt resize must not leak primary scrollback"
  criterion (separate grids; alt has `max_scrollback = 0`).
- **`new_cols == 0 || new_rows == 0`** → clamp to 1; never index an empty row.

So reflow is a **new method on `Grid`** used only for the primary screen; the
alt grid continues to call the existing simple resize.

### 3.2 Segment into logical lines

Walk `raw` top→bottom. A logical line is a maximal run `r0..=rk` where
`r0..=r(k-1)` each have `wrapped == true` and `rk` has `wrapped == false`.

For each logical line build one flat `Vec<Cell>`:

- For a **non-final** segment (`wrapped == true`): it filled the row by
  definition, so take all `old_cols` cells.
- For the **final** segment: take all cells, then drop only **truly default**
  trailing cells (`ch == ' ' && style == default && extended.is_none() &&
  bg == Color::Default`). Cells with a non-default background are kept —
  trailing-bg runs are content. (Trailing *typed spaces* that happen to be
  exactly `old_cols` wide are the one accepted lossy case; documented, matches
  alacritty/wezterm.)

This yields `logical_lines: Vec<Vec<Cell>>` preserving `ch`, `width`, `style`,
and `Arc<ExtendedAttrs>` (hyperlink / underline color / underline style) by
clone — extended attrs survive for free because we move whole `Cell`s.

### 3.3 Re-wrap each logical line at `new_cols`

Emit width-`new_cols` rows:

- Walk the flat cell vector, accumulating display columns until the next cell
  would exceed `new_cols`.
- **Wide-cell integrity:** a wide head (`width == 2`) plus its continuation
  (`width == 0`) are an atomic unit. If a head would land at column
  `new_cols-1` (no room for the continuation), emit a blank pad cell at the end
  of the current row and start the head on the next row. This mirrors the
  parser, which only writes the continuation when `col+1 < cols`.
- Mark `wrapped = true` on every emitted row **except the last** of the logical
  line; the last gets `wrapped = false`.
- Pad the final row to `new_cols` with `Cell::default()` (bg `Color::Default`).

A logical line with no cells (a hard blank line) emits exactly one blank,
non-wrapped row — hard line breaks stay hard.

### 3.4 Reassemble + scrollback discipline

Concatenate all rewrapped rows into the new `raw`. Then:

- Bottom `new_rows` rows = visible; the rest = scrollback.
- Enforce `max_scrollback`: pop from the front until
  `raw.len() <= new_rows + max_scrollback`. (Narrowing width produces *more*
  rows, so shrink-width can push the oldest scrollback out — bounded and
  expected; documented.)
- If `raw.len() < new_rows`, push `Cell::default()` blank rows at the bottom.

Set `self.cols = new_cols; self.rows = new_rows`.

### 3.5 Cursor mapping policy (explicit, content-anchored)

Before rewrap, record the cursor's **logical position**: the index of the
logical line it sits on and the cumulative display-column offset of the cursor
within that line (sum of widths of cells before `cursor.col` across the old
segments). After rewrap, walk the same logical line to find the `(row, col)`
where that offset lands at `new_cols`, convert to an absolute buffer index, then
to visible `(row, col)`.

Rules:
- `auto_wrap_pending` is cleared on reflow (consistent with `Cursor::clamp`).
- End-of-line cursor (shell prompt, the dominant case) lands at the end of the
  rewrapped last logical line — the user-expected outcome.
- If the anchored cell scrolled above the visible top on shrink, clamp the
  cursor to visible row 0.
- `reflow()` returns the new `(row, col)`; `VirtualTerminal::resize` applies it
  for the primary screen instead of the naive `clamp`. Alt screen keeps
  `clamp`.

Document this in a doc-comment and pin it with tests (§6).

## 4. Cross-cutting state

| State | Handling |
|---|---|
| `style` (fg/bg/flags) | Moved with the `Cell`; preserved. |
| `width` / wide head+tail | Kept atomic during rewrap (§3.3). |
| `extended` (hyperlink, underline) | `Arc` clone moves with the `Cell`; preserved. |
| `TerminalDefaultColors` (OSC 10/11/12) | Lives on `VirtualTerminal`, untouched by any grid op — already preserved. Reflow padding uses `Cell::default()` (bg `Color::Default`) so the rasterizer resolves it to the OSC-11 background. **Never** hardcode a pad color. |
| Scrollback ordering / limit | Preserved by construction; trimmed only by `max_scrollback` (§3.4). |
| Alternate screen | No reflow; separate grid; no scrollback leak. |
| Synchronized output (`sync_present.grid`) | Apply the **same** policy as the live grid: reflow if primary screen and cols changed; else simple resize + `cursor.clamp`. The frozen frame is read by `grid()`/`capture_text`/snapshot, so it must stay dimensionally valid and reflowed identically. |
| Scroll region | Reset to full screen after resize (unchanged). |

## 5. API shape (minimal, low-risk)

- Add `Grid::reflow(&mut self, new_rows, new_cols, cursor: (usize,usize)) ->
  (usize, usize)` (returns new cursor). Keep the existing `Grid::resize` for the
  alt screen and the cols-unchanged fast path (or have `reflow` early-delegate
  to `resize` when `new_cols == cols`).
- `VirtualTerminal::resize`: choose `reflow` for primary (+ `sync_present.grid`
  when it represents the primary), `resize`+`clamp` for `alt_grid`.

No change to `Cell`, `Row` fields, or the parser hot path beyond the doc-comment
fix in §2.

## 6. Required evidence (maps the task Testing Matrix to concrete artifacts)

### Unit (`crates/shux-vt/src/grid.rs`, `cursor`/`lib` tests)
- `wrapped` semantic pinning test (§2).
- Reflow shrink: ASCII run across a wrap boundary re-flows, no lost middle.
- Reflow grow: two wrapped rows re-join into one when width grows.
- Style/RGB/flags survive reflow (assert fg/bg/flags on a moved cell).
- Hyperlink + underline (`extended`) survive reflow.
- Wide head+tail stay paired across a boundary bump.
- Scrollback ordering stable; `max_scrollback` enforced after narrowing.
- Alt-screen resize: no reflow, no primary scrollback leak.
- Cursor mapping: end-of-line, mid-line, and above-top-clamp cases.

### Integration (`VirtualTerminal`)
- Reproduce the libghostty spike `resize-reflow` case (31% cell diff today):
  long wrapped text, resize narrower, assert no missing middle text.
- `capture_text()` after resize equals the preserved logical content.
- Round-trip text invariant: `capture_text` of pure-ASCII logical lines is
  stable across `80×24 → 40×12 → 80×24`.

### Synthetic corpus fixtures — the independent oracle
Add fixtures to `.shux/fixtures/vt-corpus/synthetic/manifest.json`:
`resize-reflow-shrink`, `-grow`, `-wide`, `-style`, `-scrollback`,
`-altscreen-noreflow`. **The hand-authored golden text is the
implementation-independent correctness oracle** (authored from VT behavior, not
from program output). Workflow:
1. Author expected text by hand.
2. Implement reflow.
3. Text gate passes *independently* of the renderer.
4. Only then promote the PNG golden (the rasterizer is the already-blessed
   task-073 path), with DootSabha design-review sign-off.

**Harness gap (BLOCKER-lite):** the documented task-073 step schema shows
`expect_text` / `render_png` steps, but the implemented harness
(`crates/shux-raster/examples/vt_corpus_harness.rs`, `SyntheticStep`) only
supports `process` and `resize`; text is compared against committed `.txt`
goldens. Either (a) extend `SyntheticStep` with an inline `expect_text`
assertion (small, preferred — makes the oracle live in the fixture), or
(b) document that the committed `.txt` golden *is* the hand-authored oracle and
author it by hand, not by capturing program output.

### Rich-TUI replays — regression-only guard
The committed `.raw` replays (`btop`, `lazygit`, `nvim`, `vicaya`, `vivecaka`)
contain **no resize steps** (`manifest.json` has zero `resize` entries), so a
correct reflow change must leave their PNG output **byte-identical**. Running
`make test-vt-corpus` and getting exact-match green is the proof that reflow did
not regress steady-state rendering. These goldens are **not** a correctness
oracle for reflow and must not be re-minted in this pass.

Real-TUI-*with-resize* evidence would require a fresh recording (tool install +
DootSabha-blessed new golden). That is optional/blocked; do not self-mint it.

### Shux automation (live RPC)
Launch a pane, write long wrapped text, resize through `80×24 → 120×40 →
40×12`, capture `pane.capture` text and `pane.snapshot` PNG at each step. Assert
`pane.capture` and `pane.snapshot` agree (both read `grid()`). Scratch under
`.shux/out/067-resize-reflow/`; promote the auditable subset to
`.shux/qa/067-resize-reflow/`.

### Pixel
`.claude/automations/pixel_verify.py` with `--max-pixel-diff-ratio 0.0
--max-mean-channel-delta 0.0`:
- Stable resize-return PNGs vs committed/DootSabha-approved baselines.
- Rich-TUI replays exact vs existing 073 goldens (regression guard).
No self-minted implementation baselines.

### QA
`shux-vt-solid-qa` → `VERDICT: PASS` (first line) in
`.shux/qa/067-resize-reflow/SOLID-QA.md`, with `evidence-manifest.json`,
full-res PNGs, and pixel-metric JSON committed.

## 7. Blockers & invariants — summary

**Blockers**
1. Wrapped-flag semantic mismatch (source vs destination row) — resolve and pin
   before coding (§2).
2. Self-minted-baseline rule — new synthetic PNG goldens need a hand-authored
   text oracle + DootSabha blessing; rich-TUI goldens stay byte-identical.
3. Harness `SyntheticStep` lacks `expect_text` — extend or formally treat the
   committed `.txt` as the hand-authored oracle (§6).
4. Pre-existing `vivecaka` OSC-11 background delta (96% pixel diff in the spike)
   is **not** introduced by 067; ensure reflow padding stays `Color::Default` so
   it is not worsened, and interpret the vivecaka replay against its existing
   golden.

**Invariants**
- Reflow runs only on the primary grid, only when `cols` changes.
- Alt screen never reflows and never shares scrollback.
- `wrapped` travels with the `Row` through scroll/scrollback (already true).
- Padding cells are `Cell::default()` (bg `Color::Default`).
- A wide head is never placed in the final column without its continuation.
- `max_scrollback` enforced after reflow.
- `capture_text` and snapshot both read `grid()` → agreement is structural;
  still asserted post-reflow.
- `TerminalDefaultColors` is untouched by grid operations.
