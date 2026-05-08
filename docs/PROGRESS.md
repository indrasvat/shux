# shux — Implementation Progress

> **STRICT RULE:** This file MUST be updated at the end of every coding session.

## Current Phase

**M0: Architecture Spike** — **Complete**. Now in M1: tasks 013–016 + 060 done, **Task 017 in progress** (multi-pane rendering + attach client wiring).

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

**2026-02-19 — Task 016: Pane I/O (send_keys, run_command, capture) — Done**
- Created `crates/shux-pty/src/command.rs`: `CommandEngine` with marker technique for detecting command completion — `start_command()` generates PTY command with `SHUX_MARKER{marker}EXIT{$?}SHUX_END` pattern, `process_output()` scans per-pane output buffers for markers (handles split-across-chunks), `check_timeouts()`, `cancel_command()`, `get_status()`, `shell_escape_args()` — 13 unit tests
- Created `crates/shux-pty/src/capture.rs`: `strip_ansi()` removing CSI, OSC, DCS, 8-bit CSI, character set designation sequences — 5 unit tests
- Added `capture_text(lines)` to `VirtualTerminal` (shux-vt): iterates last N visible rows, extracts cell chars (skipping wide continuations), trims trailing whitespace and empty lines — 2 unit tests
- Wired PTY/VT subsystems into daemon (`crates/shux/src/main.rs`): `PaneIoState` (shared writers map + VT map + CommandEngine), `run_pane_pty_task()` (per-pane async task with select! for concurrent read/write), `spawn_pane_pty()` (spawn shell + VT + read/write task). Updated all `register_*_methods()` to spawn/cleanup PTY on pane create/kill
- Registered 5 new pane I/O RPC methods: `pane.send_keys` (text or base64), `pane.run_command` (sync with marker detection + oneshot, or async), `pane.command_status`, `pane.command_cancel` (Ctrl-C + engine cancel), `pane.capture` (VT capture + strip_ansi)
- Added 3 CLI subcommands: `pane send-keys` (-t text/--data base64), `pane run` (command + args, --timeout, --async), `pane capture` (--lines N)
- Added style helpers: `print_send_keys()`, `print_run_command()` with state-colored output
- Created `crates/shux/tests/pane_io_integration.rs`: 9 integration tests with real PTY processes — send_keys text/base64, nonexistent pane error, capture after echo, run_command sync (echo/false), async+poll, cancel, capture with default lines
- Fixed marker echo bug: shell's PTY input echo contains the literal marker command text, which falsely matches the marker detector before the actual output. Split the echo string (`"SHUX_MAR""KER..."`) so input echo never contains the full pattern
- Added `runtime_ms: u64` to `PaneCommandCompleted` event variant
- 546 tests passing (510 existing + 20 command/capture unit + 7 pane_io integration + 9 event), all make targets pass
- Learning: PTY input echo contains the literal typed command — marker detection must ensure the echo text can't match the marker pattern. Splitting the shell string (`"SHUX_MAR""KER..."`) breaks the echo while shell concatenation produces the correct output.
- Learning: Channel-based PTY write architecture (mpsc sender per pane, tokio task owns PtyHandle with `select!` for read/write) avoids ownership conflicts between `PtyManager::write(&mut self)` and the read loop.

**2026-02-19 — Task 060: Rich CLI Output — Beautiful List Commands — Done**
- Rewrote `crates/shux/src/style.rs` (~1078 lines): added `TerminalContext` (auto-detect TTY, colors, unicode, width), `OutputFormat` (Text/Json/Plain), `BoxRenderer` (Unicode box-drawing frames ╭─╮│╰─╯ with ASCII fallback), `ColumnLayout` (column alignment engine), `SessionInfo`/`WindowInfo`/`PaneInfo` data structs
- Added `render_session_list()`, `render_window_list()`, `render_pane_list()`: box-framed tabular output with `short_id()` (8-char), active markers (filled diamond `◆` cyan, open diamond `◇` dim, arrow `◀ active`/`◀ focus [zoomed]`), summary footers ("3 sessions · 5 windows total"), context headers ("Windows ── session: alpha")
- Added `render_empty_state()`: box-framed empty state with hint text ("(no sessions)" + "Create one: shux new -s my-project")
- Changed all confirmation messages to `✓` prefix with short IDs: `print_success("Created", ...)`, `print_error` now uses `✗` prefix
- Updated `crates/shux/src/cli.rs`: added `Plain` variant to `OutputFormat`, `to_style_format()` converter, `format_created_at()` helper, rewrote `handle_ls`/`handle_window_list`/`handle_pane_list` to use batch renderers
- Auto-detection: piped stdout → Plain format (tab-separated, no box, no color), `NO_COLOR` → box preserved but no ANSI codes, `TERM=dumb` → Plain
- Updated `cli_integration.rs` test assertion for empty session list (piped output is empty in Plain format)
- Created `.claude/automations/test_060_rich_cli_output.py`: 44 visual tests across 13 parts (A–M) covering box frames, column alignment, active markers, short IDs, empty state, zoom state, confirmations, errors, plain format, piped auto-detect, NO_COLOR, multi-session stress, JSON cross-check — ~30 screenshots
- Zero new crate dependencies — all hand-rolled (BoxRenderer ~120 lines, ColumnLayout ~90 lines)
- 510 tests passing, all make targets pass (lint + test)

**2026-02-19 — Task 014: Window CRUD (API + CLI) — Done**
- Added window mutation methods to `SessionGraph` (graph.rs): `create_window`, `destroy_window`, `rename_window`, `focus_window`, `reorder_window` with new `GraphCommand` variants and `GraphError` variants (`WindowNameConflict`, `EmptyWindowName`, `WindowIndexOutOfRange`, `LastWindow`)
- Registered 7 window RPC methods in binary crate (main.rs): `window.list`, `window.create`, `window.kill`, `window.rename`, `window.focus`, `window.reorder`, `window.ensure` — all backed by GraphHandle closures with consistent error mapping via `graph_error_to_rpc()`
- Added `WindowCommand` enum (6 sub-subcommands) to CLI with `Window` variant (alias "win"): List, New, Kill, Rename, Focus, Reorder — each with session name → UUID resolution via `resolve_session_id()` and window spec → UUID resolution via `resolve_window_id()`
- Added 6 style helpers: `print_window_entry`, `print_window_created`, `print_window_killed`, `print_window_renamed`, `print_window_focused`, `print_window_reordered`
- Improved `rpc_display()` in CLI to show human-readable error messages (extracting detail/name/id from RPC data fields) instead of raw "RPC error -32NNN: code_name"
- Added 14 window integration tests (m0_integration.rs): create, auto-name, list, list-missing-session, kill, kill-last-fails, rename, focus, reorder, ensure, new-becomes-active, 3 CLI tests
- Created `.claude/automations/test_014_window_crud.py`: L4 visual test with 25 tests (Parts A–H: setup, creation, auto-naming, focus, rename, reorder, kill, JSON output), 21 screenshots — all passing
- 489 tests passing (458 existing + 38 graph unit + 14 integration + 9 CLI parse - some overlap), all make targets pass
- **Spike fix: stale daemon version handshake** — `ensure_daemon_running_at()` now calls `system.version` after connecting and compares against `env!("CARGO_PKG_VERSION")`. On mismatch, kills old daemon via SIGTERM (PID file), waits for exit, spawns fresh daemon. Prevents `method_not_found` errors after rebuilds.
- Added `build.rs` to both `shux` and `shux-rpc` crates to capture `git rev-parse --short HEAD` at compile time as `SHUX_GIT_SHA` env var. Version handshake now compares both `CARGO_PKG_VERSION` and `SHUX_GIT_SHA` — catches stale daemons even within the same version (e.g., after code changes without version bump).
- Updated `system.version` RPC to include `git_sha` field. `shux version` now displays `shux 0.1.0 (abc1234)`.
- Created `.claude/automations/test_014_version_handshake.py`: 13-test E2E verification of version handshake — builds v1, bumps to 0.1.99, rebuilds, verifies auto-restart (PID changes), verifies git_sha in response, verifies same-version doesn't restart.
- Learning: Improved `rpc_display()` that extracts human-readable messages from RPC error data fields (detail, name+resource, id+resource) makes CLI errors much more user-friendly

**2026-02-19 — Task 013: Session CRUD (API + CLI) — Done**
- Added `NameConflict` error code (-32007) to `shux-rpc` error types with convenience constructor
- Added session name validation to `SessionGraph`: non-empty, max 128 chars, alphanumeric + hyphens + underscores + dots. New `GraphError` variants: `EmptySessionName`, `SessionNameTooLong`, `InvalidSessionName`
- Created `graph_error_to_rpc()` helper mapping `GraphError` → `RpcError` with proper error codes: `SessionNotFound` → `NotFound`, `SessionNameExists` → `NameConflict`, validation errors → `InvalidParams`
- Created `session_to_json()` helper building consistent JSON responses with `window_count`, `active_window_id`, `window_id`, `pane_id` fields
- Enhanced `session.list`: sorted by `created_at`, includes `window_count` and `active_window_id`
- Enhanced `session.create`: returns `window_id` and `pane_id`, auto-generates `session-N` names when no name provided
- Enhanced `session.kill`: accepts `{name: ".."}` OR `{id: "uuid.."}` — tries UUID parse first, falls back to name lookup
- Added `session.rename` RPC method: accepts name or id, resolves to session_id, validates new_name, returns updated session
- Added `Rename` CLI subcommand (`shux rename -s <old> -n <new>`) with `handle_rename()` and `print_session_renamed()` style helper
- Added `FromStr` implementation to `define_id!` macro for UUID parsing in model.rs
- Updated `register_session_methods()` in both test files (`m0_integration.rs`, `cli_integration.rs`) with all 5 methods and proper error mapping
- Created `.claude/automations/test_013_session_crud.py`: L4 visual test with 20 tests (Parts A–F: creation, listing, ensure, rename, kill, error handling), 17 screenshots
- 458 tests passing (437 existing + 5 graph validation unit + 14 integration + 2 CLI parse), all make targets pass
- Learning: `graph_error_to_rpc()` centralizes error mapping — keeps RPC handlers clean and ensures consistent error codes across all session methods
- Learning: Auto-generated session names use `session-N` pattern where N is the count of existing sessions (simple, predictable, avoids conflicts)

**2026-02-19 — Task 012: M0 Integration and Quality Gate — Done**
- Wired RPC Server + SessionGraph into daemon (`crates/shux/src/main.rs`): replaced bare `UnixListener::bind` stub with real `run_rpc_server()` that creates SessionGraph + graph loop + RPC Server
- Added `register_session_methods()`: registers `session.list`, `session.create`, `session.kill`, `session.ensure` backed by GraphHandle closures — lives in binary crate since shux-rpc intentionally doesn't depend on shux-core
- Removed `session.list` stub from `shux_rpc::server::register_builtin_methods()` — session methods now registered at binary level
- Updated `crates/shux/tests/cli_integration.rs`: `start_test_server()` now creates SessionGraph + graph loop, all 17 existing tests continue to pass with real data
- Created `crates/shux/tests/m0_integration.rs`: 17 new M0 integration tests — 10 RPC tests (system.version, system.health, create/list/kill/ensure session, detach-reattach, multiple sessions, invalid method, concurrent connections), 2 PTY tests (spawn echo, exit status), 5 CLI binary tests (version json, ls, new detached, kill, ls json)
- Created `scripts/bench-baseline.sh`: performance baseline script measuring binary size, test count, make target verification; outputs to `docs/m0-baseline.txt`
- Added `bench-baseline` Makefile target
- Created `.claude/automations/test_012_m0_integration.py`: L4 visual test exercising CLI smoke tests (build, new detached, ls, api version, kill, list after kill) with screenshots
- 437 tests passing (420 existing + 17 new M0 integration), all make targets pass (build, test, lint, check)
- **M0 Architecture Spike complete:** all 13 tasks (000–012) done, daemon + SessionGraph + RPC + CLI + PTY + VT + compositor + input + event bus all wired and integration-tested
- Learning: Edition 2024 disallows `unwrap_or(&vec![])` — the temporary `vec![]` is freed while still borrowed. Use `.cloned().unwrap_or_default()` instead.
- Learning: Session RPC methods must be registered in the binary crate (not shux-rpc) because they need GraphHandle from shux-core, and shux-rpc intentionally has no dependency on shux-core. The `register_session_methods()` function is duplicated in main.rs and test files (acceptable since binary crates aren't importable).

**2026-02-19 — Task 011: CLI Foundation (clap) — Done**
- Created `crates/shux/src/cli.rs`: Cli struct with clap derive, Command enum (New/Attach/Ls/Kill/Api/Version/__daemon), OutputFormat (Text/Json), RpcClientError, rpc_call() async JSON-RPC client with length-prefix framing, handler functions (handle_ls, handle_new, handle_kill, handle_api, handle_version), custom clap Styles (cyan headers, green commands, yellow placeholders, red errors)
- Created `crates/shux/src/style.rs`: consistent CLI color palette (accent=cyan, success=green, warning=yellow, error=red, muted=dim), respects NO_COLOR convention and IsTerminal check, crossterm Stylize-based Styled helper, print helpers (print_version, print_session_entry, print_no_sessions, print_session_created, print_session_killed, print_error), banner() with figlet "shux" ASCII art and cyan→blue→indigo gradient (256-color codes 51→45→39→33→27)
- Updated `crates/shux/src/main.rs`: real CLI dispatch with CommandFactory+FromArgMatches (for dynamic banner injection), run_daemon() for __daemon subcommand, run_client() with tracing setup + styled error output, dispatch() routing all subcommands, instant version via try_connect() (no daemon auto-start)
- Updated `crates/shux/src/client.rs`: added ensure_daemon_running_at(socket_path) for explicit socket path override, try_connect() for quick probe without auto-start
- Created `crates/shux/tests/cli_integration.rs`: 17 integration tests — 5 in-process RPC tests (version, health, session.list, unknown method, concurrent), 5 CLI binary tests against real RPC server using tokio::process::Command (async), 7 smoke tests (help, version flag, invalid subcommand, kill requires session, list alias, version without daemon, version json without daemon)
- Created `.claude/automations/test_011_cli_styling.py`: L4 visual test with 7 tests (build, help banner, help headers, help commands, version styled, subcommand help, short help) — all passing, 4 screenshots confirming gradient colors and styled output
- Added crossterm, serde, serde_json, uuid to shux crate deps; bytes, futures to dev-deps
- 420 tests passing (37 new: 16 unit CLI parsing + 4 style + 17 integration)
- Learning: tokio::process::Command (async) must be used instead of std::process::Command (blocking) in #[tokio::test] to avoid deadlocking the single-threaded runtime when the test also runs a server task
- Learning: clap's before_help requires CommandFactory+FromArgMatches pattern for dynamic content (banner with terminal detection); the Styles const can use AnsiColor for consistent branded help output

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
| 012 | M0 integration and quality gate | M0 | **Done** | 001–011 |
| 013 | Session CRUD (API + CLI) | M1 | **Done** | 012 |
| 014 | Window CRUD (API + CLI) | M1 | **Done** | 013 |
| 015 | Pane operations (split, focus, resize, zoom, swap, kill) | M1 | **Done** | 014, 003 |
| 016 | Pane I/O (send_keys, run_command, capture) | M1 | **Done** | 015, 004 |
| 017 | Multi-pane rendering | M1 | **In Progress** | 015, 009 |
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
| 060 | Rich CLI output — beautiful list commands | M1 | **Done** | 011, 015 |
