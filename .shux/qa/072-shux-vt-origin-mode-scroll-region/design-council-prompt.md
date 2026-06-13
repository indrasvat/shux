You are a terminal-emulator design council reviewing shux task 072:
origin mode and scroll-region semantics.

Context:
- shux-vt stores cursor rows as absolute visible-grid coordinates.
- Existing code tracks `TerminalModes::origin_mode`, reports DECRQM `?6`, saves
  origin mode in `Cursor::save`, and uses `ScrollRegion` for linefeed/RI/SU/SD.
- Current gaps: CUP/HVP/VPA ignore origin mode; DECSET/DECRST `?6` does not
  home the cursor; DECSTBM always homes to absolute row 0; scroll-region clamp
  semantics are under-tested.

Read these files in the repository before responding:
- Task: `docs/tasks/072-shux-vt-origin-mode-scroll-region.md`
- Design: `.shux/qa/072-shux-vt-origin-mode-scroll-region/DESIGN.md`
- Current parser: `crates/shux-vt/src/parser.rs`
- Current VT owner/resize logic: `crates/shux-vt/src/lib.rs`
- Current cursor save/restore model: `crates/shux-vt/src/cursor.rs`

Questions:
1. Are the target xterm/common-terminal semantics right for DECOM, CUP/HVP,
   VPA, DECSTBM, DSR, DECRQM, save/restore, 1049 alternate screen, and
   synchronized output?
2. What edge cases must be fixed before coding? Consider invalid scroll
   regions, 1-row regions, row params beyond the region, resize, cursor clamp,
   and saved cursor state when origin mode changes.
3. What tests/evidence are mandatory to avoid false confidence?

Return severity-labelled findings. End with VERDICT: APPROVE,
APPROVE-WITH-CONDITIONS, or REJECT, plus MUST-FIX items.
