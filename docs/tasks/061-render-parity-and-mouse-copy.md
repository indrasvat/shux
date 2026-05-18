# Task 061: Render Parity and Mouse Copy

**Status:** Done
**Priority:** High (dogfood UX)
**Milestone:** M1/M3 bridge
**Depends on:** 017 (multi-pane render), 021 (copy mode), 024 (theme), 028 (cap negotiation)
**Touches:** `crates/shux-ui/src/terminal.rs`, `crates/shux/src/attach.rs`, `crates/shux-ui/src/copy_mode.rs`, `crates/shux-raster/src/lib.rs`, docs

---

## Problem

Running a TUI directly in a terminal and running it inside `shux attach`
do not mean the same visual contract:

1. Direct terminal rendering is owned by the host emulator. The host
   controls font fallback, shaping, ligatures, emoji, palette, cursor
   style, native selection, and scrollback UI.
2. SHUX live attach renders a composed multiplexer frame back into the
   host terminal. It adds pane borders, pane titles, status bar rows, and
   resizes the child PTY to the pane viewport rather than the full host
   terminal.
3. SHUX PNG snapshots are fully headless. `shux-vt` stores terminal cell
   state and `shux-raster` paints pixels with bundled fonts. The host
   terminal is not in the loop.

The immediate dogfood symptom is that mouse drag could not copy text in
an attached SHUX session. Root cause: `TerminalGuard::enter()` used
crossterm's broad `EnableMouseCapture`, which enables any-motion mouse
tracking (`?1003h`) even though SHUX only needs press/release and
button-held drags for focus and border resize. That makes host-terminal
modifier selection less reliable. SHUX also has only keyboard-driven copy
mode today (`Prefix + [` → `v`/move/`y`), so SHUX-owned mouse selection is
not yet implemented.

---

## Current Raster/VT Gaps

These are real capability gaps, not just styling differences:

- `shux-vt::Cell` stores one `char`, not a grapheme cluster. ZWJ emoji,
  variation selectors, skin tones, flag pairs, and combining sequences
  are split before the rasterizer sees them.
- `shux-raster` uses `fontdue`, which does grayscale glyph rasterization
  without shaping. There are no ligatures, bidi/RTL shaping, color emoji,
  or host font fallback.
- Snapshot rendering has a configurable primary font, but it does not
  import the user's terminal profile: font size, palette, line-height,
  cursor shape, and fallback stack can differ.
- Cursor shape is stored by `shux-vt`, but PNG rendering currently draws
  only an inverse block cursor.
- Italic is tracked in cell flags, but no real italic face is loaded.
- Parser support is partial: common CSI/SGR/alt-screen works; OSC 0/2
  title works; OSC 8 hyperlinks, DEC special graphics, and DCS/Sixel
  payloads are not implemented.
- Live attach and snapshots already share composition for panes,
  borders, titles, and status bars, but every new render feature must be
  explicitly verified across live attach, `pane.snapshot`,
  `window.snapshot`, and `session.snapshot`.

---

## Design

### Phase 1: Reduce Mouse Over-Capture

Replace crossterm's broad mouse capture with a SHUX-specific capture
profile:

- Enable `?1000h` normal tracking for press/release.
- Enable `?1002h` button-event tracking for drags while a button is held.
- Enable `?1006h` SGR coordinates for large terminals.
- Do not enable `?1003h` any-motion tracking.

This preserves SHUX click-to-focus and border-resize while giving host
terminals the best chance to keep their native modifier-drag selection
escape hatch.

### Phase 2: SHUX-Owned Mouse Selection

Add mouse drag support to copy mode:

- In copy mode, left down inside the focused pane sets anchor and cursor.
- Left drag updates cursor, clamped to the focused pane rect.
- Left up yanks only if the cursor moved, then exits copy mode.
- Existing keyboard copy (`v`, movement, `y`) remains the same state
  machine, not a parallel implementation.
- Mouse outside the focused pane while copy mode is active is swallowed.

Later, an optional direct drag-to-copy mode can promote a pane-content
drag into copy mode automatically, but it needs config because it competes
with terminal-native selection and future PTY mouse forwarding.

### Phase 3: Borderless / Fill Mode

Add an explicit way to run or snapshot a single-pane session without SHUX
chrome:

- no outer border,
- no pane title row,
- optional no status bar,
- child PTY sized to the full host viewport.

This is the right mode for demos where users compare a TUI directly
against the same TUI inside SHUX.

### Phase 4: Rasterizer Roadmap

Treat deeper visual parity as a separate emulator/raster project:

- grapheme-cluster cell storage,
- shaping engine for ligatures and complex text,
- color emoji rendering,
- OSC 8 hyperlink attributes,
- DEC special graphics,
- cursor shape rendering,
- terminal profile import for palette/font metrics.

---

## Acceptance Criteria

- [x] SHUX attach no longer emits `?1003h` any-motion mouse tracking.
- [x] Existing click-to-focus and border-drag resize still work.
- [x] Copy-mode mouse drag selects visible text in the focused pane.
- [x] Click-only mouse down/up does not yank a single accidental cell.
- [x] Zoomed focused panes map mouse coordinates to the visible full-pane rect.
- [x] Render differences are documented as expected chrome/layout vs real
      emulator/raster gaps.
- [x] Focused tests cover mouse capture ANSI and copy-mode coordinate mapping.
- [x] `make fmt-check` and focused tests pass.

---

## Verification Matrix

- live attach render path: mouse capture, focus, resize, copy-mode overlay
- `pane.snapshot`: unchanged by phase 1, later compare copy-mode-independent pane pixels
- `window.snapshot` / `session.snapshot`: unchanged by phase 1, later compare borderless/fill mode
- default config: mouse capture profile active
- `shux config init` state: no drift
- malformed config: no behavior change
- hot reload: no behavior change in phase 1
- cross-path consistency: required before borderless/fill mode lands
