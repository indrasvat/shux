You are a terminal-emulator design council reviewing the design for shux task 071: "Real Tab-Stop State". shux is a Rust terminal multiplexer; `shux-vt` is its VT emulator (vte-crate parser feeding a VecDeque grid). The north star is xterm compatibility and pixel-perfect snapshots. Be concrete and adversarial. Return findings with SEVERITY (BLOCKER / MAJOR / MINOR / NIT), each with a concrete fix. End with an overall verdict: APPROVE / APPROVE-WITH-CONDITIONS / REJECT, and an explicit list of MUST-FIX items before implementation.

Evaluate specifically:
1. Is the Default-vs-Explicit tab-stop state model faithful enough for xterm HT/HTS/TBC/CHT/CBT behavior? Are there edge cases the model mishandles (column 0, last column, width-1 grids, tab beyond last stop, no stops at all)?
2. Are the resize semantics correct — clear-all preservation, custom-stop preservation, default recomputation, width extension? Does the Default-vs-Explicit split actually capture real xterm resize behavior, or is it an oversimplification?
3. Are parser integration points complete — RIS, DECSTR (soft reset), alternate screen, DECSC/DECRC, the existing `execute(0x09)` hardcoded path, CHT('I')/CBT('Z'), HTS(ESC H), TBC(CSI g)?
4. What unit/integration/visual/pixel tests are missing?
5. What scope boundaries should be explicitly REJECTED for this task (gold-plating)?

== TASK SPEC (docs/tasks/071-shux-vt-tab-stops.md) ==

Problem: shux-vt currently assumes fixed 8-column tab stops and ignores mutable tab stop state. Real terminals support setting/clearing tab stops with HTS/TBC. Some applications depend on this for table alignment.

Scope: default tab stops every 8 columns; HTS (ESC H) sets a stop at current column; TBC (CSI 0 g) clears current stop; TBC (CSI 3 g) clears all; resize preserves valid tab stops and restores defaults only when appropriate.

Testing matrix requires: unit (default 8-col, HTS+HT, TBC clear current/all, resize clamps/removes stops beyond width without corrupting remaining), integration (table fixture aligns in capture_text), shux automation (render at 80x24, 120x40, after resize), visual (column drift / wrap artifacts), pixel (exact match to committed goldens with zero tolerance), QA (shux-vt-solid-qa VERDICT: PASS).

Acceptance: mutable tab stops behave like xterm for common HTS/TBC; default unchanged when no custom stops; capture and snapshot agree on aligned columns.

== DESIGN (.shux/qa/071-shux-vt-tab-stops/DESIGN.md) ==

State Model: Add a `TabStops` value owned by VirtualTerminal, threaded into VtHandler like charset state.
- Default state has stops at columns 8,16,24,... within current width. Column 0 is not a default stop.
- Stores explicit stop columns as zero-based usize.
- HT, CHT, CBT consult the same state. HTS inserts current cursor column. TBC 0 removes current cursor column. TBC 3 removes all. Unsupported TBC params ignored.
- Parser clears active grapheme anchor before every tab movement or tab-state mutation.

Resize Semantics (tab stops are terminal state, not grid content):
- On resize, remove stops outside new width. Preserve every remaining custom stop.
- If state is still default, recompute default stops for new width.
- If all stops explicitly cleared, do not recreate defaults on resize.
- If custom stops exist, extending width does not silently add new default stops.
Model: kind = Default (every-8 derived from width) | kind = Explicit(BTreeSet<usize>) after HTS or TBC. In Default movement computed arithmetically; in Explicit uses stored set.

Movement Rules:
- HT == one forward tab. CHT Ps moves to Ps-th next stop (default 1). No next stop -> clamp to last column.
- CBT Ps moves to Ps-th previous stop (default 1). No previous stop -> clamp to column 0.
- All tab movement clears auto_wrap_pending.

Integration Points: VirtualTerminal owns tab_stops; resize() calls tab_stops.resize(cols); VtHandler borrows tab_stops; execute(0x09) uses next_tab_col(1); csi_dispatch('I')/('Z') use same helpers; csi_dispatch('g') handles TBC; esc_dispatch(b'H') handles HTS; RIS restores default tab stops; alternate screen switches should NOT reset tab stops.

Baseline Governance: visual fixture includes default 8/16, custom non-8 stops, current-stop clear, clear-all, resize preservation across 80x24/120x40/return-to-80x24. Expected PNGs in .shux/goldens/071-tab-stops/. Promote only with SHUX_TAB_STOPS_PROMOTE=1 after this design reviewed.

Required Tests: unit (default HT compat, HTS+HT lands, TBC 0 clears current, TBC 3 clears all + HT clamps to last col, CHT/CBT honor custom stops+counts, resize preserves explicit within bounds + removes out-of-range without restoring defaults, RIS restores defaults); integration (real pane capture shows custom tab-aligned columns); shux automation (PNG+text for 80x24/120x40/return, exact pixel zero tolerance).

== CURRENT IMPLEMENTATION FACTS (crates/shux-vt/src/parser.rs, lib.rs) ==

- execute(0x09) HT: `let next_tab = (self.cursor.col / 8 + 1) * 8; self.cursor.col = next_tab.min(self.grid.cols() - 1); self.cursor.auto_wrap_pending = false;`
- next_tab_col(count): loops `col = (col/8 + 1)*8`, then `.min(cols-1)`. Used by CHT('I').
- prev_tab_col(count): loops `col = col.saturating_sub(1); col = (col/8)*8`. Used by CBT('Z').
- TBC ('g', []) => {} (explicit no-op, comment says no mutable tab state yet).
- HTS (b'H', []) => {} (explicit no-op, comment says accepting avoids unknown-byte warnings).
- RIS (b'c', []): clears grid+scrollback, resets cursor/modes/default_colors/charsets/scroll_region. No tab state.
- DECSTR / soft reset (CSI ! p) is NOT currently handled (no match arm seen) — worth confirming whether 071 should add it.
- VtHandler borrows fields individually (grid, cursor, modes, scroll_region, charsets, etc.) — adding tab_stops follows same pattern.
- resize() in lib.rs handles primary/alt/sync grids, clamps cursors, resets scroll_region to full height. No tab state today.
- Alternate screen enter/leave swaps grids+cursors only; modes.alternate_screen toggled. Tab stops would naturally persist (single shared field) — design says that's correct.
- An existing fixture `tabs-current` exists in .shux/fixtures/vt-corpus/synthetic/manifest.json: rows 8 cols 40, processes "a\tb\r\nlong\tc".
- VtHandler has `responses: &mut Vec<Vec<u8>>` for query/response; no DECST tab query exists.

Known correctness concerns to weigh in your review:
- xterm clamps HT to the last column but the cell at the last column: does CHT-with-no-next-stop land on cols-1 or cols-1 exactly matching current `.min(cols-1)`? Confirm consistency between execute(0x09) and next_tab_col after refactor.
- Does HTS at column 0 create a stop at 0 (xterm allows a stop at col 0)? The design says "column 0 is not a default stop" but HTS could explicitly set one.
- On width SHRINK then GROW back, Default mode must regenerate identical 8-col stops; Explicit mode must NOT regenerate. Is BTreeSet<usize> + kind enough, or do you need to retain stops above old width (xterm discards stops beyond the screen on resize — confirm)?
- TBC with no explicit param: CSI g === CSI 0 g. Confirm default param handling matches the p(0,0)-style default used elsewhere.
