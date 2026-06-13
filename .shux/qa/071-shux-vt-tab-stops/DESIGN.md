# Task 071 Design: Real Tab-Stop State

## Goal

Replace hardcoded 8-column tab movement with mutable xterm-style tab-stop
state for the common HT/HTS/TBC/CHT/CBT path, while preserving the current
default behavior when applications never customize stops.

## State Model

Add a `TabStops` value owned by `VirtualTerminal`, threaded into `VtHandler`
the same way charset state is threaded.

- Default state has stops at columns `8, 16, 24, ...` within the current width.
- Column `0` is not a default stop.
- The state stores stop columns as a flat bitmap (`Vec<bool>`) indexed by
  zero-based column. This is closer to xterm's tab bitmap than a tree/set and
  avoids a fragile default-to-explicit transition. The bitmap also tracks
  whether resize growth should extend 8-column defaults; `TBC 3` disables that
  extension.
- `HT`, `CHT`, and `CBT` consult the same state.
- `HTS` (`ESC H`) sets the current cursor column bit without altering other
  default stops.
- `TBC 0` (`CSI 0 g` or `CSI g`) clears the current cursor column bit without
  altering other default stops.
- `TBC 3` (`CSI 3 g`) clears all tab stops and keeps them cleared across resize.
- Unsupported `TBC` parameters are ignored.

Initialize/reset the bitmap with `col > 0 && col % 8 == 0`. Column `0` must not
be a default stop. The parser should clear the active grapheme anchor before
every tab movement or tab-state mutation, matching the existing cursor-motion
behavior.

## Resize Semantics

Tab stops are terminal state, not grid content.

- On resize narrower, truncate stops outside the new width.
- On resize wider, preserve existing bits and extend 8-column default stops
  into the newly visible columns while default extension is still enabled.
- `TBC 3` disables default extension, so clear-all plus resize does not recreate
  defaults.
- This preserves practical xterm parity for local HTS/TBC mutations without
  breaking the explicit clear-all contract.

## Movement Rules

Forward movement:

- `HT` is equivalent to one forward tab.
- `CHT Ps` moves to the Ps-th next tab stop, with Ps defaulting to 1.
- If there is no next stop, clamp to the last column.

Backward movement:

- `CBT Ps` moves to the Ps-th previous tab stop, with Ps defaulting to 1.
- If there is no previous stop, clamp to column 0.

All tab movement clears `auto_wrap_pending`.

## Integration Points

- `VirtualTerminal` owns `tab_stops`.
- `VirtualTerminal::resize()` calls `tab_stops.resize(cols)`.
- `VtHandler` borrows `tab_stops`.
- `execute(0x09)` uses `next_tab_col(1)` rather than hardcoded arithmetic.
- `csi_dispatch('I')` and `csi_dispatch('Z')` use the same helpers.
- `csi_dispatch('g')` handles TBC.
- `esc_dispatch(b'H')` handles HTS.
- RIS should restore default tab stops.
- DECSTR should not reset tab stops. The design council explicitly reviewed
  and rejected a Gemini suggestion to reset tabs on DECSTR because that would
  diverge from xterm/DEC behavior; only RIS performs the tab reset in this task.
- Alternate screen switches should not reset tab stops.

## Baseline Governance

The visual fixture intentionally includes both default and custom tab behavior:

- default columns at 8/16,
- custom stops set at non-8 columns,
- current-stop clear,
- clear-all behavior,
- resize preservation across 80x24, 120x40, and return-to-80x24.

Expected PNGs live in `.shux/goldens/071-tab-stops/`. Verification scripts may
only promote those baselines when `SHUX_TAB_STOPS_PROMOTE=1`, after this design
has been reviewed by DootSabha. Normal verification compares actual PNGs exactly
against committed goldens.

## Required Tests

- Unit: default HT remains compatible with existing 8-column behavior.
- Unit: HTS creates a custom stop and HT lands there.
- Unit: `TBC 0` clears the current stop.
- Unit: `TBC 3` clears all stops and leaves HT clamping to the last column.
- Unit: `CHT` and `CBT` honor custom stops and counts.
- Unit: resize truncates out-of-range stops on shrink, extends defaults on grow
  after local HTS/TBC mutations, and does not recreate defaults on grow after
  `TBC 3`.
- Unit: custom HTS/TBC mutations preserve other default stops.
- Unit: RIS restores default tab stops.
- Unit: DECSTR does not reset tab stops.
- Integration: real pane capture shows custom tab-aligned table columns.
- Shux automation: PNG and text captures for 80x24, 120x40, return-to-80x24,
  exact pixel comparison with zero tolerance.
