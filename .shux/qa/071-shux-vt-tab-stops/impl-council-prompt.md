# Implementation-diff review — Task 071: shux-vt mutable tab stops

You are reviewing the IMPLEMENTATION of task 071 (mutable horizontal tab stops)
in the shux terminal multiplexer. Read the actual files in the repo; do not rely
only on the diff summary.

## Files to read
- Task spec: `docs/tasks/071-shux-vt-tab-stops.md`
- Design: `.shux/qa/071-shux-vt-tab-stops/DESIGN.md`
- Design council verdict (must-fixes): `.shux/qa/071-shux-vt-tab-stops/dootsabha-design.json`
- New state module: `crates/shux-vt/src/tabstops.rs`
- Parser integration: `crates/shux-vt/src/parser.rs` (HT at execute 0x09, CHT `'I'`,
  CBT `'Z'`, TBC `'g'`, HTS `esc_dispatch b'H'`, RIS, the `next_tab_col`/`prev_tab_col`
  helpers, and the test module)
- VT owner + resize: `crates/shux-vt/src/lib.rs` (`VirtualTerminal`, `resize`, RIS wiring)
- Integration test: `crates/shux/tests/pane_io_integration.rs`
  (`test_capture_honors_mutable_tab_stops`)

## Design-council must-fixes that the implementation MUST satisfy
1. State-seeding: setting/clearing one stop must NOT wipe the other 8-column
   defaults (HTS at col 12 -> stops at 8,12,16,24).
2. Resize-wider parity: after a local HTS/TBC mutation, growing width must keep
   8-column defaults in the newly revealed columns; but TBC 3 (clear-all) must
   NOT recreate defaults on later resize.
3. Reject DECSTR resetting tabs: DECSTR (`CSI ! p`) must NOT reset tab stops;
   only RIS (`ESC c`) restores defaults.
4. Column 0 is not a default stop.
5. Bitmap representation (not BTreeSet), preserve-bits-on-resize semantics.

## Questions to answer (concise, severity-labelled P1/P2/P3, actionable)
1. Correctness bugs in tab-stop state, parser integration, resize, reset, or
   capture/snapshot behavior? Consider: count semantics for CHT/CBT, clamping to
   width-1 / column 0, behavior when cursor.col >= bitmap length, auto_wrap_pending
   clearing on all tab movement, grapheme-anchor clearing on tab-state mutations,
   the permanent `extend_defaults=false` after clear_all, narrow-then-widen
   resurrection of cleared stops.
2. Are the 5 design-council must-fixes actually addressed by the code?
3. Missing tests or evidence gaps before PR (alternate-screen preservation,
   width-boundary HT clamp, etc.).

Keep it tight. No restating the diff. Flag only real issues with file:line where
possible.
