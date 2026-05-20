# 064 — Robust xterm-256color Compatibility

**Status:** Done

## Goal

Make `shux` a truthful, low-latency `TERM=xterm-256color` host for modern rich
TUIs without regressing pixel-perfect rendering, startup latency, or multiplexer
behavior.

## Source-Backed Contract

Research snapshot: 2026-05-20.

- XTerm control sequences are current at patch #410, released 2026-05-01.
  `ctlseqs` documents 7-bit response defaults, DA/DA2, DSR, DECRQM, DECRQSS,
  XTVERSION, OSC dynamic colors, and DCS terminfo queries.
- XTerm terminfo content is current at `$XTermId: terminfo,v 1.216
  2026/02/12 22:20:25 tom Exp $`. `xterm-256color` advertises 256 colors,
  BCE, alternate screen, application cursor/keypad modes, cursor visibility,
  cursor style, color reset, scrolling regions, and xterm-style modified keys.
- tmux’s own guidance still recommends `tmux-256color` or `screen-256color`
  inside a multiplexer, with explicit RGB/Tc feature advertisement. Shux can
  choose a multiplexer identity by default, but explicit `TERM=xterm-256color`
  must not hang or degrade TUIs.
- Bubble Tea v2.0 made synchronized output mode 2026 and terminal mode reports
  part of the modern TUI surface. A multiplexer that ignores `CSI ? 2026 $ p`
  can reintroduce startup/probe waits.
- Neovim treats `xterm`-like terminals specially for 256-color and truecolor
  heuristics, and constructs missing RGB/cursor capabilities from terminfo,
  `$TERM`, `$COLORTERM`, and terminal identity.

Sources:

- https://invisible-island.net/xterm/ctlseqs/ctlseqs.pdf
- https://invisible-island.net/xterm/xterm.log.html
- https://invisible-island.net/xterm/terminfo-contents.html
- https://github.com/tmux/tmux/wiki/FAQ
- https://www.mankier.com/1/tmux
- https://github.com/charmbracelet/bubbletea/releases
- https://github.com/charmbracelet/bubbletea
- https://neovim.io/doc/user/tui.html

## Required Behavior

1. Application probes must receive prompt, truthful replies:
   - DA / DA2 / DSR / CPR.
   - DECRQM for standard and DEC private modes, including `?2026`.
   - DECRQSS for SGR, scroll region, cursor style, DECSCA, DECSCL.
   - OSC 4/10/11 color queries with matching BEL/ST terminator style.
   - XTGETTCAP for common terminfo names used by modern TUIs.
   - XTVERSION.
2. If synchronized output mode 2026 is reported as supported, rendering must
   preserve the last committed frame while the app is inside a synchronized
   update block, then atomically expose the finished frame on mode reset.
3. Explicit `TERM=xterm-256color` launches inside shux must remain fast and
   visually correct for rich TUIs.
4. The default pane `TERM` policy must remain conservative unless tests prove
   xterm compatibility is good enough to promote.

## Verification

- Unit tests for parser responses and synchronized output presentation.
- PTY integration tests proving DECRQM, XTVERSION, and sync-output probes
  receive replies through a live shux session.
- Startup timing comparison for direct launch vs shux launch.
- Rich TUI visual pass through shux snapshots:
  `lazygit`, `btop`/`htop`, `nvim`/`vim`, `vivecaka --repo=indrasvat/shux`,
  `vicaya` when installed, and at least one Bubble Tea v2-style sync probe.
- Codecov must report all modified coverable lines covered.
- Codex review must be clean before merge.

## Current Branch Results

- Added DECRQM replies for tracked ANSI/DEC private modes, including `?2026`.
- Added XTVERSION replies.
- Added tracked application keypad, focus-event, SGR mouse, and synchronized
  output mode state.
- Implemented synchronized-output presentation freezing: `grid()` / `cursor()`
  expose the last committed frame while mode 2026 is active, then expose the
  working frame after reset.
- Expanded XTGETTCAP coverage for common color, cursor-shape, alternate-screen,
  keypad, and clipboard capabilities.
- Added unit coverage for mode reports, XTVERSION, extended XTGETTCAP, and
  synchronized-output presentation behavior.
- Added live PTY integration coverage for DECRQM/XTVERSION replies and active
  synchronized-output capture freezing.
- Rich TUI proof:
  `.shux/out/xterm256-rich-tui-20260520-105116/`.
- Committed proof target:
  `.shux/goldens/xterm256-full-support/`.
