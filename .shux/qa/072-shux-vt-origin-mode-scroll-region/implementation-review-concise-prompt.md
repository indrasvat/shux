Review shux task 072 for merge-blocking defects.

Implementation summary:
- `VtHandler` now keeps cursor storage absolute.
- `DECSET/DECRST ?6` sets `origin_mode` and homes to scroll-region top or screen home.
- CUP/HVP/VPA use an `addressed_row()` helper that interprets row params relative to `scroll_region.top` only under DECOM and clamps to the active origin bottom.
- CPR/DSR reports row relative to scroll-region top under DECOM.
- DECSTBM accepts only `top < bottom`; accepted regions home through the current origin policy; invalid regions do not move cursor or mutate margins.
- CUU/CUD/CNL/CPL/VPR clamp to scroll margins when the cursor starts inside the active scroll region, otherwise use full-grid bounds.
- DECRC restores saved origin mode and saved absolute row/col, then clamps only to current grid bounds.
- Tests cover origin CUP/VPA, non-origin CUP, homing, invalid DECSTBM, save/restore across changed margins, DSR/CPR, relative vertical moves, synchronized output, corpus replay, and shux PNG automation.
- Visual automation renders fixed header/footer and body at 80x24/120x40/200x60 with exact zero-diff pixel checks. Baseline promotion uses `SHUX_ORIGIN_MODE_PROMOTE=1`; normal proof does not mint expected PNGs.

Relevant changed files:
- crates/shux-vt/src/parser.rs
- crates/shux-vt/src/lib.rs
- .shux/fixtures/vt-corpus/synthetic/manifest.json
- .shux/scripts/origin_mode_check.sh
- Makefile

Look for blockers only: VT semantic errors, missed task requirements, tests that do not actually prove the claim, or visual QA process gaps. Return concise findings. If mergeable, say mergeable.
