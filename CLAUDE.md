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
make build / release       # debug build / optimized release binary
make test                  # cargo-nextest across workspace
make lint                  # clippy -D warnings + fmt-check
make check                 # lint + test (pre-commit)
make ci / ci-strict        # CI target / forces latest stable toolchain
make deny / deny-soft      # license + advisory audit (strict / non-blocking)
make check-progress        # verify PROGRESS.md + task Status fields current
make install               # install to ~/.local/bin/shux
make hooks                 # install lefthook git hooks
make release-build         # build host binary into staging/<triple>/shux
make release-package       # package staged binaries into per-platform tarballs
```

Full target list: run `make help` or read the Makefile. Release pipeline details: [docs/agents/releases.md](docs/agents/releases.md).

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

API surface, crate versions, and core patterns: [docs/agents/api-notes.md](docs/agents/api-notes.md).

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
- **CLI output styling:** All user-facing CLI text output MUST use the style module (`crates/shux/src/style.rs`). Never use raw `println!` for styled output — use the helpers:
  - `style::accent(text)` — Cyan bold, for "shux" brand name and key identifiers
  - `style::success(text)` — Green, for confirmations (created, killed, ensured)
  - `style::warning(text)` — Yellow, for degraded states (daemon not running)
  - `style::error(text)` — Red bold, for error messages
  - `style::muted(text)` — Dim, for secondary info (IDs, timestamps, hints)
  - `style::bold(text)` — Bold white, for primary content (session names, versions)
  - `style::print_*()` functions for common output patterns (version, session entry, errors)
  - Respects `NO_COLOR` env var and `IsTerminal` detection automatically
  - When adding new CLI commands, add corresponding `print_*` helpers to `style.rs`

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
> 3. Append a new entry to `docs/agents/learnings.md` if anything was discovered
> 4. **Commit all changes** with a conventional commit message referencing the task(s)
> 5. **Push to the remote** (`git push`)
> A pre-push hook (`scripts/check-progress.sh`) will block pushes if progress files are not updated.

## Feature Protocol

> **STRICT RULE — Every feature PR follows this protocol. Skipping steps is
> how gaps ship (see PR #43 — snapshot path silently dropped script segments
> because only the default render path was verified).**
>
> 1. **Council-first design.** `dootsabha council --json` on the proposal BEFORE
>    coding. Iterate until critique converges. Use `~/.config/dootsabha/config.yaml`;
>    no CLI agent/chair/model overrides.
> 2. **Build with tests** — unit + integration coverage for every new code path.
> 3. **Verify EVERY render path the feature touches.** Enumerate the full matrix
>    at design time: live `attach` render loop, `window.snapshot` /
>    `session.snapshot` / `pane.snapshot` PNG rasterizer, `events.watch`
>    payloads, web preview, anything else. Drift between paths is the failure mode.
> 4. **Verify EVERY config state.** Not just defaults — also `shux config init`
>    output, feature-maxed config (every `[[...]]` entry populated), malformed
>    config, mid-session hot-reload. The user-configured path is where bugs hide.
> 5. **Local `dootsabha council` review of the implementation diff BEFORE pushing.**
>    Don't wait for codex-bot on the PR to find issues. The goal is the PR
>    shows up *already solid* — codex should react 👍, not write P2 reviews.
> 6. **Visual evidence per (render path × config state) cell.** Save under
>    `.claude/screenshots/<feature>/`, name
>    `v<N>_<render-path>_<width>_<config-state>.png` (e.g.
>    `v1_attach_120_default.png`, `v1_window_snapshot_120_max.png`). Render
>    path is mandatory in the filename — two cells from different paths
>    at the same width + state would otherwise collide and silently
>    overwrite each other, making the matrix unauditable.
> 7. **Cross-path consistency assertion.** At least one test that asserts the
>    same logical output across render paths (e.g., snapshot at width W matches
>    the attach renderer's bar at width W). Prevents future drift.
> 8. **`gh-ghent` post-push, background only** (per memory `feedback-ghent-background`).
> 9. **Post-merge `curl|sh` smoke** (per memory `feedback-post-merge-smoke-test`) —
>    verify against the *publicly-installed* binary, not local `target/release/`.
>
> **Paste this into every feature PR description:**
>
> ```
> ## Verification matrix
> - [ ] dootsabha council on design — converged
> - [ ] dootsabha council on implementation diff — clean
> - [ ] live attach render path
> - [ ] window.snapshot / session.snapshot / pane.snapshot PNG paths
> - [ ] default-config state
> - [ ] `shux config init` state
> - [ ] feature-maxed config state
> - [ ] malformed-config state (gracefully ignored / clear error)
> - [ ] hot-reload state (config edit mid-session takes effect)
> - [ ] cross-path consistency test
> - [ ] `make check` (lint + tests)
> - [ ] visual evidence for every relevant (path × state) cell
> ```
>
> If a cell can't be filled for a *good* reason (e.g., welcome toast doesn't
> render in snapshots by design), call it out explicitly in the PR description.
> Empty cells without explanation are gaps. **Gaps are what the user is going
> to find.**

## Key Decisions

| Decision | Rationale | Date |
|---|---|---|
| Cargo workspace with separate crates | Clean dependency boundaries, parallel compilation, independent testing | 2026-02-18 |
| `rust-toolchain.toml` pins stable | PRD requires stable Rust; pin ensures reproducible builds | 2026-02-18 |
| Hand-rolled JSON-RPC (not jsonrpsee) | jsonrpsee lacks native UDS; hand-rolled matches Zellij's pattern | 2026-02-18 |
| cargo-nextest over `cargo test` | Better output, parallelism, JUnit XML for CI, retry support | 2026-02-18 |
| VecDeque grid (not alacritty_terminal) | alacritty_terminal is too coupled; PRD §15.2 specifies custom grid | 2026-02-18 |
| Fork-before-tokio daemonization | Fork in multi-threaded process is UB; PRD §4.5 specifies this | 2026-02-18 |

## Learnings & Reference Docs

These move out of CLAUDE.md to keep the top-level file scannable — read them
when the relevant topic comes up:

- [`docs/agents/learnings.md`](docs/agents/learnings.md) — dated session
  learnings (Rust edition 2024 pitfalls, vte/crossterm/pty-process quirks,
  PTY winsize rules, render-path parity, plugin permission model, etc.).
  **Append a new entry here at the end of every session per the Session
  Protocol** — do NOT inline learnings back into CLAUDE.md.
- [`docs/agents/api-notes.md`](docs/agents/api-notes.md) — validated crate
  versions and core architecture patterns (SessionGraph, single-writer
  channel, event bus, plugin host).
- [`docs/agents/releases.md`](docs/agents/releases.md) — semantic-release
  pipeline, cross-compile targets, bootstrap process, local testing.
- [`docs/agents/visual-testing.md`](docs/agents/visual-testing.md) — L4
  iterm2-driver automation, screenshot conventions.
