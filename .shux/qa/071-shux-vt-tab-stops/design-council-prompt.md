# DootSabha Design Review Prompt: Task 071

Review the design for shux task 071, real tab-stop state.

Inputs:

- Task: `docs/tasks/071-shux-vt-tab-stops.md`
- Design: `.shux/qa/071-shux-vt-tab-stops/DESIGN.md`
- Current implementation references:
  - `crates/shux-vt/src/lib.rs`
  - `crates/shux-vt/src/parser.rs`
  - `.shux/fixtures/vt-corpus/synthetic/manifest.json`

Please evaluate:

1. Is the proposed `Default` vs `Explicit` tab-stop state model faithful enough
   for xterm-compatible HT/HTS/TBC/CHT/CBT behavior?
2. Are the resize semantics correct, especially clear-all and custom-stop
   preservation?
3. Are the parser integration points complete, including RIS and alternate
   screen behavior?
4. What unit/integration/visual/pixel tests are missing?
5. What scope boundaries should be explicitly rejected for this task?

Return findings with severity and concrete fixes. Approval can be conditional,
but call out any must-fix design issues before implementation.
