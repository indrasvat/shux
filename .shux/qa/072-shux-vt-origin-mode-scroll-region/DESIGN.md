# Task 072 Design: Origin Mode and Scroll-Region Semantics

## Current Gaps

`shux-vt` already tracks `TerminalModes::origin_mode`, reports DECRQM `?6`,
stores origin mode in saved cursor state, and uses `ScrollRegion` for
linefeed/reverse-index/scroll-up/down. The missing behavior is in cursor
addressing and reset side effects:

- CUP/HVP (`CSI row;col H/f`) always address absolute screen rows.
- VPA (`CSI row d`) always addresses absolute screen rows.
- DECSET/DECRST `?6` toggles origin mode without moving the cursor to home.
- DECSTBM (`CSI top;bottom r`) always homes to absolute row 0.
- Cursor clamp after scroll-region changes does not account for origin-mode
  row limits.

## Target Semantics

Model origin mode as a cursor-addressing policy, not a grid-storage policy.
The cursor always stores absolute visible-grid coordinates. When origin mode is
enabled, row parameters for origin-aware absolute movement are interpreted
relative to `scroll_region.top` and clamped within
`scroll_region.top..=scroll_region.bottom`.

### Helpers

Add small helpers on `VtHandler`:

- `origin_top() -> usize`: `scroll_region.top` when origin mode is enabled,
  otherwise `0`.
- `origin_bottom() -> usize`: `scroll_region.bottom` when origin mode is
  enabled, otherwise `grid.rows() - 1`.
- `addressed_row(param, default) -> usize`: one-based VT row parameter converted
  to absolute row, applying origin mode and clamping to the active addressing
  range.
- `home_cursor()`: move to `origin_top(), 0` and clear `auto_wrap_pending`.
- `relative_vertical_bounds()`: if the cursor is currently inside the scroll
  region, return `scroll_region.top..=scroll_region.bottom`; otherwise return
  the full screen. Use this for relative vertical movement clamping.

### Cursor Movement

- CUP/HVP use `addressed_row(p(0, 1), 1)` and normal absolute column clamping.
- VPA uses `addressed_row(p(0, 1), 1)` and preserves column.
- HPA/CHA remain column-only and unaffected by origin mode.
- CUU/CUD/CNL/CPL/VPR remain relative movement, but when the cursor starts
  inside the scroll region they clamp to the scroll margins instead of crossing
  into fixed header/footer rows. They do not reinterpret parameters through
  origin mode.
- IND/RI/NEL already use linefeed/reverse-index behavior and continue to scroll
  only inside the active scroll region at its boundaries.
- DSR/CPR (`CSI 6 n`, `CSI ? 6 n`) reports the row relative to the scroll-region
  top while origin mode is enabled; otherwise it reports absolute visible-grid
  coordinates. Columns remain absolute because shux does not implement DECSLRM
  left/right margins yet.

### Mode Side Effects

- DECSET `?6` enables origin mode and homes to the scroll-region top.
- DECRST `?6` disables origin mode and homes to absolute row 0.
- DECRQM `?6` continues returning set/reset status.

### Scroll Region

- DECSTBM validates top < bottom, updates the region, then homes using the
  current origin mode. That means row 0 when origin mode is reset, and
  `scroll_region.top` when origin mode is set.
- Invalid DECSTBM parameters leave the existing region intact and do not move
  the cursor.
- `VirtualTerminal::resize()` continues to reset scroll region to the full
  screen, then clamps cursor. If origin mode remains enabled, the full-screen
  region makes origin-home equivalent to absolute home; tests should pin the
  current resize behavior rather than introducing extra mode resets.
- RIS resets modes and scroll region to defaults.

### Save/Restore and Alternate Screen

- Existing `Cursor::save(origin_mode, charsets)` remains the storage point for
  DECSC/DECRC and CSI s/u.
- Restoring cursor state restores both absolute cursor coordinates and
  `origin_mode`, then clamps the restored cursor to the current grid bounds, not
  the current scroll region. This matches xterm-style saved page coordinates and
  avoids moving a saved cursor just because margins changed after save.
- DECSET/DECRST 1049 behavior remains unchanged: 1049 save/restore includes
  origin mode through `save_cursor_state()` / `restore_cursor_state()`.
- Alternate screen switches do not reset scroll region or origin mode unless
  the application emits the corresponding sequences.
- ED/EL and other erasure commands continue operating on their normal absolute
  screen/line scopes; origin mode affects addressing, not erase extents.

### Synchronized Output

Synchronized output needs no special origin-mode state. It freezes a
presentation snapshot of grid/cursor while working origin-mode-addressed writes
are pending. Until `DECRST ?2026`, capture/snapshot should continue showing the
frozen presented cursor/grid; after release, the addressed working frame becomes
visible.

## Evidence Plan

- Unit tests for CUP/HVP/VPA inside and outside origin mode.
- Unit tests for DECSET/DECRST `?6` homing behavior.
- Unit tests for save/restore restoring origin mode and absolute cursor.
- Unit tests for DECSTBM homing under origin mode and invalid-region no-op.
- Unit tests for CPR/DSR row reporting in origin mode.
- Unit tests for CUU/CUD/CNL/CPL/VPR clamping to scroll margins when the cursor
  starts inside the region, and not clamping when it starts outside.
- Unit/response test for `process_with_responses()` DECRQM `?6`.
- Synthetic corpus fixture drawing header/body/footer with scroll region and
  origin-mode addressing.
- Shux automation rendering the fixture at 80x24, 120x40, and 200x60 with exact
  pixel comparison to committed goldens.
- A synchronized-output test that freezes old content while origin-mode writes
  are pending, then reveals the correctly addressed frame on reset.

New scroll-margin visual baselines require post-render approval. The design
review approves the baseline process, not the pixels themselves. Promotion
(`SHUX_ORIGIN_MODE_PROMOTE=1`) stays separate from normal verification, and the
promoted PNGs must be visually inspected plus exact-pixel verified before the
SOLID QA gate can pass.
