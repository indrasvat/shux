# Task 077: shux lens — give every agent eyes

**Status:** In Progress
**Priority:** High
**Milestone:** M3
**Depends On:** 016, 017, 060, 064, 074
**Touches:** `.shux/fixtures/lens/`, `crates/shux/tests/lens_*`, `crates/shux/src/` (P1+), `scripts/check-lens-frozen.sh`, `Makefile`, `lefthook.yml`

> **Normative PRD (gitignored, outside the worktree):**
> `/Users/indrasvat/orca/workspaces/indrasvat-shux/spiderfish/.local/20260704-2326-shux-lens-PRD.md`
> Doc ID `LENS-PRD-20260704`. This task file mirrors that PRD; the PRD wins on
> any conflict (§0 precedence: §3 Decisions > SPEC > TEST > prose).

---

## Problem

Coding agents build and drive terminal apps blind: text capture cannot see
color, alignment, focus, or glyph width. shux's deterministic embedded
rasterizer is the only engine that can close the loop. `lens` exposes it as an
agent loop: **run** (hidden self-cleaning pane) → **settle** → **glance**
(pixels + text of one frame) → fix → **diff** (prove what changed), with PNG
proof.

Five NEW RPC methods: `pane.glance`, `pane.wait_settled`, `pane.checkpoint`,
`pane.diff_since`, `lens.run`; plus two FIELD extensions (`session.create`
scratch params — superseded by DEC-21: scratch is created only by `lens.run`;
and `session.snapshot` pane entries gain `content_revision`). CLI mirrors RPC
1:1. Nothing else (§2.1b verb map).

## Testing Matrix (mirrors PRD §15 — one PR per phase, strict order)

| Phase | Scope | Green gate | Extra DoD |
|---|---|---|---|
| **P0** | Fixtures + entire red suite + stubs (this task, current) | ALL §12 tests fail `method_not_found` / missing field (red receipt); fixture smoke tests green | PRD council convergence; cross-arch PNG spike (RESOLVED: shared goldens, §17); red receipt embedded; this task file |
| **P1** | ContentRevision substrate (§4) | G3, G4 via `session.snapshot` + unit mutation-class table | no render-path behavior change (existing goldens byte-stable) |
| **P2** | `pane.glance` (§5) | G1, G2, G2w + determinism micro-test | SOLID VT QA (glance); goldens approved §16.3 |
| **P3** | `pane.wait_settled` (§6) | S1–S5 (incl. 100× S2) | — |
| **P4** | checkpoints + `pane.diff_since` (§7) | D1–D4, A1 + attached-client concurrency | SOLID VT QA (heat) |
| **P5** | scratch + `lens.run` (§8, §9) | R1–R7 | audit entries asserted; serial-only |
| **P6** | skill rewrite + CLI polish + T-tier + demo (§10, §13) | K1, E1, T1–T4; T5 demo evidence | shux-tui-qa PASS; clean-env skill test |

Dependencies are strict: P2–P4 require P1. Do not parallelize phases; do not
implement during P0.

## Acceptance Criteria (per-phase green gates)

- **P0:** every `crates/shux/tests/lens_*` synthetic test FAILS rooted in a
  missing RPC method (`-32601`) or a missing result field; `lens_fixtures_smoke`
  is GREEN; frozen-path guard + Makefile lane wired; `make check` clean (the red
  suite lives in a `test = false` lane, so `make test` does not run it).
- **P1:** G3, G4 green (revisions read via `session.snapshot`, LENS-R-006).
- **P2:** G1, G2, G2w green (CLI + RPC).
- **P3:** S1–S5 green (S2 is the 100× flake gate).
- **P4:** D1–D4, A1 green.
- **P5:** R1–R7 green under `no_leak_guard.sh`, serial.
- **P6:** K1, E1, T1–T4 green; T5 unaided-agent demo evidence.

## Definition of Done (per PRD phase DoDs)

Every phase implicitly includes: `make check` clean · leak-guard serial run
clean · no frozen red-suite file modified without the `LENS-TEST-CHANGE:`
trailer (§16.2) · `docs/PROGRESS.md` + this Status updated · **converging
dootsabha review per §2.4 (REQUIRED)**. Phase-specific DoDs are the §4–§10 DoD
checklists in the PRD.

## P0 deliverables (this phase)

- Fixtures `.shux/fixtures/lens/f1..f10` (§11): POSIX sh + printf, token-handshake
  paced (no sleeps), shellcheck-clean, truecolor + 256 + basic content each.
- Fixture smoke tests (`lens_fixtures_smoke.rs`) — GREEN, existing machinery only.
- Red suite `crates/shux/tests/lens_*.rs` — 24 synthetic tests (G1,G2,G2w,G3,G4 ·
  S1–S5 · D1–D4 · A1 · R1–R7 · K1 · E1) + RPC twins where marked ⇄.
- T-tier scaffolding (§13): `t/make_nidhi_repo.sh`, `t/demo-app/` (seeded border
  break at col 80), tests T1–T4 (loud-skip when `nidhi`/`vivecaka` absent).
- `scripts/check-lens-frozen.sh` (§16.2) + lefthook `commit-msg` wiring +
  Makefile `check-lens-frozen` / `test-lens` / `test-lens-t`.
- Red receipt: `make test-lens` output captured to `.shux/out/lens-p0/`.

## Mid-flight deltas applied (PRD convergence council)

1. F4 `s`-before-`a` documented NO-OP. 2. F7 `while :; do read -r _ || :; done`
loop (SIGWINCH-interrupt-proof). 3. D4 resequenced `a → settle → checkpoint → s
→ settle → diff`. 4. Explicit repo-relative fixture paths only. 5. glance
`evicted_revision`; zero-delta diff `bounding_box`=0 / `regions_truncated`=false;
`SPAWN_FAILED (-32014)`; FIFO eviction. 6. Scratch is created only by `lens.run`.
