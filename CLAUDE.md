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
make check-vt-qa           # verify completed VT tasks have tracked SOLID QA evidence
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
crates/shux-pty/       PTY manager (openpty, async I/O, lifecycle)
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
- **Process hygiene is non-negotiable:** every shux feature/test/automation run MUST leave zero new `shux` daemons or child processes behind; use `.shux/scripts/no_leak_guard.sh` and isolated `XDG_RUNTIME_DIR` cleanup for any daemon-backed command.
- **Leak-guarded shux checks MUST run serially:** never parallelize daemon-backed shux tests or leak-guard wrappers, because each guard intentionally kills new matching processes it did not baseline.
- **External reviewer CLIs are process-hygiene risks:** run Claude/Codex/DootSabha/agy review commands through `.shux/scripts/agent_review_guard.sh`; do not use Gemini for shux review automation unless explicitly requested.
- **Color probes are mandatory in shux automation:** any daemon-backed shux test or fixture that captures pane/window output MUST include explicit truecolor, indexed-color, or basic-color content so monochrome/`NO_COLOR` regressions cannot pass unnoticed.
- **Prefer real terminal workloads:** when behavior is user-visible, tests should exercise real shux panes, Unix commands, and installed TUIs where practical; keep synthetic fixtures for narrow parser invariants, not as the only proof.
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

## Rich TUI Compatibility Guardrail

> **STRICT RULE — Do not regress rich TUI rendering or interactivity.**
> The north star is top-notch, pixel-perfect TUIs rendered inside shux.
> shux is a terminal multiplexer and pixel snapshotter; `vim`/`nvim`,
> `lazygit`, `btop`/`htop`, `vicaya`, `vivecaka`, and other ratatui/Bubbletea/
> curses-style apps must continue to render correctly inside shux panes.
>
> Any change touching PTY spawn, pane environment (`TERM`, `COLORTERM`,
> `TERM_PROGRAM`, `NO_COLOR`, locale), VT parsing, input/mouse encoding,
> resize/winsize, render composition, snapshot rasterization, or attach output
> MUST include a rich-TUI compatibility pass before delivery:
>
> 1. Run the known rich TUI set that exists on the machine: at minimum
>    `lazygit`, `btop` or `htop`, `vim` or `nvim`, plus shux dogfood TUIs such
>    as `vicaya` and `vivecaka` when installed.
> 2. Capture visual evidence through shux, not direct terminal screenshots:
>    `pane.snapshot` for the app surface and `window.snapshot` when borders,
>    titles, status bars, or layout are touched.
> 3. Save screenshots under `.shux/out/<task-or-branch>/` and inspect them for
>    layout corruption, missing color, broken borders, glyph fallback issues,
>    alternate-screen failures, mouse/input regressions, and startup stalls.
> 4. Compare against any existing `.shux/out/rich-tui-parity/` artifacts or
>    relevant goldens when present. If the visual output differs, explain why
>    the difference is intentional; otherwise fix it before handoff.
> 5. Treat these screenshots as scratch evidence by default. For PR review,
>    attach the important screenshots to GitHub PR comments, preferably via the
>    `browsing-as-you` skill so the authenticated GitHub UI stores the images in
>    the PR discussion. Do not commit these screenshots unless they are approved
>    regression baselines or product assets.
> 6. Report the screenshot paths and a concise pass/fail table in the final
>    response. If a TUI cannot be run on the current machine, say exactly which
>    one and why.
>
> Never treat `TERM` or terminal capability changes as "just environment."
> They change what TUIs emit. If changing terminal identity is necessary, prove
> that rich TUIs still render correctly and document the compatibility impact.

## General TUI QA Hard Gate

> **STRICT RULE — user-visible terminal/TUI work MUST pass the general TUI QA gate.**
> Any change touching attach UI, keyboard input, mouse input, copy mode, command
> palette, help, status bar, themes, pane/window/session UX, plugin UX, CLI
> flows, agent workflows, templates, recordings, or rich TUI compatibility MUST
> use the local `shux-tui-qa` sub-agent before the task is marked done or a PR
> is opened, unless the stricter `shux-vt-solid-qa` gate applies.
>
> The sub-agent is defined in:
> - `.claude/agents/shux-tui-qa.md`
> - `.codex/agents/shux-tui-qa.toml`
>
> Its verdict is a hard gate:
> - `VERDICT: PASS` is required to complete the task.
> - `VERDICT: FAIL` or `VERDICT: BLOCKED` must be fixed or explicitly
>   re-scoped in the task file before proceeding.
> - It MUST enforce the active task's exact Testing Matrix, Acceptance
>   Criteria, and Definition of Done when a task file exists.
> - It MUST use real colored shux automation and real Unix/TUI workloads where
>   practical, not synthetic fixtures alone.
> - It MUST visually inspect full-resolution screenshots and use pixel-level
>   verification whenever a baseline, expected frame, or stability contract
>   exists.
> - It MUST prove cleanup: no new `shux` daemons and no new orphan automation
>   processes.
> - Screenshots, transcripts, and pixel diffs from this general gate are scratch
>   artifacts by default. Keep them under `.shux/out/<scope>/` or another
>   gitignored working directory, then attach the review-worthy subset to the PR
>   as comments. Use `browsing-as-you` for authenticated GitHub uploads when
>   `gh` cannot attach images directly.
> - Do not commit general TUI QA screenshots or manifests unless the task
>   explicitly justifies a durable repo artifact and DootSabha agrees. `make
>   check-tui-qa` validates committed manifests only when such an exception is
>   intentionally present.
>
> Run daemon-backed shux QA serially through `.shux/scripts/no_leak_guard.sh`.
> Do not parallelize leak-guarded shux checks.

## VT Quality Hard Gate

> **STRICT RULE — VT/raster/snapshot work MUST pass the SOLID VT QA gate.**
> Any change touching `crates/shux-vt`, `crates/shux-raster`, PTY output
> processing, pane sizing/resize, capture text, snapshot pixels, Unicode width,
> default colors, cursor presentation, alternate screen, scroll regions, or
> terminal request/response behavior MUST use the local `shux-vt-solid-qa`
> sub-agent before the task is marked done or a PR is opened.
>
> The sub-agent is defined in:
> - `.claude/agents/shux-vt-solid-qa.md`
> - `.codex/agents/shux-vt-solid-qa.toml`
>
> Its verdict is a hard gate:
> - `VERDICT: PASS` is required to complete the task.
> - `VERDICT: FAIL` or `VERDICT: BLOCKED` must be fixed or explicitly
>   re-scoped in the task file before proceeding.
> - The PASS report must be committed at `.shux/qa/<task>/SOLID-QA.md`.
> - The first line of that file must be exactly `VERDICT: PASS`.
> - Committed PNG evidence is allowed only when it is a true regression baseline,
>   golden fixture, or intentionally durable VT/raster evidence approved by the
>   task plan and DootSabha design review. Otherwise keep full-resolution PNGs
>   in scratch storage and attach them to the PR as comments.
> - When committed PNG evidence is justified, the same directory must include a
>   committed `evidence-manifest.json`, at least one full-resolution PNG evidence
>   file, and pixel metric JSON.
>
> The SOLID gate MUST read the active `docs/tasks/NNN-*.md` file and enforce
> that task's exact Testing Matrix, Acceptance Criteria, and Definition of Done.
> Missing evidence is failure, not residual risk.
>
> Pixel-level screenshot verification is mandatory whenever visible terminal
> state is affected. Use `.claude/automations/pixel_verify.py` for exact or
> thresholded PNG comparisons. Contact sheets are useful summaries, but they
> never replace full-resolution individual screenshots and pixel diffs.
>
> `.shux/out/<task>/` is scratch space for bulky intermediate captures and live
> recording output. The default review path is PR comments with attached
> screenshots, not committed binary artifacts. The tracked `.shux/qa/<task>/`
> subset is reserved for durable reports, manifests, and justified baselines.
> Baselines must come from committed `.shux/goldens/` or committed
> `.shux/fixtures/` replay outputs. New or changed baselines require explicit
> task documentation plus DootSabha design-review approval; an implementation
> cannot mint its own expected PNG in the same pass and call it proof.

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
> 5. **Adversarial review (skill: `adversarial-review`).** Once the change is green
>    and BEFORE the convergence review, spawn 2–4 parallel adversarial subagents that
>    **drive the real system** to break it (disjoint attack surfaces; "try to break X"
>    charters). Reproduce every finding independently, fix each with a regression test.
>    Static review + councils miss what real hostile input exposes — this catches the
>    sharpest bugs (a VS16-emoji validator blocker on task 078 that the author and 3
>    councils all passed over). Standard for any nontrivial schema/contract/parser/
>    protocol/guard change; skip only for trivial edits.
> 6. **Local `dootsabha council` review of the implementation diff BEFORE pushing.**
>    Don't wait for codex-bot on the PR to find issues. The goal is the PR
>    shows up *already solid* — codex should react 👍, not write P2 reviews.
> 7. **Visual evidence per (render path × config state) cell.** Save local
>    screenshots under `.shux/out/<feature>/` or `.claude/screenshots/<feature>/`,
>    name
>    `v<N>_<render-path>_<width>_<config-state>.png` (e.g.
>    `v1_attach_120_default.png`, `v1_window_snapshot_120_max.png`). Render
>    path is mandatory in the filename — two cells from different paths
>    at the same width + state would otherwise collide and silently
>    overwrite each other, making the matrix unauditable.
> 8. **PR evidence, not repo cruft.** Attach the review-worthy screenshots to the
>    PR as comments. Prefer `browsing-as-you` for GitHub UI uploads when image
>    attachment is needed. Do not commit screenshots unless they are durable
>    goldens/baselines/product assets with explicit task and DootSabha approval.
> 9. **Cross-path consistency assertion.** At least one test that asserts the
>    same logical output across render paths (e.g., snapshot at width W matches
>    the attach renderer's bar at width W). Prevents future drift.
> 10. **`gh-ghent` post-push, background only** (per memory `feedback-ghent-background`).
> 11. **Post-merge `curl|sh` smoke** (per memory `feedback-post-merge-smoke-test`) —
>    verify against the *publicly-installed* binary, not local `target/release/`.
>
> **Paste this into every feature PR description:**
>
> ```
> ## Verification matrix
> - [ ] dootsabha council on design — converged
> - [ ] adversarial review (`adversarial-review` skill) — parallel agents drove the real system; findings fixed + regression-tested
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
> - [ ] visual evidence for every relevant (path × state) cell is attached to PR comments
> - [ ] no screenshots committed unless justified as durable baselines/assets
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
