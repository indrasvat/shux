# 065 — Attach Pane Color Preservation Under NO_COLOR

**Status:** Done

## Goal

Fix issue #69: `shux session attach` must preserve pane-content colors even
when the daemon inherited `NO_COLOR=1`, without changing normal CLI `NO_COLOR`
behavior.

## Diagnosis

Release-artifact checks showed the failure was already present in `v0.24.3`
and did not start at `v0.25.0`.

- `v0.24.3`, clean daemon, `vivecaka --repo indrasvat/gh-hound`:
  attach emitted color SGR (`truecolor_sgr=54`, `indexed_sgr=2`).
- `v0.24.3`, daemon launched with `NO_COLOR=1`: attach emitted zero color
  SGR and empty `ESC[m` sequences.
- `v0.25.0`, clean daemon: attach emitted color SGR
  (`truecolor_sgr=54`, `indexed_sgr=2`).
- `v0.25.0`, daemon launched with `NO_COLOR=1`: attach emitted zero color
  SGR and empty `ESC[m` sequences.

Root cause: attach pane rendering used crossterm color commands, which obey
crossterm's process-global `NO_COLOR` state. Pane PTYs and snapshot
rasterization can still be colorful, so human attach and agent snapshots can
diverge.

## Implementation

- Replaced crossterm's gated color commands in `shux-ui::RenderBackend` with
  renderer-local ANSI color commands for foreground, background, and underline
  color.
- Kept the fix scoped to terminal-emulation bytes; no crossterm global color
  state is mutated.
- Added unit coverage proving pane colors serialize while crossterm's global
  color-disabled flag remains set.
- Added multi-pane attach-compositor coverage for truecolor, 256-color, and
  basic indexed VT colors under `NO_COLOR`.
- Added `make test-ui` for focused `shux-ui` testing.
- Added `make test-attach-color`, backed by
  `.shux/scripts/issue_69_attach_color_check.sh`, as a release-binary guard.

## Verification

- `make test-ui`
- `make test-attach-color`
- `make ci`
- Patched `target/release/shux` with daemon `NO_COLOR=1` and real
  `vivecaka --repo indrasvat/gh-hound`: attach emitted
  `truecolor_sgr=182`, `indexed_sgr=2`, `empty_sgr=0`.
- Visual proof:
  `.shux/out/issue-69/patched-no-color-vivecaka-pane.png`
