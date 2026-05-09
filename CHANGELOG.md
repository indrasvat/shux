# Changelog

All notable changes to shux are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Subsequent versions are appended automatically by `semantic-release` based
on Conventional Commit messages on `main`.

## [0.1.0] — 2026-05-09

Initial public release. shux is a typed-API terminal multiplexer for
humans and AI agents, with a tiny core and first-class support for
plugins, scripting, and structured introspection.

### Added

- Multi-pane TUI client (`shux attach`) with rounded borders, focused-pane
  highlight, and Catppuccin-themed status bar
- Five border styles (thin / thick / double / rounded / ascii / none),
  switchable via TOML hot reload
- Tier-1 keybindings via `Ctrl+Space` prefix (split, focus, zoom, kill,
  resize, window navigation)
- Mouse support: click-to-focus, drag-to-resize on borders
- Real PTYs with proper shell loading + dotfile integration
  (starship, atuin, ble.sh)
- TOML-driven configuration with hot reload (mirrors `tmux source-file`)
- Status-bar runner — drop-in starship, shell scripts, anything that prints
- Full session / window / pane CRUD via CLI and JSON-RPC
- CLI passthrough: `shux new -s vim -- vim foo.rs`
- Rich CLI output with box-drawing tables and short IDs

### Architecture

- Cargo workspace with seven crates: `shux` (CLI/daemon), `shux-core`,
  `shux-pty`, `shux-vt`, `shux-rpc`, `shux-plugin`, `shux-ui`
- Single binary, daemon auto-starts on first use
- Single-writer mutation channel (mpsc) with `ArcSwap` snapshots for
  lock-free reads
- JSON-RPC over UDS + TCP for both human CLI and programmatic clients
