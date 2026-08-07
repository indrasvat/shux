# CLAUDE.md — shux AI Agent Instructions

> Source of truth for AI agents on shux. AGENTS.md points here. Don't duplicate elsewhere.

shux: terminal multiplexer in Rust. Tiny core, plugin system, typed API, pixel-perfect snapshots.

| | |
|---|---|
| Requirements | `docs/PRD.md` |
| Progress tracker | `docs/PROGRESS.md` — keep current |
| Tasks | `docs/tasks/NNN-*.md` |
| Session learnings | `docs/agents/learnings.md` — append every session |
| API + crate notes | `docs/agents/api-notes.md` |
| Releases | `docs/agents/releases.md` |
| Visual testing | `docs/agents/visual-testing.md` |

## Commands

**Always `make <target>`, never raw `cargo`/`lefthook`/scripts.** No target for what you
need? Add one first. Hooks invoke `make`. `make help` for the full list.

```bash
make build / release    # debug / optimized binary
make test               # nextest across workspace
make lint               # clippy -D warnings + fmt-check
make check              # lint + test (pre-commit)
make deny               # license + advisory audit
make check-progress     # PROGRESS.md + task Status current
make check-vt-qa        # completed VT tasks have tracked QA evidence
make install / hooks    # install binary / lefthook hooks
```

## Architecture

```
crates/shux/        CLI entrypoint (clap, daemon auto-start)
crates/shux-core/   SessionGraph, LayoutEngine, EventBus, config, theme
crates/shux-pty/    PTY manager (openpty, async I/O, lifecycle)
crates/shux-vt/     VT grid (vte parser, VecDeque grid, scrollback)
crates/shux-rpc/    JSON-RPC (UDS + TCP, length-prefixed framing)
crates/shux-plugin/ Plugin host (process plugins over stdio JSON-RPC, permissions)
crates/shux-ui/     TUI client (crossterm, hand-rolled chrome, compositor)
```

- Client/server: single binary, daemon auto-starts on first use.
- Single writer, many readers: mutations via mpsc to one state-owner task; reads via ArcSwap.
- CLI == API: every subcommand is a thin JSON-RPC call.
- Events are the integration surface: typed, sequenced, broadcast.

## Hard rules

**Correctness is never a scope question.** Fix every correctness/robustness defect. Never
defer, hand off, or ask the user whether to fix. "Pre-existing" / "out of scope" /
"already Done" / "only a P2" → be careful, not skip. Needs sequencing? Say so while
already doing it. Applies hardest to defects in verification machinery.
Fix it on whichever task's surface owns it, then **re-run that task's gate and frozen
suite and record the fix in its task file** — a fix on a Done task leaves its committed
QA evidence stale and its scope undocumented otherwise.

**Reproduce before believing — including your own findings.** Every report is a
hypothesis: review agent, QA gate, dogfood, council, or you. A/B against a worktree of
the base commit before attributing a regression.

**Every fix ships with a test seen failing first.** A test never observed red asserts
only that the code does what it does.

**Process hygiene.** Zero leaked daemons or child processes. Use
`.shux/scripts/no_leak_guard.sh` + isolated short `XDG_RUNTIME_DIR`.

Daemon-backed **shell** suites (`.shux/scripts/*_check.sh`, `make test-lens*`, the QA
gates) run **serially** — never two at once. Daemon-backed **cargo tests** do not: they
are scheduled by nextest under the `daemon-pty` group in `.config/nextest.toml`, capped
at 12 concurrent, and each isolates its own `XDG_RUNTIME_DIR`. That cap is measured, not
chosen — see `docs/tasks/093-parallel-test-suite.md`; if you change it there, change it
here, and `make check-test-groups` will tell you if the group's membership drifts. Do not add
`--test-threads=1` or `-j 1` to a cargo test invocation to "be safe" — that was the
house pattern until issue #130, it cost 461s a run, and it protected nothing that
nextest's process-per-test model does not already protect. If a test genuinely needs a
machine-global resource, put it in a group and give it an expected member count in
`scripts/check-test-groups.sh`.

Identify processes by **pidfile**; never `pgrep -f`/`pkill -f` on a substring (your own
argv matches it → phantom leaks). Never `pkill -f shux`. A test that counts processes
with `ps` MUST use a per-run unique needle — `ps` is a machine-wide view, and a shared
needle silently reads other suites' processes.

**Rich TUIs must not regress.** `vim`/`nvim`, `lazygit`, `btop`/`htop`, `vicaya`,
`vivecaka` must render correctly in panes. Required pass for any change to PTY spawn,
pane env (`TERM`, `COLORTERM`, `NO_COLOR`, locale), VT parsing, input/mouse encoding,
resize, render composition, rasterization, attach output. `TERM` changes are never "just
environment".

**Colour probes mandatory.** Any daemon-backed test capturing pane output includes
truecolor + indexed + basic colour, so monochrome/`NO_COLOR` regressions can't pass.

**Real workloads over fixtures.** Real panes, real commands, real installed TUIs.
Colour-probed `printf`/`cat` is the letter, not the spirit.

## Verification discipline

- **Prove a check can FAIL before trusting it to pass.** Run every gate/guard/comparator
  against a reintroduced defect AND empty input.
- **Anything that parses cargo output pins `--color never` at the call site.** CI
  exports `CARGO_TERM_COLOR=always`; cargo colours on a TTY and not through a pipe, so
  the coloured path is the one local runs never see. A guard that only fails in CI is
  the worst shape a guard can have — `make check-ci-parity` runs the parsers under CI's
  environment so that failure lands on your machine instead.
- **Never mask failures in a measurement harness.** `|| true` turns an instant error into
  a fast success. Abort loudly.
- **A not-yet-started app is quiet.** `wait-settled` alone races slow starters and
  captures blanks. Require content, then settle.
- **Screenshot-diffing animated TUIs measures capture timing, not rendering.** For
  before/after proof, replay recorded PTY bytes through both versions and compare grids.
- **Open the artifact.** A valid PNG of the right size can be blank. Assert on content,
  never on "file exists".
- **Verify counts before reporting them.**
- **Batch changes while a gate is auditing** — each mid-audit edit costs a full re-run.

## Code conventions

- `rustfmt` + `clippy -D warnings`. Enforced.
- `thiserror` for libraries, `anyhow` for applications; wrap with context.
- No `panic!` outside tests. `unwrap()` only with a comment proving it safe.
- No `unsafe` unless necessary, documented, justified.
- I/O via `tokio`; no blocking in async — `spawn_blocking` for CPU work.
- Tests: `#[cfg(test)]` per file, integration in `tests/`, `proptest` where it fits.
- **All CLI output via `crates/shux/src/style.rs`** — never raw `println!`. Use
  `accent`/`success`/`warning`/`error`/`muted`/`bold` + `print_*` helpers; add a `print_*`
  per new command.
- External reviewer CLIs (Claude/Codex/DootSabha/agy) run through
  `.shux/scripts/agent_review_guard.sh`. No Gemini unless asked.

## Gates

Hard gates. `VERDICT: PASS` required; `FAIL`/`BLOCKED` must be fixed or explicitly
re-scoped in the task file first.

| Gate | Applies to |
|---|---|
| `shux-vt-solid-qa` | `shux-vt`, `shux-raster`, PTY output, pane sizing/resize, capture, snapshot pixels, Unicode width, default colours, cursor, alt screen, scroll regions, terminal responses |
| `shux-tui-qa` | attach UI, keyboard/mouse, copy mode, palette, help, status bar, themes, pane/window/session UX, plugin UX, CLI flows, **agent workflows**, templates, recordings, **rich-TUI compatibility** — when the VT gate doesn't apply |

Defined in `.claude/agents/<name>.md` and `.codex/agents/<name>.toml`.

Both MUST: enforce the task's exact Testing Matrix / Acceptance Criteria / DoD; use real
coloured workloads and real TUIs; inspect full-resolution screenshots and pixel-verify
where a baseline exists (`.claude/automations/pixel_verify.py` via `uv run --script`);
prove zero leaked daemons. **Missing evidence is failure, not residual risk.**

VT gate PASS report commits to `.shux/qa/<task>/SOLID-QA.md`, first line exactly
`VERDICT: PASS`.

**Evidence storage.** `.shux/out/<scope>/` is gitignored scratch. Review via PR comments,
not committed binaries. Commit a PNG only as a true baseline/golden with task
documentation + DootSabha approval; then also commit `evidence-manifest.json` and
pixel-metric JSON. Baselines come from committed `.shux/goldens/` or `.shux/fixtures/`
replay output — never mint your own expected PNG and call it proof.

## Feature protocol

Every feature/fix PR.

1. **Council on the design, before coding.** `dootsabha council --json`; iterate until
   converged. Config from `~/.config/dootsabha/config.yaml`; no CLI agent/chair/model
   overrides.
2. **Build with tests** — unit + integration for every new path.
3. **Verify every render path touched** — live attach, `pane`/`window`/`session.snapshot`,
   `events.watch`, web preview. Enumerate the matrix at design time.
4. **Verify every config state** — default, `shux config init`, feature-maxed, malformed,
   hot-reload.
5. **Adversarial review** (`adversarial-review` skill) once green, before the convergence
   council. 2–4 parallel agents on **disjoint** surfaces that drive the real system.
   Reproduce each finding; fix with a regression test.
6. **Council on the implementation diff, before pushing.**
7. **Capture evidence for every relevant (render path × config state) cell**, named
   `v<N>_<render-path>_<width>_<config-state>.png`. Render path is mandatory in the name
   or two cells collide silently. One default-state screenshot is not the matrix — drift
   hides in the feature-maxed and malformed cells.
8. **Visual proof in a PR comment — MANDATORY for any user-visible change.**
   Non-visual changes (CI config, docs, resource limits, protocol validation) are
   exempt; say so in one line in the PR rather than attaching an artifact that proves
   nothing. Load `browsing-as-you`;
   `cdp.py --json gh-attach --repo O/R --pr N -f shot.png` uploads, then post via
   `gh api repos/O/R/issues/N/comments`. **Skill unavailable (cloud/headless) or
   attachment fails → publish a Claude Artifact, link it in the comment.** Prose-only
   evidence is not done.
9. **Cross-path consistency test** — assert identical logical output across render paths.
10. **Real-target dogfood** for user-facing surfaces. Real binary, real installed TUI/CLI,
    genuine lifecycle. Judge consumer-facing output: `--help` truthfulness, artifact
    exists and reads legibly (OPEN it), errors point at the cause. Reproduce findings
    before believing.
11. **Start `gh-ghent` when the PR is created.** Load `gh-ghent`;
    `gh ghent status --pr N --await-review --solo --logs --format json --no-tui` right
    after `gh pr create`, and after every fix push. Background, never foreground. Don't
    report a PR done with unread review comments.
12. **Post-merge `curl|sh` smoke.** After merge + semantic-release tags, install via
    `curl -fsSL https://shux.pages.dev/install.sh | sh` and smoke the *published* binary.

**PR description: problem, change, measured result, risks.** Detail goes in commits,
evidence in comments. Not a transcript.

Paste into every feature PR:

```
## Verification matrix
- [ ] dootsabha council on design — converged
- [ ] adversarial review — parallel agents drove the real system; findings fixed + regression-tested
- [ ] dootsabha council on implementation diff — clean
- [ ] every render path touched
- [ ] config states: default · init · feature-maxed · malformed · hot-reload
- [ ] cross-path consistency test
- [ ] `make check` (lint + tests)
- [ ] real-target dogfood — consumer-facing output judged; findings reproduced
- [ ] visual proof for every (render path × config state) cell, attached as a PR comment (or Claude Artifact link); non-visual change → exemption stated
- [ ] `gh ghent status --await-review` from PR creation; all threads answered
- [ ] no screenshots committed unless justified as durable baselines
```

Unfillable cell → explicit callout. **Empty cells without explanation are gaps.**

## Git & session protocol

- Branches: `feat/`, `fix/`, `refactor/`, `docs/`, `chore/`. **Branch before the first
  edit — never commit to `main`.**
- Conventional commits. One feature/fix per PR; reference the task number.
- Hooks: pre-commit fmt+clippy; pre-push full suite + `check-progress`.

**Starting a task:** `Status: In Progress` in the task file AND the `docs/PROGRESS.md`
table.

**Completing a task / ending a session:** mark **Done** in both, add a `docs/PROGRESS.md`
session-log entry, append to `docs/agents/learnings.md` if anything was discovered,
commit, push. `scripts/check-progress.sh` blocks the push otherwise.

## Key decisions

| Decision | Rationale |
|---|---|
| Cargo workspace, separate crates | Clean boundaries, parallel compilation, independent testing |
| `rust-toolchain.toml` pins stable | PRD requires stable; reproducible builds |
| Hand-rolled JSON-RPC (not jsonrpsee) | jsonrpsee lacks native UDS; matches Zellij |
| cargo-nextest over `cargo test` | Better output, parallelism, JUnit XML, retries |
| VecDeque grid (not alacritty_terminal) | alacritty_terminal too coupled; PRD §15.2 |
| Fork-before-tokio daemonization | Fork in a multi-threaded process is UB; PRD §4.5 |
