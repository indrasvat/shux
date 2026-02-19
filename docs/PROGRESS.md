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

**2026-02-19 — Task 011: CLI Foundation (clap) — Done**
- Created `crates/shux/src/cli.rs`: Cli struct with clap derive, Command enum (New/Attach/Ls/Kill/Api/Version/__daemon), OutputFormat (Text/Json), RpcClientError, rpc_call() async JSON-RPC client with length-prefix framing, handler functions (handle_ls, handle_new, handle_kill, handle_api, handle_version)
- Created `crates/shux/src/style.rs`: consistent CLI color palette (accent=cyan, success=green, warning=yellow, error=red, muted=dim), respects NO_COLOR convention and IsTerminal check, crossterm Stylize-based Styled helper, print helpers (print_version, print_session_entry, print_no_sessions, print_session_created, print_session_killed, print_error)
- Updated `crates/shux/src/main.rs`: real CLI dispatch with clap::Parser, run_daemon() for __daemon subcommand, run_client() with tracing setup, dispatch() routing all subcommands, styled version fallback when daemon not running, styled error output
- Updated `crates/shux/src/client.rs`: added ensure_daemon_running_at(socket_path) for explicit socket path override
- Created `crates/shux/tests/cli_integration.rs`: 17 integration tests — 5 in-process RPC tests (version, health, session.list, unknown method, concurrent), 5 CLI binary tests against real RPC server (version, version json, ls, ls json, api raw), 7 smoke tests (help, version flag, invalid subcommand, kill requires session, list alias, version without daemon, version json without daemon)
- Added crossterm, serde, serde_json, uuid to shux crate deps; bytes, futures to dev-deps
- 419 tests passing (36 new: 16 unit CLI parsing + 3 style + 17 integration)
- Learning: tokio::process::Command (async) must be used instead of std::process::Command (blocking) in #[tokio::test] to avoid deadlocking the single-threaded runtime when the test also runs a server task

**2026-02-19 — Task 010: Minimal TUI Client — Done**
- Created `crates/shux-ui/src/terminal.rs`: TerminalGuard (RAII raw mode + alt screen + mouse + Kitty keyboard), install_panic_hook (restores terminal before panic), shutdown_signal (SIGTERM/SIGINT)
- Created `crates/shux-ui/src/client.rs`: ClientRequest/DaemonMessage serde types, ClientConfig (prefix key default Ctrl+Space), ExitReason enum, encode_key_event (Ctrl/Alt/arrows/F-keys/nav), parse_key_from_bytes (prefix key detection), parse_resize_event, run_client skeleton (TODOs for daemon wiring in tasks 011/012)
- Created `crates/shux-ui/examples/terminal_demo.rs`: standalone demo exercising TerminalGuard + VirtualTerminal + RenderCompositor + key encoding, with prefix key detach (Ctrl+Space d)
- Created `.claude/automations/test_010_tui_client.py`: L4 visual test with 9 tests (build, alt screen, banner, key echo, enter, arrows, Ctrl+C handling, detach, terminal restore) — all passing
- Updated `crates/shux-ui/Cargo.toml`: added tokio, serde, serde_json, anyhow deps; tempfile dev-dep
- Updated `crates/shux-ui/src/lib.rs`: added client + terminal modules with re-exports
- Updated `docs/tasks/010-minimal-tui-client.md`: added L4 visual testing section
- 41 new tests (2 terminal, 39 client), 383 total passing
- Learning: `parse_key_from_bytes` must handle Enter (0x0d) and Tab (0x09) before the Ctrl range (1..=26), since \r=Ctrl+M and \t=Ctrl+I overlap
- Learning: `enable_raw_mode()` is global (not per-thread), so `spawn_blocking` for crossterm event polling avoids blocking the async runtime

**2026-02-18 — Task 009: Render Compositor (Single Pane) — Done**
- Created `crates/shux-ui/src/buffer.rs`: RenderCell, RenderAttrs, FrameBuffer (double-buffered), DirtyCell, From<&shux_vt::Cell> conversion
- Created `crates/shux-ui/src/render.rs`: RenderBackend<W: Write> with style tracking, render_diff (synchronized output Mode 2026), render_full, clear/hide/show/set_cursor
- Created `crates/shux-ui/src/compositor.rs`: RenderCompositor<W: Write> orchestrating compose->diff->render, CompositorConfig (border, status_bar_height), RenderStats, border rendering with Unicode box-drawing chars
- Created `crates/shux-ui/src/vt_convert.rs`: vt_color_to_crossterm mapping (Default->None, Indexed->AnsiValue, Rgb->Rgb)
- Updated `crates/shux-ui/Cargo.toml`: added shux-vt dependency
- Updated `crates/shux-ui/src/lib.rs`: added buffer, compositor, render, vt_convert modules with re-exports
- 44 new tests (17 buffer, 13 compositor, 11 render, 3 vt_convert), 342 total passing
- Performance: 80x24 full render completes well under 8ms budget (Vec<u8> sink)
- Learning: When RenderCompositor borrows `&mut W`, tests that need multiple render passes should use `Cursor<Vec<u8>>` (owned by compositor) instead of `&mut Vec<u8>` to avoid borrow conflicts
- Learning: crossterm 0.29 `SetAttribute(Attribute::Reset)` resets fg/bg too, so attribute changes must re-emit color sequences afterward

**2026-02-18 — Tasks 005, 006, 007, 008: VT Grid, Input Decoder, Event Bus, JSON-RPC**
- Completed: all four tasks implemented in parallel
- Task 005: Virtual terminal grid (shux-vt) — cell, grid, cursor, vte parser, VirtualTerminal API
- Task 006: Input decoder (shux-ui) — key types, modifiers, crossterm event translation
- Task 007: Event bus (shux-core) — typed event taxonomy, broadcast pub/sub, sequence numbers, history
- Task 008: JSON-RPC server (shux-rpc) — error codes, codec, router, UDS/TCP server, builtin methods

**2026-02-18 — Tasks 002, 003, 004: Core Data Model, Layout Engine, PTY Manager**
- Created `crates/shux-core/src/model.rs`: SessionId, WindowId, PaneId (UUID newtypes via macro), Session, Window, Pane, RestartPolicy with serde kebab-case, Version stamps, Tags
- Created `crates/shux-core/src/graph.rs`: SessionGraph (single-writer with ArcSwap), SessionGraphSnapshot (immutable reads), GraphCommand (13 mutation variants with oneshot reply), GraphHandle (async convenience methods), run_graph_loop
- Created `crates/shux-core/src/layout.rs`: LayoutNode (Split/Leaf binary tree), Direction, Rect, NavDirection, WindowLayout with zoom save/restore, smart_split (wider→vertical, taller→horizontal), directional_focus (center-distance heuristic), resize_pane with ratio clamping [0.05, 0.95], 1-cell separator
- Created `crates/shux-pty/src/handle.rs`: PtyHandle wrapping pty_process::Pty + tokio::process::Child, PtyConfig, PtySize, PtyError (pty_process::Error for Open/Spawn/Resize, std::io::Error for Read/Write), CWD tracking via /proc/pid/cwd (Linux) or initial_cwd fallback (macOS)
- Created `crates/shux-pty/src/manager.rs`: PtyManager, PtyEvent (Output/Exited/Restarted), run_pty_read_loop with CancellationToken, should_restart, respawn_pty
- Created `crates/shux-pty/tests/integration.rs`: 7 integration tests (spawn_echo, exit_status, failing_command, write_and_read, resize, initial_cwd, pty_event_output)
- Updated workspace Cargo.toml: added `async` feature to pty-process
- 101 tests passing (36 model+graph, 28 layout, 10 pty unit, 7 pty integration, 20 pre-existing)
- Learning: pty-process 0.5 API differs from docs — `open()` returns `(Pty, Pts)`, Command uses consuming builder pattern, `spawn(pts)` takes Pts arg
- Learning: tokio::process::Child `kill()` is async; use `start_kill()` for synchronous kill

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
| 002 | Core data model and SessionGraph | M0 | **Done** | 000 |
| 003 | Layout engine (binary split tree) | M0 | **Done** | 002 |
| 004 | PTY manager | M0 | **Done** | 001 |
| 005 | Virtual terminal grid | M0 | **Done** | 000 |
| 006 | Input decoder | M0 | **Done** | 000 |
| 007 | Event bus | M0 | **Done** | 002 |
| 008 | JSON-RPC server foundation | M0 | **Done** | 001, 002 |
| 009 | Render compositor (single pane) | M0 | **Done** | 005, 006 |
| 010 | Minimal TUI client | M0 | **Done** | 004, 008, 009 |
| 011 | CLI foundation (clap) | M0 | **Done** | 001, 008 |
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
