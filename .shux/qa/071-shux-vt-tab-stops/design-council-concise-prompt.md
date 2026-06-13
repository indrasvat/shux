# DootSabha Concise Design Review: Task 071

Task: implement real mutable tab-stop state in `shux-vt`.

Current behavior:

- `HT` uses hardcoded next 8-column boundary.
- `CHT`/`CBT` use hardcoded 8-column helpers.
- `TBC` and `HTS` are ignored.

Proposed design:

- Add `TabStops` owned by `VirtualTerminal` and borrowed by `VtHandler`.
- State has two modes:
  - `Default`: implicit stops at 8, 16, 24, ... for current width.
  - `Explicit(BTreeSet<usize>)`: after HTS/TBC, use exactly stored stops.
- `HTS` (`ESC H`) inserts current column and transitions to `Explicit`.
- `TBC 0` / `CSI g` removes current column and transitions to `Explicit`.
- `TBC 3` clears all stops and transitions to `Explicit(empty)`.
- `HT`, `CHT`, `CBT` all use `TabStops`; no next stop clamps to last column,
  no previous stop clamps to column 0.
- Resize removes out-of-range explicit stops; default mode recomputes implicit
  stops for new width; explicit mode never recreates defaults.
- RIS resets to default tab stops; alternate screen does not reset tab stops.

Please answer:

1. Approve/reject this design for common xterm-compatible HT/HTS/TBC/CHT/CBT?
2. List any P1/P2 must-fix design issues.
3. List mandatory tests.

Keep the response concise and actionable.
