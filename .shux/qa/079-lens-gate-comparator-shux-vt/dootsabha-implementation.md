# Task 079 — DootSabha implementation-diff review (raw: dootsabha-raw/impl.json)

**agy: CLEAN** (no findings). **codex: CHANGES-NEEDED, MINOR only.**
- MINOR: `GridFrame::cell()` panicked on an out-of-range row (`visible_row()` before bounds check),
  violating the trait contract ("out-of-range → `Cell::EMPTY`"). `diff_frames` never hit it (loops
  `min` dims) but `CellGridView` is public API. **FIXED**: `grid.row(scrollback_len()+row).and_then(get(col)).unwrap_or(EMPTY)`;
  pinned by `out_of_range_cell_is_empty_for_both_views`.

Both reviewers confirmed CLEAN: faithful lift (span-merge/wide-pairing/default-color/truncation/bbox),
byte-identical daemon output (new fields stay false + unserialized; no borrow/lifetime hazard with
`cur_grid` moved into the heat closure after the block-scoped `GridFrame` borrows), correct scrollback
fix, sound `try_view` validate+cap, meaningful non-self-referential parity + divergence pins.
