# 062 - Scrollback-Backed Copy Mode

**Status:** Done
**Priority:** High (daily-driver human UX)
**Milestone:** M1 polish
**Depends On:** 005, 021, 061
**Touches:** `crates/shux-ui/src/copy_mode.rs`, `crates/shux-vt/src/grid.rs`, `crates/shux/src/attach.rs`, `.shux/scripts/`

---

## Problem

SHUX copy mode can select visible text, including mouse-drag selection
from task 061, but it cannot browse pane history. This is the biggest
remaining gap versus the expected `tmux` copy-mode workflow.

Users need to enter copy mode, scroll back through previous pane output,
search, select text that is no longer visible, yank it to the clipboard,
and return to normal input mode without the child PTY seeing those keys
or mouse events.

## Design

Use the existing `shux-vt::Grid` scrollback as the source of truth. The
copy-mode state gets:

- `scroll_offset`: number of rows above the live viewport bottom.
- cursor and anchor coordinates in the currently displayed copy view.
- search query/direction and last match state.

When `scroll_offset == 0`, rendering remains the existing live viewport
path. When it is nonzero, the focused pane is drawn from a logical view
over `scrollback + visible rows`, while the rest of the SHUX chrome is
unchanged.

Navigation:

- `PageUp` / `PageDown`
- `Ctrl-b` / `Ctrl-f`
- `Ctrl-u` / `Ctrl-d`
- `g` / `G`
- mouse wheel while copy mode is active
- `/`, `?`, `n`, `N` search

Selection/yank must work across both historical and visible rows.

## Acceptance Criteria

- [x] A command that prints 500 lines can be searched and yanked after
      the target line has scrolled off screen.
- [x] Copy mode visually replaces the focused pane with the selected
      scrollback view while preserving SHUX borders/status bar.
- [x] Mouse wheel scrolls history only while copy mode is active.
- [x] Selection and yank work across historical and live-visible rows.
- [x] Existing visible-only keyboard and mouse copy still work.
- [x] Focused unit tests cover view-window math, search, selection, and
      scroll behavior.
- [x] Dogfood automation saves visual evidence under
      `.shux/out/` via `.shux/scripts/human_copy_mode_check.sh`.

## Completion Notes

Completed on 2026-05-18 as part of `feat/human-interactive-core`.
Copy mode now supports scrollback viewport rendering, PageUp/PageDown,
Ctrl-b/f, Ctrl-u/d, gg/G, `/` / `?` search with `n` / `N`, mouse-wheel
history scrolling while copy mode is active, and selection/yank from the
logical scrollback view. Snapshot paths remain intentionally unchanged;
the live attach copy viewport is an interactive overlay.

## Verification Matrix

- live attach render path: copy viewport, overlay, wheel, search
- snapshot paths: unchanged by design, explicitly documented
- default config: enabled
- `shux config init` state: no drift
- malformed config: no behavior change
- hot reload: no behavior change for phase 1
- cross-path consistency: live copy-view text equals the same logical
  rows extracted from `shux-vt::Grid`
