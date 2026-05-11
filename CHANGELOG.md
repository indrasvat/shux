# [0.6.0](https://github.com/indrasvat/shux/compare/v0.5.0...v0.6.0) (2026-05-11)


### Features

* **ui,rpc,cli:** pane titles — manual + OSC + auto-derived (task 027, PR 4) ([#12](https://github.com/indrasvat/shux/issues/12)) ([059b38a](https://github.com/indrasvat/shux/commit/059b38ab03d8edbaa3f96d74f362c677777c90a0)), closes [#7](https://github.com/indrasvat/shux/issues/7)

# [0.5.0](https://github.com/indrasvat/shux/compare/v0.4.0...v0.5.0) (2026-05-11)


### Features

* **rpc,cli:** optimistic concurrency surface (task 037, PR 3b) ([#11](https://github.com/indrasvat/shux/issues/11)) ([a4b40c5](https://github.com/indrasvat/shux/commit/a4b40c518e474b62b449c81cd81ac5bca9ff80d7))
* **ui:** bare Alt+h/j/k/l + Alt+n/p + Alt+1..9 Tier-1 bindings (task 018) ([#13](https://github.com/indrasvat/shux/issues/13)) ([4ac7b43](https://github.com/indrasvat/shux/commit/4ac7b433d3aca6443eb3c8f2b68bf01b7dc4c95f)), closes [#8](https://github.com/indrasvat/shux/issues/8)

# [0.4.0](https://github.com/indrasvat/shux/compare/v0.3.0...v0.4.0) (2026-05-10)


### Features

* **apply:** state.apply RPC + shux apply CLI (task 030, PR 3a) ([#10](https://github.com/indrasvat/shux/issues/10)) ([95d9417](https://github.com/indrasvat/shux/commit/95d941726766c73be91174cd5a0a5c1f3a39ec2c)), closes [#3](https://github.com/indrasvat/shux/issues/3) [#2](https://github.com/indrasvat/shux/issues/2) [#1](https://github.com/indrasvat/shux/issues/1) [#7](https://github.com/indrasvat/shux/issues/7)

# [0.3.0](https://github.com/indrasvat/shux/compare/v0.2.0...v0.3.0) (2026-05-10)


### Features

* **events:** events.watch + events.history RPC + CLI (task 036, PR 2a) ([#9](https://github.com/indrasvat/shux/issues/9)) ([6bc0e04](https://github.com/indrasvat/shux/commit/6bc0e046c61e937e304cfbf2d3406c550ec0b0d2)), closes [3-#7](https://github.com/3-/issues/7) [#7](https://github.com/indrasvat/shux/issues/7) [#4](https://github.com/indrasvat/shux/issues/4) [#6](https://github.com/indrasvat/shux/issues/6) [hi#volume](https://github.com/hi/issues/volume)

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
