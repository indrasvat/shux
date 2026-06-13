Review task 070 for shux: implement common VT100 DEC Special Graphics charset
support in shux-vt.

Repository context:
- shux is a Rust terminal multiplexer and pixel snapshotter.
- Current parser ignores SO/SI and `ESC ( 0` / `ESC ) 0` charset designation.
- Existing VT quality tasks require unit, integration, shux automation,
  full-resolution screenshots, exact pixel comparisons, DootSabha reviews, and
  SOLID QA.
- Task file: `docs/tasks/070-shux-vt-dec-special-graphics.md`.
- Proposed design is in `.shux/qa/070-dec-special-graphics/DESIGN.md`.

Please critique the design specifically for:
1. Correctness of the minimal charset state model (`G0`, `G1`, active G0/G1).
2. Correct DEC Special Graphics mapping and any missing common glyphs.
3. Parser integration risks with Unicode/grapheme handling, wide-cell
   invariants, RIS/reset, and `vte::Perform`.
4. Test gaps in unit, corpus, pane.capture, shux automation, visual, and pixel
   evidence.
5. Whether any scope should be explicitly rejected to avoid a partial
   ISO-2022 implementation.

Return concrete findings with severity and actionable fixes. If the plan is
sound, say so explicitly and list the key guardrails that must remain.
