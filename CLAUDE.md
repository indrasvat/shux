# CLAUDE.md — shux AI Agent Instructions

> **This file is the source of truth for all AI coding agents working on shux.**
> AGENTS.md points here. Do not duplicate instructions elsewhere.

## Project Overview

**shux** is a modern, batteries-included terminal multiplexer built in Rust.
Tiny core, powerful plugin system, first-class support for both humans and AI agents.

- **PRD:** `docs/PRD.md` — full product requirements, architecture, UI specs
- **Use Cases:** `docs/use_cases/shux_plugin_use_cases.md` — plugin architecture validation
- **Progress:** `docs/PROGRESS.md` — implementation tracker (MUST be kept current)
- **Tasks:** `docs/tasks/NNN-descriptive-name.md` — individual task specifications

## Build & Test Commands

```bash
make build                 # Build all crates (debug)
make release               # Build optimized binary → target/release/shux
make test                  # Run tests with cargo-nextest (all workspace crates)
make test-verbose          # Run tests with output visible
make test-lib              # Run library tests only
make clippy                # Run clippy linter
make fmt-check             # Check formatting (no changes)
make fmt                   # Format all code
make lint                  # clippy + fmt-check
make check                 # lint + test (what pre-commit runs)
make ci                    # CI-only target (lint + test-lib + test-doc, fail-fast)
make deny                  # Run license/advisory audit (strict)
make deny-soft             # Run license/advisory audit (non-blocking)
make check-progress        # Verify PROGRESS.md and task Status fields are updated
make check-progress-active # Verify progress (allows In Progress during active session)
make install               # Install to ~/.local/bin/shux
make install-tools         # Install dev dependencies (nextest, llvm-cov, deny, fuzz, lefthook)
make hooks                 # Install lefthook git hooks
make bench                 # Run benchmarks
make doc                   # Build documentation
make clean                 # Remove build artifacts
```

## Architecture

```
crates/shux/           CLI entrypoint (clap, daemon auto-start)
    ↓
crates/shux-core/      Core engine (SessionGraph, LayoutEngine, EventBus, config, theme)
    ↓
crates/shux-pty/       PTY manager (pty-process, async I/O, lifecycle)
crates/shux-vt/        Virtual terminal grid (vte parser, VecDeque grid, scrollback)
crates/shux-rpc/       JSON-RPC server (UDS + TCP, length-prefixed framing)
crates/shux-plugin/    Plugin host (wasmtime, WIT, process plugins, permissions)
crates/shux-ui/        TUI client (crossterm, ratatui for chrome, render compositor)
```

**Key patterns:**
- **Client/server**: Single binary, daemon auto-starts on first use
- **Single writer, many readers**: Mutations via mpsc → single state-owner task; reads via ArcSwap snapshots
- **CLI == API**: Every `shux` subcommand is a thin JSON-RPC call
- **Events as integration surface**: typed, sequenced, via tokio::sync::broadcast

## Code Conventions

- **Format:** `rustfmt` (enforced by CI and pre-commit hook). No debates.
- **Linting:** `clippy` with `-D warnings`. Must pass before commit.
- **Errors:** Use `thiserror` for library errors, `anyhow` for application errors. Wrap with context.
- **No `panic!`** outside test code. Use `Result` everywhere. `unwrap()` only with a comment explaining why it's safe.
- **No `unsafe`** unless absolutely necessary, documented, and justified.
- **Async:** All I/O operations use `tokio`. No blocking in async contexts. Use `tokio::task::spawn_blocking` for CPU-heavy work.
- **Testing:** `#[cfg(test)]` modules in each file. Integration tests in `tests/`. Property tests with `proptest` where applicable.
- **Imports:** `use` statements grouped: std → external crates → workspace crates → local modules. Enforced by `rustfmt`.
- **Makefile is the command interface.** ALWAYS use `make <target>` instead of running raw `cargo`, `lefthook`, or script commands directly. If a task requires a command that has no Makefile target, add one (with proper parameterization) before using it. At the end of each task, audit any new commands discovered during implementation and add them as Makefile targets. All hooks (lefthook, Claude Code) MUST invoke `make` targets, never raw commands.

## Git Workflow

- **Branch naming:** `feat/`, `fix/`, `refactor/`, `docs/`, `chore/`
- **Commits:** Conventional commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`)
- **PRs:** One feature/fix per PR. Reference task number if applicable.
- **Hooks:** lefthook runs fmt+clippy on pre-commit, full test suite on pre-push.

## Session Protocol

> **STRICT RULE — When STARTING a task, you MUST:**
> 1. Set the task file's `Status:` field to `**Status:** In Progress`
> 2. Set the task's status to `In Progress` in the `docs/PROGRESS.md` task table

> **STRICT RULE — When COMPLETING a task (or at end of session), you MUST:**
> 1. Update `docs/PROGRESS.md` — mark task status **Done**, add session log entry
> 2. Update the task file's `Status:` field to `**Status:** Done`
> 3. Update `CLAUDE.md` Learnings section if anything was discovered
> 4. **Commit all changes** with a conventional commit message referencing the task(s)
> 5. **Push to the remote** (`git push`)
> A pre-push hook (`scripts/check-progress.sh`) will block pushes if progress files are not updated.

## Key Decisions

| Decision | Rationale | Date |
|---|---|---|
| Cargo workspace with separate crates | Clean dependency boundaries, parallel compilation, independent testing | 2026-02-18 |
| `rust-toolchain.toml` pins stable | PRD requires stable Rust; pin ensures reproducible builds | 2026-02-18 |
| Hand-rolled JSON-RPC (not jsonrpsee) | jsonrpsee lacks native UDS; hand-rolled matches Zellij's pattern | 2026-02-18 |
| cargo-nextest over `cargo test` | Better output, parallelism, JUnit XML for CI, retry support | 2026-02-18 |
| VecDeque grid (not alacritty_terminal) | alacritty_terminal is too coupled; PRD §15.2 specifies custom grid | 2026-02-18 |
| Fork-before-tokio daemonization | Fork in multi-threaded process is UB; PRD §4.5 specifies this | 2026-02-18 |

## Important API Notes

### Crate Versions (Validated Feb 2026)
- `crossterm` 0.29 — Kitty keyboard, synchronized output, OSC 52
- `vte` 0.15 — with `ansi` feature for typed handler callbacks
- `ratatui` 0.30 — workspace reorganization, used for chrome only
- `wasmtime` 41+ — WASI Preview 2, Component Model, epoch interruption
- `pty-process` 0.5.3 — AsyncRead/AsyncWrite, tokio integration
- `arc-swap` 1.x — lock-free state snapshots
- `clap` 4.x — derive macro, subcommands, completions

### Architecture Patterns
- `SessionGraph` owns all state. ArcSwap for lock-free reads.
- Single-writer mutation channel (tokio::sync::mpsc → state-owner task)
- Event bus: tokio::sync::broadcast + sequence numbers (AtomicU64) + gap detection
- Plugin host: wasmtime Engine + Linker shared; per-plugin Store (dropped on hot reload)

## Visual Testing (L4)

Visual tests use iterm2-driver to automate iTerm2 for screenshot-based regression testing.

```bash
uv run .claude/automations/<test>.py   # Run a visual test script
```

Screenshots are saved to `.claude/automations/screenshots/` (gitignored).
Visual test scripts live in `.claude/automations/` and are added per-task as needed.

## Learnings

> **STRICT RULE:** This section MUST be updated at the end of every coding session.
> Each entry should be a concrete, actionable insight. Delete entries that become obsolete.

- **2026-02-18 (task 000):** `edition = "2024"` requires Rust 1.85+. The `rust-toolchain.toml` pins stable which is ≥1.85 as of Feb 2026, but CI should use `dtolnay/rust-toolchain@stable` to stay current.
- **2026-02-18 (task 001):** Rust edition 2024 makes `std::env::set_var`/`remove_var` unsafe. Wrap in `unsafe {}` with safety comments in tests. Use `tokio::time::pause()` + `advance()` for deterministic timer tests instead of real sleeps.
- **2026-02-18 (task 001):** nix 0.29 requires explicit feature flags per module: `"user"` for `getuid()`, `"process"` for `fork()`/`setsid()`, `"signal"` for signal handling, `"fs"` for `dup2()`. Grace timer pattern: store `Option<tokio::time::Instant>` deadline and use `sleep_until()` inside `select!` async block to avoid `Pin` complexity.
- **2026-02-18 (tasks 002-004):** pty-process 0.5 async API: `pty_process::open()` returns `(Pty, Pts)` (not `Pty::new()`); `Command` uses consuming builder pattern; `spawn(pts)` takes `Pts` arg. Error types: `pty_process::Error` for open/spawn/resize, `std::io::Error` for read/write. Use `child.start_kill()` (sync) instead of `child.kill()` (async) in `PtyHandle::kill()`.
- **2026-02-18 (tasks 002-004):** ArcSwap pattern for single-writer/many-readers: `Arc<ArcSwap<Snapshot>>` shared between GraphHandle (readers) and run_graph_loop (writer). Writer calls `state.store(Arc::new(snapshot))` after each mutation. Readers call `state.load()` for lock-free access. GraphCommand enum with oneshot::Sender reply channels for async request-response.
