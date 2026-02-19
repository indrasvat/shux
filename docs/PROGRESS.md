# shux — Implementation Progress

> **STRICT RULE:** This file MUST be updated at the end of every coding session.

## Current Phase

**M0: Architecture Spike** — In progress (bootstrap complete)

## Status

### Milestone Targets

- [ ] **M0: Architecture Spike** (tasks 001–012)
  - [ ] Daemon skeleton with fork-before-tokio daemonization
  - [ ] PTY manager with async I/O
  - [ ] Virtual terminal grid (vte + VecDeque)
  - [ ] Minimal TUI client (single pane)
  - [ ] JSON-RPC server on UDS (system.version, system.health, session.list)
  - [ ] Basic input decoder (legacy + Kitty)
  - [ ] `shux` binary with `new`, `attach`, `ls`
  - [ ] L1 + L2 tests passing

- [ ] **M1: Daily-Driver Core** (tasks 013–034)
  - [ ] Full session/window/pane CRUD (API + CLI)
  - [ ] Splits, directional focus, resize, zoom, swap
  - [ ] Copy mode with clipboard
  - [ ] Graded keybindings (Tier 1 + 2), command palette, help overlay
  - [ ] TOML config with live reload
  - [ ] Theme engine with per-pane theming
  - [ ] Mouse support, pane titles, status bar
  - [ ] Session templates
  - [ ] L1–L4 tests passing
  - [ ] Dogfooding begins

- [ ] **M2: API + Plugin System** (tasks 035–052)
  - [ ] Complete JSON-RPC API surface
  - [ ] Event stream with filters and sequence numbers
  - [ ] Plugin host (Wasm + process plugins)
  - [ ] Event interception, command override, overlays
  - [ ] Bundled plugins (status-bar, theme-pack, diagnostics)
  - [ ] gRPC API (optional)
  - [ ] L1–L6 tests passing

- [ ] **M3: Polish, Performance, Docs** (tasks 053–059)
  - [ ] All P0 performance budgets met
  - [ ] Shell completions, image passthrough
  - [ ] Fuzzing campaigns (ANSI, JSON-RPC, config, layout)
  - [ ] Documentation (README, guides, API reference)
  - [ ] Binary releases (macOS + Linux)
  - [ ] v1.0 release

---

## Session Log

**2026-02-18 — Task 001: Daemon Skeleton and Process Lifecycle**
- Created `crates/shux-core/src/daemon.rs`: DaemonState, DaemonCommand, ShutdownTokens, run_daemon_state_loop with auto-exit grace timer
- Created `crates/shux/src/daemon.rs`: runtime_dir, PID file, socket path, double-fork daemonize(), signal handler (SIGTERM/SIGINT/SIGHUP)
- Created `crates/shux/src/client.rs`: ensure_daemon_running() with UDS probe + exponential backoff + re-exec auto-start
- Wired up main.rs with __daemon internal subcommand (fork-before-tokio) and client entrypoint
- 20 tests passing: DaemonState lifecycle, grace timer with tokio::time::pause(), shutdown tokens, PID file round-trip, runtime dir
- Added nix (user feature), tokio-util, thiserror dependencies
- Learning: Rust edition 2024 makes `std::env::set_var`/`remove_var` unsafe (process-global mutable state)
- Learning: nix 0.29 requires explicit `user` feature flag for `getuid()`
- Learning: Use `tokio::time::pause()` + `advance()` for deterministic timer tests instead of real 5+ second sleeps

**2026-02-18 — Task 000: Repository Scaffold and Tooling**
- Created Cargo workspace with 7 crates (shux binary + 6 library crates)
- All crates compile, clippy passes, rustfmt passes, nextest runs (0 tests)
- Created Makefile with self-documenting help, colored output, all required targets
- Created lefthook.yml with pre-commit (fmt+clippy) and pre-push (progress-check+test+deny)
- Created CLAUDE.md agent instructions and AGENTS.md redirect
- Created .github/workflows/ci.yml (check + test on ubuntu/macos + deny)
- Created deny.toml, clippy.toml, .cargo/config.toml, rust-toolchain.toml
- Created scripts/setup-dev.sh and scripts/check-progress.sh
- Created .claude/settings.json with Stop hook (progress gate), PreToolUse hooks (push gate, commit reminder)
- Created .claude/automations/ directory for iterm2-driver visual tests
- Learning: `cargo nextest` exits 4 with 0 tests unless `--no-tests=pass` is passed
- Learning: `edition = "2024"` requires Rust 1.85+; stable channel is 1.93.1 as of Feb 2026

---

## Task List

| ID | Task | Phase | Status | Depends On |
|----|------|-------|--------|-----------|
| 000 | Repository scaffold and tooling | Bootstrap | **Done** | — |
| 001 | Daemon skeleton and process lifecycle | M0 | **Done** | 000 |
| 002 | Core data model and SessionGraph | M0 | Pending | 000 |
| 003 | Layout engine (binary split tree) | M0 | Pending | 002 |
| 004 | PTY manager | M0 | Pending | 001 |
| 005 | Virtual terminal grid | M0 | Pending | 000 |
| 006 | Input decoder | M0 | Pending | 000 |
| 007 | Event bus | M0 | Pending | 002 |
| 008 | JSON-RPC server foundation | M0 | Pending | 001, 002 |
| 009 | Render compositor (single pane) | M0 | Pending | 005, 006 |
| 010 | Minimal TUI client | M0 | Pending | 004, 008, 009 |
| 011 | CLI foundation (clap) | M0 | Pending | 001, 008 |
| 012 | M0 integration and quality gate | M0 | Pending | 001–011 |
| 013 | Session CRUD (API + CLI) | M1 | Pending | 012 |
| 014 | Window CRUD (API + CLI) | M1 | Pending | 013 |
| 015 | Pane operations (split, focus, resize, zoom, swap, kill) | M1 | Pending | 014, 003 |
| 016 | Pane I/O (send_keys, run_command, capture) | M1 | Pending | 015, 004 |
| 017 | Multi-pane rendering | M1 | Pending | 015, 009 |
| 018 | Tier 1 keybindings (bare keys) | M1 | Pending | 017 |
| 019 | Prefix key system (Tier 2) | M1 | Pending | 018 |
| 020 | Mouse support | M1 | Pending | 017 |
| 021 | Copy mode | M1 | Pending | 019 |
| 022 | TOML config system | M1 | Pending | 012 |
| 023 | Live config reload | M1 | Pending | 022 |
| 024 | Theme engine and token system | M1 | Pending | 022 |
| 025 | Per-pane theming | M1 | Pending | 024, 017 |
| 026 | Status bar (hardcoded, pre-plugin) | M1 | Pending | 025 |
| 027 | Pane titles (manual + auto) | M1 | Pending | 015 |
| 028 | Capability negotiation (ClientCaps) | M1 | Pending | 010 |
| 029 | Synchronized output (Mode 2026) | M1 | Pending | 028 |
| 030 | Session templates | M1 | Pending | 022, 015 |
| 031 | Keybinding configuration and conflict detection | M1 | Pending | 019, 022 |
| 032 | Command palette | M1 | Pending | 019, 031 |
| 033 | Help overlay (keybinding cheat sheet) | M1 | Pending | 032 |
| 034 | M1 integration and quality gate | M1 | Pending | 013–033 |
| 035 | Complete JSON-RPC API surface | M2 | Pending | 034 |
| 036 | Event stream (events.watch) | M2 | Pending | 035, 007 |
| 037 | Optimistic concurrency and ensure operations | M2 | Pending | 035 |
| 038 | Plugin host: wasmtime integration | M2 | Pending | 034 |
| 039 | Plugin permissions and sandbox | M2 | Pending | 038 |
| 040 | Plugin WIT host functions | M2 | Pending | 039 |
| 041 | Plugin lifecycle and hot reload | M2 | Pending | 040 |
| 042 | Event interception chain | M2 | Pending | 041, 036 |
| 043 | Command override system | M2 | Pending | 041 |
| 044 | Process plugin protocol | M2 | Pending | 041 |
| 045 | Plugin API extensions | M2 | Pending | 041, 035 |
| 046 | Overlay system (z-ordered stack) | M2 | Pending | 041 |
| 047 | Inter-plugin event bus | M2 | Pending | 041, 036 |
| 048 | Bundled plugin: shux-status-bar | M2 | Pending | 046, 047 |
| 049 | Bundled plugin: shux-theme-pack | M2 | Pending | 041 |
| 050 | Bundled plugin: shux-diagnostics | M2 | Pending | 046, 045 |
| 051 | gRPC API (optional transport) | M2 | Pending | 035 |
| 052 | M2 integration and quality gate | M2 | Pending | 035–051 |
| 053 | Performance optimization campaign | M3 | Pending | 052 |
| 054 | Shell completions (bash, zsh, fish) | M3 | Pending | 052 |
| 055 | Image passthrough (DCS, Kitty, Sixel, iTerm2) | M3 | Pending | 052 |
| 056 | Fuzzing campaign (ANSI, JSON-RPC, config, layout) | M3 | Pending | 052 |
| 057 | Documentation (README, guides, API reference) | M3 | Pending | 052 |
| 058 | Binary releases and distribution | M3 | Pending | 052 |
| 059 | M3 final quality gate and v1.0 release | M3 | Pending | 053–058 |
