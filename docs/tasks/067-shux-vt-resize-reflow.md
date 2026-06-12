# Task 067: shux-vt Resize Reflow

**Status:** Not Started
**Priority:** High
**Milestone:** VT Quality Track
**Depends On:** 005, 016, 066, 073
**Touches:** `crates/shux-vt/src/grid.rs`, `crates/shux-vt/src/lib.rs`, `crates/shux-raster`, `.shux/scripts/`, `.shux/qa/067-resize-reflow/`

---

## Problem

`Grid::resize()` currently truncates or extends rows. The grid already tracks
`Row.wrapped`, but column resizing does not use wrapped runs to reconstruct
logical terminal lines. The libghostty spike exposed the consequence: after a
pane resize, current `shux-vt` can lose intervening wrapped content while a
more complete VT preserves and reflows it.

For agents, this is correctness, not polish. If pane content disappears after a
resize, `pane.capture`, `pane.snapshot`, and visual QA can all inspect the wrong
state.

Additionally, OSC 10/11/12 default color assignments need to be preserved
and accurately rendered, which was a highlighted regression risk in the spike.

## Scope

Implement reflow for soft-wrapped visible and scrollback rows when terminal
columns change.

In scope:

- Preserve logical lines across grow and shrink.
- Use existing `Row.wrapped` as the source of truth for soft-wrap runs.
- Preserve cell style, width metadata, extended attrs, default colors, cursor
  clamping, scrollback limits, and alternate-screen behavior.
- Handle resize under synchronized output presentation state.
- Ensure OSC 10/11/12 default color settings are maintained and applied correctly.

Out of scope:

- Grapheme-cluster storage beyond existing cell model.
- Hard-wrapped lines not marked by `Row.wrapped`.
- Changing raster font/shaping behavior.

## Implementation Notes

- Treat consecutive rows where row `N+1.wrapped == true` as one logical line.
- Reflow only soft-wrapped runs; hard line breaks remain hard line breaks.
- Preserve wide-cell head/tail integrity while wrapping.
- Keep scrollback ordering stable and trim only according to `max_scrollback`.
- Cursor mapping must be explicit: document and test where the cursor lands
  after reflow.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa` for an independent hard-gate review.
- Save auditable task artifacts under `.shux/qa/067-resize-reflow/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | `Grid::resize()` reflows wrapped ASCII runs on shrink and grow. |
| Unit | Reflow preserves styles, RGB colors, hyperlinks/extended attrs, and wide-cell continuations. |
| Unit | Reflow preserves scrollback ordering and respects `max_scrollback`. |
| Unit | Alternate screen resize does not leak primary scrollback. |
| Integration | `VirtualTerminal::resize()` reproduces the libghostty spike case: no missing middle text. |
| Integration | `capture_text()` after resize returns the preserved logical content. |
| Shux automation | Launch a pane, write long wrapped text, resize through 80x24, 120x40, 40x12, and capture text + PNG. |
| Real TUI | Replay committed `.raw` PTY fixtures from `.shux/fixtures/vt-corpus/rich-tui/` for `btop`, `lazygit`, `nvim`, `vicaya-tui`, and `vivecaka` via harness (mandatory). Refreshing requires installation. |
| Visual | Inspect full-resolution PNGs for lost rows, clipped text, ghost wide cells, and cursor artifacts. |
| Pixel | Compare before/after stable resize-return PNGs with `.claude/automations/pixel_verify.py` using `--max-pixel-diff-ratio 0.0` and `--max-mean-channel-delta 0.0`; exact match required against committed `.shux/goldens/` or DootSabha-approved `.shux/qa/067-resize-reflow/` baselines. Self-minted implementation baselines are not allowed. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/067-resize-reflow/SOLID-QA.md`. |

## Acceptance Criteria

- [ ] Soft-wrapped text survives shrink and grow without dropping intervening content.
- [ ] Hard line breaks remain hard line breaks.
- [ ] Styles and extended attrs survive reflow.
- [ ] Wide-cell head/tail pairs remain valid after reflow.
- [ ] Scrollback limits remain enforced.
- [ ] OSC 10/11/12 default color settings apply correctly and survive resize.
- [ ] `pane.capture` and `pane.snapshot` agree on visible content after resize.
- [ ] Real TUI corpus has no visual regressions or documented intentional deltas.

## Definition of Done

- [ ] DootSabha design council evidence saved under `.shux/qa/067-resize-reflow/`.
- [ ] Implementation-diff DootSabha review saved and clean or addressed.
- [ ] Focused unit/integration tests pass through Make targets.
- [ ] Real shux automation artifacts include text captures, PNGs, and pixel diffs.
- [ ] Full-resolution PNGs, pixel metric JSON, and `evidence-manifest.json` are committed under `.shux/qa/067-resize-reflow/`.
- [ ] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/067-resize-reflow/SOLID-QA.md`.
- [ ] `make check` passes.
- [ ] `docs/PROGRESS.md` and this task status are updated.
- [ ] New learnings are appended to `docs/agents/learnings.md`.
