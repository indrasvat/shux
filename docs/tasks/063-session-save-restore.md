# 063 - Session Save and Restore

**Status:** Done
**Priority:** High (daily-driver workspace UX)
**Milestone:** M1/M3 bridge
**Depends On:** 013, 014, 015, 030
**Touches:** `crates/shux/src/session_persist.rs`, `crates/shux/src/cli.rs`, `crates/shux/src/template.rs`, `.shux/scripts/`

---

## Problem

SHUX can create workspaces from templates, but it cannot save a current
workspace back into a reusable layout file. That keeps humans from
treating SHUX sessions as persistent workspaces and makes it harder for
agents to checkpoint a terminal layout before risky work.

## Design

Implement a conservative save/restore layer on top of the existing
template and `state.apply` machinery:

- `shux session save -s NAME -o FILE`
  serializes the current session into the existing template TOML shape.
- `shux session restore FILE`
  validates the file and applies it through `state.apply`.
- Saved templates include session name, window titles, pane cwd, command,
  split direction, and ratio where reconstructable.
- Running processes are not silently resurrected with hidden side
  effects. Restored panes run the saved command if one is known, else the
  default shell.

This is intentionally explicit. Automatic daemon restart recovery can be
a later first-party plugin once the manual format and safety semantics
are stable.

## Acceptance Criteria

- [x] Save a multi-window, multi-pane session to TOML.
- [x] Restore that TOML into an equivalent session through `state.apply`.
- [x] Output is stable enough for review and commit.
- [x] Unknown or missing pane commands restore as default shell.
- [x] Invalid restore files fail with the existing template diagnostics.
- [x] Integration tests cover save, restore, dry-run compatibility, and
      round-trip shape.
- [x] Dogfood automation creates a session, saves it, restores it, and
      compares session/window/pane summaries.

## Completion Notes

Completed on 2026-05-18 as part of `feat/human-interactive-core`.
`shux session save -s NAME -o FILE` exports the current graph state into
the existing template TOML shape, and `shux session restore FILE` lowers
that template through the same `state.apply` path as normal workspace
templates. `--dry-run` prints the lowered ops for review. The first
implementation preserves pane commands, cwd, window titles, split
direction, and ratio for reconstructable layouts.

## Verification Matrix

- live attach render path: restored sessions attach normally
- `window.snapshot` / `session.snapshot`: restored layout renders
- default config: save/restore works
- `shux config init` state: no drift
- malformed config: no behavior change
- hot reload: no behavior change
- cross-path consistency: saved template lowered to `state.apply` ops
  matches restore behavior
