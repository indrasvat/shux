Review shux task 072 design for VT origin mode and scroll-region semantics.

Current code facts:
- Cursor stores absolute visible-grid `row,col`.
- `TerminalModes::origin_mode` exists and DECRQM `?6` reports it.
- `Cursor::save()` stores origin mode; restore returns it.
- `ScrollRegion` is used for LF/RI/SU/SD.
- Gaps: CUP/HVP/VPA ignore origin mode; DECSET/DECRST `?6` does not home;
  DECSTBM always homes to absolute row 0; tests are thin.

Proposed semantics:
- Keep cursor storage absolute.
- In origin mode, row params for CUP/HVP/VPA are relative to
  `scroll_region.top` and clamp within `top..=bottom`.
- Outside origin mode, CUP/HVP/VPA remain absolute full-screen addressing.
- HPA/CHA and relative movement stay unaffected by origin mode.
- DECSET `?6` enables origin mode and homes to scroll-region top, col 0.
- DECRST `?6` disables origin mode and homes to row 0, col 0.
- DECSTBM validates `top < bottom`; on success updates margins and homes using
  the current origin mode. Invalid regions leave margins/cursor unchanged.
- DSR cursor reports absolute visible coordinates.
- DECRQM `?6` reports set/reset.
- Save/restore restores both absolute cursor and origin mode; restored cursor
  is clamped to the current grid/region.
- 1049 alternate screen continues using existing save/restore behavior.
- Synchronized output freezes presented grid/cursor while working origin-mode
  writes are pending; release shows the addressed working frame.

Planned evidence:
- Unit tests for CUP/HVP/VPA in/out of origin mode, row clamping, DECSET/DECRST
  homing, DECSTBM homing/invalid no-op, save/restore, DECRQM `?6`, alt-screen,
  sync-output presentation freeze.
- Synthetic corpus fixture with fixed header/footer and scrollable body.
- Shux PNG automation at 80x24, 120x40, 200x60 with zero pixel tolerance.
- SOLID QA PASS under `.shux/qa/072-shux-vt-origin-mode-scroll-region/`.

Return only severity-labelled findings and missing must-fixes. End with
`VERDICT: APPROVE`, `VERDICT: APPROVE-WITH-CONDITIONS`, or `VERDICT: REJECT`.
