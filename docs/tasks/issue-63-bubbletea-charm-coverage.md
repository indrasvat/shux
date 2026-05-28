# Issue 63 — Bubble Tea and Charm Terminal Coverage

**Status:** Done.

Issue #63 is an evaluation and hardening pass after the Bubble Tea redraw fixes.
The scope here is rendering-adjacent behavior that affects attach rendering,
pane/window PNG snapshots, and the VT grid seen by capture APIs.

## Official Sources Reviewed

- `github.com/charmbracelet/bubbletea` at `c60f0c53042238305ec13b486326588f12aea0ec`
- `github.com/charmbracelet/x` at `009e6338d40ddfbc65bcd4a2d5b822015302aa5a`

Relevant Bubble Tea renderer behavior:

- Alternate-screen toggles (`1047`, `1048`, `1049`).
- Synchronized output mode (`2026`).
- Cursor visibility, shape, and color.
- Dynamic terminal colors (`OSC 10`, `OSC 11`, `OSC 12`).
- Window title (`OSC 0`, `OSC 2`).
- Bracketed paste, focus reports, mouse tracking.

Relevant Charm renderer/VT behavior:

- Optimized cursor movement and erase primitives: `CHA`, `HPA`, `VPA`, `CHT`,
  `CBT`, `REP`, `ECH`, `ICH`, `SU`, `SD`.
- OSC 8 hyperlinks.
- Advanced underline style and underline color.
- OSC color query/set/reset for foreground, background, and cursor.

## Coverage Matrix

| Area | Status |
| --- | --- |
| Alternate screen `1047/1048/1049` | Covered by VT regressions and render matrix screenshots. |
| Synchronized output `2026` | Covered by VT regressions and `issue_63_render_matrix.sh`; held-frame capture rejects partial-frame leakage. |
| Cursor visibility | Covered; hidden cursors are omitted from pane/window snapshots. |
| Cursor shape | Fixed here: live attach emits `SetCursorStyle`; pane/window snapshots render block, underline, and bar cursors. |
| Cursor color `OSC 12/112` | Fixed here: VT state/query/reset, live attach OSC 12/112 emission, and PNG cursor color rendering. |
| Dynamic fg/bg `OSC 10/11/110/111` | Already covered; matrix includes pane/window default color proof. |
| Window title `OSC 0/2` | Already covered; matrix includes composed-window border proof. |
| Truecolor and SGR attrs | Covered by existing VT/UI tests and matrix color probe. |
| Advanced underline style/color | Fixed here for PNG snapshots; live attach already emitted crossterm underline style/color. |
| OSC 8 hyperlinks | Covered for VT state and live attach emission; PNG remains visual-only because hyperlink metadata is not visible in a bitmap. |
| Charm renderer primitives | Covered by existing VT regressions plus stale-cell matrix probe. |

## Fixes Landed

- Added `TerminalDefaultColors.cursor` and OSC 12/112 parser/query/reset support.
- Propagated focused-pane cursor shape/color through `RenderCompositor`.
- Added attach cleanup for cursor shape and OSC 12 cursor color so state does
  not leak after detach.
- Added PNG raster support for cursor shape/color.
- Added PNG raster support for advanced underline style/color.
- Added `.shux/scripts/issue_63_render_matrix.sh` as repeatable evidence for
  stale-cell, color metadata, cursor, title, synchronized-output, and rich TUI
  smoke paths.

## Verification Artifacts

Generated under `.shux/out/issue-63/`:

- `primitives-window.png`
- `color-meta-window.png`
- `cursor-window.png`
- `title-window.png`
- `sync-held-pane.png`
- `sync-released-window.png`
- `vim-window.png`

The matrix script validates PNG existence/dimensions and asserts the held
synchronized-output frame does not expose the unreleased `partial-hidden` text.

## Deferred

- Hyperlink metadata in PNG output. A bitmap cannot expose an interactive link
  target without a sidecar metadata format.
- Graphics protocols such as sixel/Kitty/iTerm2 images. Those are rendering
  features, but they are larger protocol work and remain in the image
  passthrough track.
- Grapheme-cluster storage and shaped ligatures. Raster snapshots still operate
  on the VT cell model.
