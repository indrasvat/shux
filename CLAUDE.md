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
make ci-strict             # Forces latest stable toolchain (closes version-skew gap)
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
make release-build         # Build release binary for the current host
make release-package       # Package staged binaries into per-platform tarballs
```

## Releases

shux uses Conventional-Commits-driven `semantic-release`, mirroring the
pattern used by [`indrasvat/vicaya`](https://github.com/indrasvat/vicaya).
A single workflow (`.github/workflows/release.yml`) runs on every push
to `main` against a `macos-latest` runner. Inside the workflow:

1. semantic-release analyzes commit history, computes the next
   version, bumps `Cargo.toml`, updates `CHANGELOG.md`.
2. `scripts/build-release.sh` is the `prepareCmd` of
   `@semantic-release/exec` — it cross-compiles four binaries, all
   embedding the freshly-bumped `CARGO_PKG_VERSION`:

   | Target                         | How                              |
   |---|---|
   | `aarch64-apple-darwin`         | native cargo build               |
   | `x86_64-apple-darwin`          | cross via Apple SDK              |
   | `x86_64-unknown-linux-gnu`     | `cargo zigbuild` (glibc 2.17)    |
   | `aarch64-unknown-linux-gnu`    | `cargo zigbuild` (glibc 2.17)    |

3. `@semantic-release/git` commits the version bump (`[skip ci]` so this
   workflow does NOT loop) and pushes a `v<X.Y.Z>` tag.
4. `@semantic-release/github` creates the GitHub release and uploads the
   four `.tar.gz` archives plus their `.sha256` sidecars.

### Bootstrap (first-ever release)

semantic-release defaults to `v1.0.0` for the very first release without
a prior tag. To start at `v0.1.0`, use the manual `workflow_dispatch`
trigger in `release.yml`:

```bash
gh workflow run release.yml -f version=0.1.0
```

This skips semantic-release, runs `set-version.sh` + `build-release.sh`,
and creates the `v0.1.0` GitHub release directly. Subsequent `feat:` /
`fix:` commits on `main` then auto-bump from `v0.1.0`.

### Local testing

```bash
make release-build      # build host binary into staging/<triple>/shux
make release-package    # HOST_ONLY=1 → package whatever staging has
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
> 3. Update `CLAUDE.md` Learnings section if anything was discovered
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

Screenshots are saved to `.claude/screenshots/` (gitignored).
Visual test scripts live in `.claude/automations/` and are added per-task as needed.

## Learnings

> **STRICT RULE:** This section MUST be updated at the end of every coding session.
> Each entry should be a concrete, actionable insight. Delete entries that become obsolete.

- **2026-02-18 (task 000):** `edition = "2024"` requires Rust 1.85+. The `rust-toolchain.toml` pins stable which is ≥1.85 as of Feb 2026, but CI should use `dtolnay/rust-toolchain@stable` to stay current.
- **2026-02-18 (task 001):** Rust edition 2024 makes `std::env::set_var`/`remove_var` unsafe. Wrap in `unsafe {}` with safety comments in tests. Use `tokio::time::pause()` + `advance()` for deterministic timer tests instead of real sleeps.
- **2026-02-18 (task 001):** nix 0.29 requires explicit feature flags per module: `"user"` for `getuid()`, `"process"` for `fork()`/`setsid()`, `"signal"` for signal handling, `"fs"` for `dup2()`. Grace timer pattern: store `Option<tokio::time::Instant>` deadline and use `sleep_until()` inside `select!` async block to avoid `Pin` complexity.
- **2026-02-18 (tasks 002-004):** pty-process 0.5 async API: `pty_process::open()` returns `(Pty, Pts)` (not `Pty::new()`); `Command` uses consuming builder pattern; `spawn(pts)` takes `Pts` arg. Error types: `pty_process::Error` for open/spawn/resize, `std::io::Error` for read/write. Use `child.start_kill()` (sync) instead of `child.kill()` (async) in `PtyHandle::kill()`.
- **2026-02-18 (tasks 002-004):** ArcSwap pattern for single-writer/many-readers: `Arc<ArcSwap<Snapshot>>` shared between GraphHandle (readers) and run_graph_loop (writer). Writer calls `state.store(Arc::new(snapshot))` after each mutation. Readers call `state.load()` for lock-free access. GraphCommand enum with oneshot::Sender reply channels for async request-response.
- **2026-02-18 (task 005):** vte 0.15's `Parser::advance()` accepts a full `&[u8]` slice (not byte-by-byte). The raw `vte::Perform` trait (`print`/`execute`/`csi_dispatch`/`esc_dispatch`/`osc_dispatch`) gives more control than `vte::ansi::Handler` and is the primary trait. VtHandler borrows all VirtualTerminal fields mutably.
- **2026-02-18 (task 008):** Rust edition 2024 requires `Send + Sync` bounds on `Box<dyn std::error::Error>` for tokio::spawn contexts. `ref` patterns in match arms are disallowed in edition 2024 — use `&` patterns instead.
- **2026-02-18 (task 009):** crossterm 0.29 `SetAttribute(Attribute::Reset)` resets fg/bg colors too, so after an attribute change the render backend must re-emit `SetForegroundColor`/`SetBackgroundColor`. Handle attributes before colors in `apply_style()`.
- **2026-02-18 (task 009):** When `RenderCompositor<W: Write>` borrows `&mut Vec<u8>`, tests needing multiple render passes hit borrow conflicts. Use `Cursor<Vec<u8>>` (owned by compositor) or separate compositor instances per render call. The `Cursor<Vec<u8>>` pattern works well with a `make_compositor()` helper in tests.
- **2026-02-19 (task 010):** `parse_key_from_bytes` must handle Enter (`\r`=0x0d) and Tab (`\t`=0x09) as specific match arms BEFORE the Ctrl+A-Z range (1..=26), since \r and \t fall within that range but should map to `KeyCode::Enter`/`KeyCode::Tab` rather than `Ctrl+M`/`Ctrl+I`.
- **2026-02-19 (task 010):** crossterm `enable_raw_mode()` is process-global (not per-thread). For async event loops, use `tokio::task::spawn_blocking` for `crossterm::event::poll()`/`event::read()` to avoid blocking the tokio runtime. The terminal_demo example shows the pattern: poll in main thread with Duration timeout, render after each key.
- **2026-02-19 (task 011):** `tokio::process::Command` (async) must be used instead of `std::process::Command` (blocking) inside `#[tokio::test]` when the test also runs a server task on the same runtime, otherwise the blocking `.output()` call starves the server and deadlocks.
- **2026-02-19 (task 011):** CLI output styling lives in `crates/shux/src/style.rs`. All CLI text output MUST use the style helpers (accent/success/warning/error/muted/bold + print_* functions) for consistent aesthetics. Respects NO_COLOR and IsTerminal. Color palette: accent=Cyan, success=Green, warning=Yellow, error=Red, muted=Dim.
- **2026-02-19 (task 012):** Edition 2024 disallows `unwrap_or(&vec![])` — the temporary `vec![]` is freed while the borrow is still live. Use `.cloned().unwrap_or_default()` instead.
- **2026-02-19 (task 012):** Session RPC methods (`session.list`, `session.create`, `session.kill`, `session.ensure`) must be registered in the binary crate (`crates/shux/src/main.rs`), not in `shux-rpc`, because they require `GraphHandle` from `shux-core` and the RPC crate intentionally has no dependency on core. The `register_session_methods()` helper is duplicated in test files (acceptable since binary crates aren't importable by integration tests).
- **2026-02-19 (task 013):** Centralize `GraphError` → `RpcError` mapping in a `graph_error_to_rpc()` helper function. Each RPC handler calls this mapper instead of ad-hoc error conversion, ensuring consistent error codes (`NotFound`, `NameConflict`, `InvalidParams`, `VersionConflict`) across all session methods. Similarly, `session_to_json()` standardizes response structure.
- **2026-02-19 (task 013):** When an RPC method accepts either a name or UUID identifier (e.g., `session.kill`, `session.rename`), try `SessionId::from_str()` first; if it fails as a UUID, treat it as a name lookup. This dual-mode resolution gives both humans (names) and programmatic clients (UUIDs) convenient access.
- **2026-02-19 (task 014):** Window RPC methods (`window.list/create/kill/rename/focus/reorder/ensure`) follow the same `register_*_methods()` pattern as session methods — registered in binary crate with `GraphHandle` closures, duplicated in test files. `window_to_json()` standardizes response structure with `title`, `index`, `pane_count`, `is_active`, `active_pane_id`.
- **2026-02-19 (task 014):** CLI `resolve_window_id()` tries numeric index parse first, then name lookup — same dual-mode pattern as session resolution. Window commands use `-s session -w window` flags consistently.
- **2026-02-19 (task 014):** `rpc_display()` extracts human-readable messages from RPC error data fields (`detail` for invalid_params, `name`+`resource` for name_conflict, `id`+`resource` for not_found) instead of showing raw "RPC error -32NNN: code_name". Makes CLI errors much more user-friendly.
- **2026-02-19 (task 014):** `ensure_daemon_running_at()` performs a version handshake: after connecting, calls `system.version` and compares against `CLIENT_VERSION` (`env!("CARGO_PKG_VERSION")`) AND `CLIENT_GIT_SHA` (`env!("SHUX_GIT_SHA")`). On mismatch, kills old daemon via SIGTERM (PID file), waits for exit, spawns fresh. Prevents `method_not_found` after rebuilds. The `build.rs` in both `shux` and `shux-rpc` captures `git rev-parse --short HEAD` at compile time.
- **2026-02-19 (task 015):** Pane RPC methods follow the same `register_pane_methods()` pattern as session/window. `resolve_pane_id_from_params()` provides flexible resolution: explicit `pane_id` → window's `active_pane` → session's active window's active pane. `resolve_window_id_from_params()` similarly chains session → active_window. Both helpers are duplicated in test files. clap auto-lowercases variant names, so `Pane` already creates the `pane` command — adding `#[command(alias = "pane")]` causes a panic.
- **2026-02-19 (task 060):** `TerminalContext::detect()` auto-switches Text→Plain when stdout is piped (`!is_tty`) or `TERM=dumb`. This means CLI integration tests that capture stdout via `tokio::process::Command` get Plain format (tab-separated, no box-drawing). Test assertions must match Plain format or explicitly pass `--format text`. Empty lists in Plain format produce no output (standard Unix convention).
- **2026-02-19 (task 060):** Hand-rolled `BoxRenderer` and `ColumnLayout` (~210 lines total) are sufficient for CLI tabular output — no need for `tabled` or `comfy-table` crates. Key pattern: `styled_if(text, colors, fg, bold, dim)` applies ANSI codes only when `colors=true`, enabling the same rendering code path for colored and plain output. `short_id()` truncates UUIDs to 8-char prefix (like git short SHA).
- **2026-02-19 (task 060):** Unicode width pitfalls in box-drawing: (1) Use `unicode-width` crate (`UnicodeWidthStr::width()`) not `.len()` or `.chars().count()` for terminal column calculations. (2) Rust's `format!("{:<width$}")` pads by char count, not display width — use manual `pad_right()`/`pad_left()` helpers with `display_width()` instead. (3) In `BoxRenderer::header()`, the between-corners fill must exclude the corner characters from the prefix length calculation — counting the corner inflates the prefix and makes the header 1 char shorter than rows/footer.
- **2026-02-19 (task 016):** PTY input echo contains the literal typed command. When using a marker technique (`SHUX_MARKER{id}EXIT{$?}SHUX_END`), the terminal's echo of the `echo` command matches the marker detector before the actual output, causing exit_code=None. Fix: split the shell string (`echo "SHUX_MAR""KER..."`) so input echo breaks the pattern while shell concatenation produces the correct output.
- **2026-02-19 (task 016):** Channel-based PTY write architecture: each pane gets an `mpsc::Sender<Vec<u8>>` write channel. A per-pane tokio task owns the `PtyHandle` and uses `select!` for concurrent read (PTY→VT+CommandEngine) and write (channel→PTY). This avoids ownership conflicts between `PtyManager::write(&mut self)` and the read loop that borrows the handle.
- **2026-02-19 (task 016):** `PaneIoState` (shared `Arc<Mutex<>>`) holds `writers` (HashMap<PaneId, mpsc::Sender>), `vts` (HashMap<PaneId, VirtualTerminal>), and `cmd_engine` (CommandEngine). Every `register_*_methods()` function that creates or destroys panes must also spawn/cleanup PTY tasks and VT instances via this shared state.
- **2026-05-08 (task 017):** `tokio::sync::Notify` `notify_waiters()` only wakes tasks **currently awaiting** `.notified()`; if the renderer is mid-CPU when the wakeup posts, it's silently dropped. Use `notify_one()` which queues a permit consumed by the next `.notified().await` — this is the correct primitive for "wake the next render".
- **2026-05-08 (task 017):** `tokio::select!` arm patterns: `Some(x) = recv()` is a **refutable** pattern that silently disables the branch when the channel returns None. You cannot detect channel close this way. Use `res = recv() => { match res { Some(x) => ..., None => break } }` so closing the sender prompt-exits the task.
- **2026-05-08 (task 017):** Multi-pane multiplexer winsize rule: each pane's PTY must be told its **layout rect size**, not the full client size. Apps polling `TIOCGWINSZ` (vim, htop, less) lay themselves out wrong otherwise. The daemon must re-fan winsizes after every layout-changing action (split, zoom, kill, resize, window switch), not just on initial attach + client resize.
- **2026-05-08 (task 017):** Don't infer client terminal size from a pane's VT grid. It creates a self-feeding shrink loop: split pane is half-width → its grid is 40 cols → daemon "infers" 40-col client → resizes compositor → pane shrinks to 18 cols → infers 18-col client, etc. Track client size as authoritative state (`Arc<Mutex<(u16, u16)>>`) updated **only** by `Resize` frames.
- **2026-05-08 (task 017):** Holding `Arc<Mutex<PaneIoState>>` across `.await` on a bounded `mpsc::send()` deadlocks the entire session if any single PTY task gets slow. Pattern: `let tx = { state.lock().await.writers.get(&p).cloned() }; tx.send(...).await` — clone the Sender out, drop the lock, then await.
- **2026-05-08 (task 017):** Interactive input forwarding should use `tx.try_send(bytes)` (drop the keystroke if full) rather than `tx.send(bytes).await` (block the whole attach loop). A backpressured pane shouldn't be able to freeze the user out of detaching or switching panes.
- **2026-05-08 (task 017):** Border-drawing compositor pattern: pane content goes inside a 1-cell-inset viewport (`Rect::new(content.x+1, content.y+1, content.width-2, content.height-2)`), and the outer ring is the border outline. Pass the OUTER content area to `compute_borders` so it can render the outline + inter-pane separators in the gaps reserved by `compute_rects`. Suppress borders entirely when content area is < 3×3.
- **2026-05-08 (task 017):** Daemon-renders-everything attach pattern: client is a thin pipe (writes daemon-supplied ANSI bytes to stdout, polls crossterm events on a separate OS thread, forwards keys as Input frames). Daemon owns the RenderCompositor, walks all VTs in the active window, runs render_multi_pane into a `Vec<u8>`, drains via `std::mem::take(compositor.inner_mut())`, ships base64'd as Render frames at 200ms tick + on render_pulse notify. This matches tmux's architecture and lets multiple clients attach independently.
- **2026-05-08 (task 017 followup):** Spawning user shells: use `<shell> -l -i` (login + interactive), not just `-l`. Many users' `~/.bash_profile` sources `~/.bashrc` gated on `$- == *i*`; without `-i` that branch never fires, so `~/.bashrc` (where starship/atuin/ble.sh init lives) never runs. Same flags iTerm2 uses by default.
- **2026-05-08 (task 017 followup):** Multiplexers must claim `TERM_PROGRAM` (don't inherit). User rc files branch on it (e.g. "skip starship under Warp", "iTerm-specific copy/paste"). Inheriting the parent emulator's value silently fires those branches wrong. Pattern: set `TERM_PROGRAM=<your name>` and `TERM_PROGRAM_VERSION=<your ver>` on every PTY spawn — tmux uses `TERM_PROGRAM=tmux`. shux uses `TERM_PROGRAM=shux`. Also inject `SHUX=1` (mirrors `TMUX` env var) so users can detect they're inside shux.
- **2026-05-08 (task 017 followup) — iterm2-driver patterns:** (1) Never use `app.current_terminal_window` — race-prone with parallel scripts. Use `iterm2.Window.async_create()` per script. (2) `Window.async_create()` returns BEFORE iTerm finishes init; the returned object's `current_tab` is None. Sleep ~0.5s, then refresh via `async_get_app()` and find your window by `window_id`. Skipping this is the #1 cause of intermittent automation failures. (3) Multi-level cleanup in `try/finally` — track every window/session, close all in finally even on crash. Add a `cleanup_stale_windows(prefix=...)` janitor at the START of every script too. (4) Screenshots: position-based Quartz correlation works without focus and for non-frontmost windows; `screencapture -l <quartz-id>` with id picked by minimum (Δx*2 + ΔW + ΔH) score. (5) For shell automation use `\n` (LF), not `\r` (CR) — readline replacements like ble.sh map `\r` to "insert-newline" within multiline edits and trap automation. `\n` bypasses the readline keymap entirely.
- **2026-05-10 (PR 3b — optimistic concurrency):** (1) `GraphError::VersionConflict { resource: &'static str, id: String, expected, actual }` — adding `resource`+`id` to the model makes `RpcError::version_conflict(...)` produce the full PRD §8.3 `data` shape without the RPC handler needing to know which entity it's talking about. The error mapper just unpacks the struct fields. (2) Layout ops (resize/zoom/swap) bump every pane's version in the affected window, not just the target's. Without this, `expected_version` checks on sibling panes after a concurrent layout op would silently succeed — pane.version must be a monotonic stamp for "anything visible on this pane changed", not just "name/exit_status changed". (3) Order-of-operations on destroy_pane / destroy_session / destroy_window: ALWAYS mutate the graph FIRST, tear down PTY/VT/writer state second. A stale `expected_version` must reject the destroy before any IO state is touched, otherwise a rejected kill leaves orphaned VTs. (4) `swap_panes(a, b, expected_version)` only checks pane `a` (the anchor) — sibling-bump makes either check equivalent, and checking both halves the success rate of concurrent swaps for no safety gain. (5) `shux api` should print `{result: ...}` xor `{error: {code, message, data}}` on stdout, with `std::process::exit(2)` on the error path. Agents parse the structured envelope; they shouldn't have to scrape human-readable `rpc_display()` text from stderr. (6) Test-file duplicates of register_*_methods() and graph_error_to_rpc() bit me again — every PR that adds an RPC param needs to update them all (`crates/shux/src/main.rs`, `tests/m0_integration.rs`, `tests/cli_integration.rs`, `tests/pane_io_integration.rs`). Worth eventually extracting into a `shux-test-helpers` crate.
- **2026-05-10 (PR 4 — pane titles):** (1) Per-pane title priority: `manual_title > osc_title > command-basename > cwd-basename`. `Pane.title` is the cached priority-resolved value (renderers read it directly); `effective_title()` is the live re-compute fallback. (2) `set_osc_title()` returns `bool changed` so subscribers can fire `PaneTitleChanged` only on visible movement — crucial because bash's `PROMPT_COMMAND` re-emits the same OSC 2 every prompt and we'd otherwise flood the event bus. (3) DO NOT `std::mem::take(&mut self.title)` before `recalculate_title()` — when `auto_title=false` the recalc is a no-op and `take` leaves title empty. Clone instead, then diff. (4) Per-pane PTY task should track `last_osc_title: Option<String>` locally and forward changes to graph OUTSIDE the `io_state.lock()` — holding a Mutex across a bounded mpsc send is the classic deadlock pattern (PR #7 lesson). (5) `MultiPaneFrame.titles: Option<&HashMap<PaneId, String>>` — caller passes `Pane.title` from the snapshot, NOT `vt.title()` (the VT only knows about OSC; it doesn't see manual overrides). Border overlay: ` title ` (space-padded so corners survive), truncated to `rect.width - 4` chars, written onto the pane's top border row in the same color as that pane's border. Suppress when `rect.width < 6`. (6) Pre-existing gap: `session.create` RPC spawns PTY with `command` but stores empty `Pane.command` in graph (codex P2 #10 only fixed this for `apply_batch`). Means `shux new --cmd vim` auto-derives title from cwd, not the command. Standalone fix later. (7) clap tri-state ("title: null clears, omitted leaves alone, set replaces") doesn't map directly to a clap arg. Use two CLI flags (`-t` and `--clear`) with `conflicts_with = "clear"` and synthesize the JSON null in the handler.
- **2026-05-12 (task 044a phase 0 — process plugins v0):** (1) `Subscription` type lives in `shux_core::bus`, not `EventSubscription`. `SubscriptionEvent::Lagged(u64)` is a tuple variant, not struct variant — easy to mis-import from RPC handler shapes. (2) Plugin Manager → Router circular dep is best broken with `Arc<tokio::sync::OnceCell<Router>>`: build the router with `register_plugin_methods(builder, mgr.clone())`, then `mgr.set_router(router.clone())` after `.build()`. Plugin → daemon RPC dispatches through this; tokio::spawn each dispatch so the I/O loop isn't blocked. (3) `pane.send_keys` requires UUID identifier fields (`session_id`/`window_id`/`pane_id`), NOT human names. The CLI handler resolves names → UUIDs before sending; plugins talking RPC directly hit "invalid_params" if they pass `{"session":"name"}` instead of `{"session_id":"<UUID>"}`. The event payload carries UUIDs in `params.data.data.session_id` — use those directly. Worth fixing in v0.next by accepting both forms at the RPC layer. (4) Daemon → plugin event subscription must use `subscribe_filtered(filters)` against an `Option<Subscription>` since plugins with `subscribes: []` should park forever in the select! arm — use `std::future::pending::<()>().await` as the None branch so the arm never fires. (5) Process plugins use `kill_on_drop(true)` on `tokio::process::Command`; combined with a oneshot kill signal + 2s grace via `tokio::time::timeout(_, child.wait())`, that gives a clean shutdown without explicit signal handling. (6) The handshake budget is 5s; long plugin init should happen lazily after sending the manifest. stderr is relayed to daemon `debug!()` logs tagged with the plugin name — no separate log file needed.
- **2026-05-12 (PR #23 follow-up — codex bot review fixes):** (1) **Biased `tokio::select!` + queued shutdown frame = silent force-kill.** `PluginManager::kill()` pushes `plugin.shutdown` onto `inbox_tx` (mpsc, queued) then signals `kill_tx` (oneshot, instant). The I/O loop has `biased;` and checks `kill_rx` first, so the shutdown frame sits in the queue while the kill branch starts the grace timer. Fix: drain `inbox_rx.try_recv()` into stdin on the kill branch BEFORE entering the grace wait. Any time you couple "send a goodbye over channel A, then signal exit over channel B" with a biased select, audit for this. (2) **Dedup + insert must be one lock window.** Two `Mutex::lock()` calls separated by `tokio::spawn` lets two concurrent installs of plugins reporting the same manifest name both pass `contains_key()` and overwrite each other in the HashMap — first child becomes unmanaged. Hold the lock across the entire stage-2 register window; spawn is non-blocking so the window stays tiny. The same pattern applies anywhere you `contains_key → do stuff → insert`. (3) **One canonical event-wire helper, hoisted to the lowest common crate.** `event_to_json` lived in the binary crate while the plugin host hand-rolled `serde_json::to_value(&Event)`. Moved to `Event::to_wire_json()` in `shux_core::event`. Now both `events.watch` consumers and process plugins get the identical shape — and codex bot review caught the drift before it shipped. Pattern: any helper consumed by ≥2 crates belongs in the crate they both depend on, not in whichever one happened to author it first. (4) **The `data.data.session_id` re-wrap is a separate ergonomics fix.** `#[serde(tag = "type", content = "data")]` on `EventData` means the wire shape has a payload nested under `.data.data.*`. Existing automation (`test_036_events_watch.py`) navigates that. Flattening further breaks every existing event consumer — punt to a deliberate breaking-change PR with all consumer updates batched.
- **2026-05-11 (PR #17 — landing page + skill):** (1) Bash `trap '...' RETURN` is NOT function-scoped unless `set -T` (functrace) is enabled — without it the trap persists past the function boundary and fires on every later function return. If the trap body references locals, they're long gone and `set -u` blows up with "unbound variable" AFTER successful completion. Pattern: route per-function tmp files into a script-global tmpdir + EXIT trap; never put `trap '...' RETURN` in a script that doesn't `set -T`. (2) On a Cloudflare Pages site where release-version metadata is staged into the deploy at build time (not fetched at runtime), `connect-src` should be locked to `'self'` — leaving it permissive "for the version fetch" is a stale comment that becomes an attack-surface lie. (3) GitHub Actions `workflow_run` trigger fires for completions on ANY branch by default. To gate a deploy on main, add `if: github.event.workflow_run.head_branch == 'main'` on the deploy job — `branches:` on the trigger itself is a separate, narrower filter and easy to miss. (4) `session.create.cwd` plumbing was a 3-line param-extraction fix that the prior `create_session_with_command(name, cwd, command)` graph method already supported — code/docs lied for months because the RPC handler hardcoded `current_dir()`. Always trace docs → handler → graph end-to-end when reviewing a new RPC surface.
- **2026-05-13 (PR #33 — plugin permission/audit model):** (1) **Identity must NOT be the plugin name.** Council caught this: keying grants and `Pane.created_by_plugin` on the manifest name lets a reinstall under the same name inherit the predecessor's authority + ownership. Per-install UUID at `<state_root>/by-name/<name>` is the fix; name is a display-only link. `created_by_plugin: Option<PluginId>` (typed UUID), not `Option<String>`. (2) **Sensitivity policy belongs on the route, not in a separate match.** `RouterBuilder::register_with_policy(method, Policy::fixed|param_aware, handler)` keeps the classification next to the handler; `Router::assert_every_route_has_policy()` panics at boot if you add a route and forget to classify. Some methods are param-dependent — `events.watch` filter starting with `plugin.<self>.` is `Public`, broader is `ContentRead`; use `Policy::param_aware(|params, plugin_id| ...)` for those. (3) **Audit BEFORE the plugin-only intercept early-return.** `event.publish` and `plugin.state.*` short-circuit before the router; if you only audit on the router-bound branch, those calls are invisible. Pattern: build the AuditEntry once at the top of `dispatch_plugin_frame`, write it on every parsed frame regardless of which branch handles it. (4) **macOS `tempdir()` returns under `/var` which IS a symlink to `/private/var`.** Walking parent components for symlink rejection breaks every test. Only check `symlink_metadata` of the final path; the threat is a symlink AT the grants/audit location, not symlinks in the tempdir prefix. (5) **`unwrap_or_else(PluginId::new)` triggers `clippy::unwrap_or_default`.** `PluginId` has `Default` (via the `define_id!` macro), so use `unwrap_or_default()`. Same applies to any newtype wrapping `Uuid` via the macro. (6) **Manifest `subscribes:` must lock after first install.** Hot reload re-runs handshake → without diff-vs-prior-allowlist, an attacker who edits the plugin's manifest mid-session widens the bus subscription silently. Compare new `manifest.subscribes` to the persisted `grants.subscribes.allowed`; fail handshake on net-new entries. First install snapshots whatever was in the manifest as the baseline. (7) **`Policy::ParamAware` boxed closure trips `clippy::type_complexity`.** Extract the trait-object type as `pub type PolicyFn = dyn Fn(...) + Send + Sync;` first, then wrap in `Arc<PolicyFn>`. (8) **`if d == "allow" { style::success(d) } else { style::error(d) }` doesn't compile** — each `style::*` returns a different `impl Display` opaque type. Call `.to_string()` on each branch (or use a `match` returning a single concrete `String`).
- **2026-05-15 (fix/snapshot-statusbar-segments):** (1) **Render-path parity is a recurring blind spot — anything the attach loop assembles must be re-assembled by every other render path (`window.snapshot`, `session.snapshot`, `pane.snapshot`).** PR #43 wired `populate_bar(&mut bar, &config, &segments)` into the attach loop but the snapshot path only called the first half (`build_status_bar_shared` → `build_snapshot_status_bar`) so user `[[statusbar.segment]]` entries silently vanished from PNGs. Pixel-perfect snapshots only stay honest if every render path runs the same assembly. Treat the snapshot path as "headless attach" and audit each new attach-side enhancement against it. (2) **`SegmentCache::set` should stay module-private — expose `set_for_test` under `#[cfg(test)]` instead.** Production has exactly one writer (the runner task); making `set` `pub` invites other call sites to invent a second writer and we lose the single-source-of-truth property. (3) **Visual proof matrix is cheap.** Capturing `(snapshot × no-segments)` + `(snapshot × rich-segments)` + `(session.snapshot × rich-segments)` takes ~30s with `target/release/shux session create --detached` and confirms the regression and the fix in one pass — far stronger evidence than just "tests pass". The OOTB-baseline shot also catches "fix accidentally always-appends" failure modes. (4) **`build_snapshot_status_bar` is exercisable as a `#[cfg(test)]` unit test inside `main.rs`** — binary crates can host their own tests, and the function only needs a hand-built `SessionGraphSnapshot` + `ConfigHandle` + `SegmentCache` to reach the integration seam. Cheaper than mirroring the snapshot machinery in `pane_io_integration.rs`.
- **2026-05-14 (fix/pane-capture-after-exit-and-hyphen-args — agent friction):** (1) **`Pane.exit_status` is the dead flag — keep the VT.** The PTY-task cleanup at `main.rs:323` used to evict the VT from `io_state.vts` the instant the child EOF'd. The Pane stays in the graph (with `exit_status: Some(code)`), but every subsequent `pane.capture` / `pane.snapshot` / `pane.wait_for` returned `not_found: pane VT`. tmux's `remain-on-exit` model is the right one: keep grid + scrollback until the user explicitly destroys the pane; drop only the PTY-bound writer + resizer (so `send_keys` / `set_size` to a dead PTY fail meaningfully). Codex hit this with short-lived commands and had to wrap them in `sleep`. (2) **clap `allow_hyphen_values = true` for any agent-facing string arg that can plausibly take a `--`-prefixed value.** Agents wait for CLI help text, error strings, and flag names; `shux pane wait-for --text '--search'` silently failed with "unknown flag --search" until you knew to write `--text=--search`. Apply to `--text` / `--regex` on wait-for, `--text` on send-keys. Tradeoff: `allow_hyphen_values` lets a missing value adjacent to a flag silently consume the next flag (`--text --absent` becomes `text="--absent"`); acceptable here because (a) the arg requires a value, so clap still errors on a bare `--text` at end-of-args, and (b) the reported friction is exactly the leading-`--` value pattern. (3) **Test-scaffold mirror discipline.** `pane_io_integration.rs` has its own `run_pane_pty_task` mirroring `main.rs`. When `main.rs` cleanup changes, the scaffold MUST follow or tests pass against buggy mirror-of-buggy-prod logic. Same hazard as the duplicated `register_*_methods` in test files (PR3b lesson). (4) **TDD discipline pays here.** Two red tests written FIRST against current code (`UnknownArgument` for clap, `not_found: pane VT` for capture) made the symptom unambiguous and gave a green oracle for the fix. (5) **`#[arg(allow_hyphen_values = true)]` is per-arg, not crate-wide** — clap doesn't expose a top-level toggle, so opt in deliberately on the args where leading-`--` content is plausible.
