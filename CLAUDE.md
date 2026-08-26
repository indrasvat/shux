# CLAUDE.md — shux AI Agent Instructions

> Source of truth for AI agents on shux. AGENTS.md points here. Don't duplicate elsewhere.

shux: terminal multiplexer in Rust. Tiny core, plugin system, typed API, pixel-perfect snapshots.

| | |
|---|---|
| Requirements | `docs/PRD.md` |
| Task archive | `docs/tasks/NNN-*.md` — historical record; don't add new ones |
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
make check-vt-qa        # VT-touching diffs carry tracked QA evidence
make check-test-groups  # nextest groups still bound what they claim
make shellcheck         # every tracked shell script (the guards live in shell)
make install / hooks    # install binary / lefthook hooks
```

## Architecture

```
crates/shux/        CLI entrypoint (clap, daemon auto-start); internal lib so tests reach it
crates/shux-core/   SessionGraph, LayoutEngine, EventBus, config, theme
crates/shux-pty/    PTY manager (openpty, async I/O, lifecycle)
crates/shux-vt/     VT grid (vte parser, VecDeque grid, scrollback)
crates/shux-rpc/    JSON-RPC (UDS + TCP, length-prefixed framing)
crates/shux-plugin/ Plugin host (process plugins over stdio JSON-RPC, permissions)
crates/shux-ui/     TUI client (crossterm, hand-rolled chrome, compositor)
crates/shux-raster/ Grid -> PNG rasterizer (headless snapshots, pixel goldens)
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
Then **re-run the gates the fix falls under** (`make check`, plus `make check-vt-qa` if
it touched VT rendering) and say what you fixed in the commit and the PR — that is where
the record lives now.

**Reproduce before believing — including your own findings.** Every report is a
hypothesis: review agent, QA gate, dogfood, council, or you. A/B against a worktree of
the base commit before attributing a regression.

**Every fix ships with a test seen failing first — failing for the RIGHT reason.** A
test never observed red asserts only that the code does what it does. Run the
UNCHANGED test against the unfixed tree and read the failure message — that is the
only thing proving it catches THIS defect. Sabotaging the assertion proves only
that the assertion is wired; it manufactures a red on any tree. Reach for that
solely when there is no unfixed tree to run against, e.g. when the test itself was
the defect. On #167 a capture test passed because the PTY echoed the marker before
the shell had exited — stable, green, and testing nothing.

**Process hygiene.** Zero leaked daemons or child processes. Use
`.shux/scripts/no_leak_guard.sh` + isolated short `XDG_RUNTIME_DIR`.

Daemon-backed **shell** suites (`.shux/scripts/*_check.sh`, `make test-lens*`, the QA
gates) run **serially** — never two at once. Daemon-backed **cargo tests** do not: they
are scheduled by nextest under the `daemon-pty` group in `.config/nextest.toml`, capped
at 12 concurrent, and each isolates its own `XDG_RUNTIME_DIR`. That cap is measured, not
chosen — see `docs/tasks/093-parallel-test-suite.md`; if you change it there, change it
here. Group membership is matched by pattern, so a new suite joins automatically;
`make check-test-groups` fails if a group goes empty or two groups claim the same test.
Do not add `--test-threads=1` or `-j 1` to a cargo test invocation to "be safe" — that
was the house pattern until issue #130, it cost 461s a run, and it protected nothing that
nextest's process-per-test model does not already protect. If a test genuinely needs a
machine-global resource, put it in a group in `.config/nextest.toml` — prefer a pattern
over naming the binary, and exclude it from any looser group that would claim it first.

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
  a fast success. So does letting a branch END in a pipeline: without `pipefail`,
  `if cmd; then :; else grep x log | tail; fi` exits 0 because the pipeline's status
  becomes the block's. Guards here run `set -euo pipefail`, which covers that case —
  but agent-written one-liners and subagent instructions often do not. Capture
  `status=$?` on the FIRST line of the failure branch, before any diagnostic
  command — put it after the `tail` and you have captured the `tail` — then
  re-exit it. Abort loudly. A guard whose tool is missing must say so
  and exit non-zero — never report success for work it did not do.
- **`make shellcheck` is a gate, and suppressions carry a reason.** The guards are shell,
  so a defect there stops a guard guarding instead of failing a test. Some patterns here
  are deliberate and shellcheck cannot know it — `ps | grep` over `pgrep` (SC2009) is
  *required* by the process-hygiene rule above. Suppress with
  `# shellcheck disable=SCxxxx  # why`, never bare.
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

Hard gates. `VERDICT: PASS` required; `FAIL`/`BLOCKED` must be fixed, or explicitly
re-scoped in the PR description before it ships.

| Gate | Applies to |
|---|---|
| `shux-vt-solid-qa` | `shux-vt`, `shux-raster`, PTY output, pane sizing/resize, capture, snapshot pixels, Unicode width, default colours, cursor, alt screen, scroll regions, terminal responses |
| `shux-tui-qa` | attach UI, keyboard/mouse, copy mode, palette, help, status bar, themes, pane/window/session UX, plugin UX, CLI flows, **agent workflows**, templates, recordings, **rich-TUI compatibility** — when the VT gate doesn't apply |

`shux-simplify-architect` is advisory, not a gate — no `VERDICT`, nothing to pass.
See step 5a.

Defined in `.claude/agents/<name>.md` and `.codex/agents/<name>.toml`.

Both MUST: enforce the Testing Matrix / Acceptance Criteria / DoD the PR states for
itself; use real coloured workloads and real TUIs; inspect full-resolution screenshots and
pixel-verify where a baseline exists (`.claude/automations/pixel_verify.py` via
`uv run --script`); prove zero leaked daemons. **Missing evidence is failure, not residual
risk.**

VT gate PASS report commits to `.shux/qa/<scope>/SOLID-QA.md`, first line exactly
`VERDICT: PASS`. `<scope>` is free-form — name it after the change. `make check-vt-qa`
demands it from any diff touching `crates/shux-vt/`, `crates/shux-raster/` or
`crates/shux-pty/src/capture.rs`, and demands nothing from any diff that doesn't.
The one exemption is the `tests/` and `benches/` trees under those crates: they
ship no cells and no pixels, so the only audit they could carry would be about
code the diff never touched. A diff touching `src/` **and** `tests/` still owes
evidence. `scripts/check-vt-qa-selftest.sh` runs the real guard against a
reintroduced defect and against empty input, and `make check-vt-qa` runs it first.

**Evidence storage.** `.shux/out/<scope>/` is gitignored scratch. Review via PR comments,
not committed binaries. Commit a PNG only as a true baseline/golden, documented in the PR
+ DootSabha approval; then also commit `evidence-manifest.json` and pixel-metric JSON.
With no committed baseline to compare against, the pixel-metric JSON stands alone — don't
commit a screenshot that nothing can be diffed against. Baselines come from committed
`.shux/goldens/` or `.shux/fixtures/` replay output — never mint your own expected PNG and
call it proof.

## Feature protocol

Every feature/fix PR.

1. **Council on the design, before coding.** `dootsabha council --json`; iterate until
   converged. Config from `~/.config/dootsabha/config.yaml`; no CLI agent/chair/model
   overrides. **`dootsabha` unavailable → the council step is not skipped.** Spawn
   context-appropriate parallel adversarial agents on disjoint surfaces instead and say
   in the PR that you did. See *Tooling fallbacks*.
2. **Build with tests** — unit + integration for every new path.
3. **Verify every render path touched** — live attach, `pane`/`window`/`session.snapshot`,
   `events.watch`, web preview. Enumerate the matrix at design time.
4. **Verify every config state** — default, `shux config init`, feature-maxed, malformed,
   hot-reload.
5. **Adversarial review** (`adversarial-review` skill) once green, before the convergence
   council. 2–4 parallel agents on **disjoint** surfaces that drive the real system.
   Reproduce each finding; fix with a regression test.
5a. **Simplification review** (`shux-simplify-architect`) alongside step 5, on any diff
   over ~200 added lines. Cold context, read-only, biased toward deletion; advisory,
   no verdict. Findings are addressed or explicitly declined in the PR. On #170 it
   found a shipping security defect and ~350 removable lines that two bot reviews and
   a QA gate had already passed.

   **5, 5a and 6 run before the first push, before the PR exists.** A finding that
   lands after the push arrives as churn on an open PR.
6. **Council on the implementation diff, before pushing.** `dootsabha council`, or the
   step-1 fallback (parallel adversarial agents on disjoint surfaces) when it is
   unavailable. Not optional, and **not scaled down for small diffs** — a 4-line
   `Cargo.toml` change silently made a benchmark incomparable (#166) and a docs-only
   diff shipped two P1s (#168). Size does not predict defects. Every PR merged in the
   2026-08-15..17 run skipped this step; every one had a real defect found after push.
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
11. **The moment a PR exists, load the `gh-ghent` skill and follow it.** Load it — do
    not run the command from memory. The skill owns the invocation, the cadence, the
    decision order and the reply/resolve semantics; recalling it instead of loading it
    is how half the contract silently gets dropped. It is a loop that runs until the PR
    is done, not a kickoff — re-enter it after every push. **Reading its output IS the
    step**: a finished background task is an unread message, not an answer, and a
    hand-rolled `gh pr checks` poll sees CI while staying blind to review threads.
    Don't report a PR done with unread review comments. **`gh-ghent` or `gh`
    unavailable → monitor by another route, starting the moment the PR exists.** See
    *Tooling fallbacks*.
12. **Post-merge `curl|sh` smoke.** After merge + semantic-release tags, install via
    `curl -fsSL https://shux.pages.dev/install.sh | sh` and smoke the *published* binary.

**PR description: problem, change, measured result, risks.** Detail goes in commits,
evidence in comments. Not a transcript.

Paste into every feature PR:

```
## Verification matrix
- [ ] dootsabha council on design — converged
- [ ] adversarial review — parallel agents drove the real system; findings fixed + regression-tested
- [ ] implementation diff reviewed before push — `dootsabha council`, or parallel adversarial agents on disjoint surfaces; findings addressed
- [ ] every render path touched
- [ ] config states: default · init · feature-maxed · malformed · hot-reload
- [ ] cross-path consistency test
- [ ] `make check` (lint + tests)
- [ ] real-target dogfood — consumer-facing output judged; findings reproduced
- [ ] visual proof for every (render path × config state) cell, attached as a PR comment (or Claude Artifact link); non-visual change → exemption stated
- [ ] `gh-ghent` skill loaded at PR creation, re-entered after every push, output read; every thread answered **and** resolved
- [ ] no screenshots committed unless justified as durable baselines
```

Unfillable cell → explicit callout. **Empty cells without explanation are gaps.**

## Tooling fallbacks

A missing tool never downgrades the step it serves. Substitute, and name the substitution
in the PR.

| Missing | Do this instead |
|---|---|
| `dootsabha` | Spawn context-appropriate **parallel adversarial agents on disjoint surfaces** that drive the real system. Reproduce every finding before believing it; fix with a test seen failing first. |
| `gh-ghent` / `gh` | Monitor the PR from the moment it is created by whatever route exists — `subscribe_pr_activity`, the GitHub MCP tools, scheduled self check-ins. **Reply to and resolve every bot review thread**, exactly as `gh-ghent` would. Webhooks do not reliably deliver CI success, so poll on a timer as well. |
| `browsing-as-you` | Publish a Claude Artifact and link it (already step 8). |

**An agent that rewrites tracked files runs in its own git worktree.** Never point one at
the shared checkout: a `git add -A` during its run commits its scratch. That is not
hypothetical — an adversarial agent's attack payload reached `origin` on PR #147 that
way, and only `make check-test-groups` caught it.

## Git protocol

- Branches: `feat/`, `fix/`, `refactor/`, `docs/`, `chore/`. **Branch before the first
  edit — never commit to `main`.**
- Conventional commits. One feature/fix per PR; reference the issue it closes.
- Hooks: pre-commit fmt+clippy; pre-push full suite + `check-vt-qa`.

**No shared ledger.** There is no progress table, no session log, and no learnings file
to append to. They were a guaranteed merge conflict the moment two branches were open at
once, which is most of the time. Everything worth keeping goes in the commit message and
the PR description — the two places that are already per-branch.

**A guard may read anything, but must never require you to write to a file someone else
is also writing to.** Before adding one, check it derives its answer from the code or the
diff. `docs/tasks/NNN-*.md` are a frozen archive: read them for history, don't add more.

## Key decisions

| Decision | Rationale |
|---|---|
| Cargo workspace, separate crates | Clean boundaries, parallel compilation, independent testing |
| `rust-toolchain.toml` pins stable | PRD requires stable; reproducible builds |
| Hand-rolled JSON-RPC (not jsonrpsee) | jsonrpsee lacks native UDS; matches Zellij |
| cargo-nextest over `cargo test` | Better output, parallelism, JUnit XML, retries |
| VecDeque grid (not alacritty_terminal) | alacritty_terminal too coupled; PRD §15.2 |
| Fork-before-tokio daemonization | Fork in a multi-threaded process is UB; PRD §4.5 |
