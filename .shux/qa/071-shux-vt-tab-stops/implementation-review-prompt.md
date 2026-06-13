# DootSabha Implementation Review: Task 071

Review the implementation diff for task 071, mutable tab stops.

Inputs:

- Task: `docs/tasks/071-shux-vt-tab-stops.md`
- Design: `.shux/qa/071-shux-vt-tab-stops/DESIGN.md`
- Design council: `.shux/qa/071-shux-vt-tab-stops/dootsabha-design.json`
- Diff: `.shux/qa/071-shux-vt-tab-stops/implementation-diff.txt`

Implementation summary:

- Adds `TabStops` bitmap state owned by `VirtualTerminal`.
- Wires tab state into `VtHandler`, `HT`, `HTS`, `TBC`, `CHT`, `CBT`, resize,
  and RIS.
- Keeps DECSTR from resetting tabs.
- Adds unit tests for default stops, HTS preserving defaults, TBC current/all,
  CHT/CBT counts, resize growth/clear-all, and RIS/DECSTR.
- Adds pane.capture integration coverage.
- Adds shux automation `make test-vt-tab-stops` with 80x24, 120x40, and
  return-to-80x24 text/PNG/pixel evidence.
- Adds a synthetic corpus fixture for mutable tab stops.

Please answer:

1. Are there P1/P2 correctness bugs in tab-stop state, parser integration,
   resize, reset, or capture/snapshot behavior?
2. Are the design-council must-fixes actually addressed?
3. Are there missing tests or DoD evidence gaps before PR?

Keep findings concise, severity-labeled, and actionable.
