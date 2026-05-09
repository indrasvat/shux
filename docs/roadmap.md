# Roadmap

Live status: see [`PROGRESS.md`](PROGRESS.md) for the per-task table and
session log.

## Milestones

| Milestone | Focus | State |
|---|---|---|
| **M0** Architecture spike | Daemon, model, layout, PTY, VT, RPC, compositor, CLI foundations | ✅ Done |
| **M1** Daily-driver core | Multi-pane TUI, attach client, mouse, TOML config + hot reload, status bar, CLI passthrough | 🟡 Mostly done — copy mode, help overlay, full theme engine pending |
| **M2** API + plugin system | Wasm + process plugins, complete RPC surface, event streaming, MCP | ⏸️ Pending |
| **M3** Polish, performance, 1.0 | P0 perf budgets, fuzzing, image passthrough, shell completions, binary releases | ⏸️ Pending |

## What works today (post M0 + most of M1)

- Sessions, windows, panes — full CRUD via CLI and JSON-RPC
- Real PTYs with proper shell loading + dotfile integration (starship,
  atuin, ble.sh)
- Multi-pane TUI rendering with rounded borders, focused-pane highlight,
  Catppuccin-themed status bar
- Five border styles, switchable live via TOML hot reload
- Tier-1 keybindings via `Ctrl+Space` prefix
- Mouse: click-to-focus, drag-to-resize
- CLI passthrough — `shux new -s vim -- vim foo.rs`
- TOML config + hot reload, mirrors `tmux source-file`
- Status-bar segments — drop-in starship, shell scripts, anything that
  prints

## M1 follow-ups (next branches)

| Task | Description |
|---|---|
| 018 | Tier 1 keybindings (formal binding manager) |
| 019 | Prefix key system (Tier 2 chord support) |
| 021 | Copy mode — vim-style scrollback nav + OSC 52 clipboard |
| 022 | TOML config (additional polish — already mostly done) |
| 024 | Theme engine + token system |
| 025 | Per-pane theming |
| 027 | Pane titles (manual + auto from window title escapes) |
| 028 | Capability negotiation (ClientCaps) |
| 029 | Synchronized output (Mode 2026) |
| 030 | Session templates |
| 031 | Keybinding configuration + conflict detection |
| 032 | Command palette |
| 033 | Help overlay (cheat sheet) |
| 034 | M1 integration + quality gate |

## M2 (plugin system)

| Task | Description |
|---|---|
| 035 | Complete JSON-RPC API surface |
| 036 | Event stream (`events.watch`) |
| 037 | Optimistic concurrency + ensure operations |
| 038 | Plugin host: wasmtime integration |
| 039 | Plugin permissions + sandbox |
| 040 | Plugin WIT host functions |
| 041 | Plugin lifecycle + hot reload |
| 042 | Event interception chain |
| 043 | Command override system |
| 044 | Process plugin protocol |
| 045 | Plugin API extensions |
| 046 | Overlay system (z-ordered stack) |
| 047 | Inter-plugin event bus |
| 048 | Bundled plugin: `shux-status-bar` |
| 049 | Bundled plugin: `shux-theme-pack` |
| 050 | Bundled plugin: `shux-diagnostics` |
| 051 | gRPC API (optional transport) |
| 052 | M2 integration + quality gate |

## M3 (polish + 1.0)

| Task | Description |
|---|---|
| 053 | Performance optimization campaign |
| 054 | Shell completions (bash, zsh, fish) |
| 055 | Image passthrough (DCS, Kitty, Sixel, iTerm2) |
| 056 | Fuzzing campaign (ANSI, JSON-RPC, config, layout) |
| 057 | Documentation (README, guides, API reference) |
| 058 | Binary releases + distribution |
| 059 | M3 final quality gate + v1.0 release |

## Stretch goals (post 1.0)

- Session persistence — save/restore layouts and running commands across restarts
- Floating panes — overlay panes for transient tasks
- Session replay — time-travel through terminal history
- MCP integration — expose shux operations as MCP tools (likely a bundled M2 plugin)
- Web client — attach to sessions from a browser
- Plugin marketplace — discover and install community plugins
