# Task 077: shux lens — give every agent eyes

**Status:** Partial (P0 complete — fixtures + 27-test red suite + freeze guard landed; P1 In Progress; P2–P6 pending)
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
| **P1** _(In Progress)_ | ContentRevision substrate (§4) | G3, G4 via `session.snapshot` + unit mutation-class table | no render-path behavior change (existing goldens byte-stable) |
| **P2** | `pane.glance` (§5) | G1, G2, G2w + determinism micro-test | SOLID VT QA (glance); goldens approved §16.3 |
| **P3** | `pane.wait_settled` (§6) | S1–S5, V1 (incl. 100× S2) | — |
| **P4** | checkpoints + `pane.diff_since` (§7) | D1–D5, A1 + attached-client concurrency | SOLID VT QA (heat) |
| **P5** | scratch + `lens.run` (§8, §9) | R1–R8 | audit entries asserted; serial-only |
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
- **P3:** S1–S5, V1 green (S2 is the 100× flake gate).
- **P4:** D1–D5, A1 green.
- **P5:** R1–R8 green under `no_leak_guard.sh`, serial.
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
- Red suite `crates/shux/tests/lens_*.rs` — 27 synthetic tests (G1,G2,G2w,G3,G4 ·
  S1–S5,V1 · D1–D5 · A1 · R1–R8 · K1 · E1) + RPC twins where marked ⇄.
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

## P0 phase-diff council round 1 (2026-07-05) — hardening applied

1 blocker + 9 majors + 4 minors adjudicated (PRD §A1). Applied: S3 per-check
pump lifetimes (no false-green window) · harness NO_COLOR removed, color cases
assert non-grayscale · CLI twins completed (G1 50/50 split, G2/G2w full-field +
--png file, D1/D2 successful-path diff + --heat file, D3/R5 json error
envelopes, R1 CLI-scratch reap, R3 CLI size path) · D2 byte-exact full-width
rows · G4 session+pane structural versions · NEW tests D5/V1/R8 (count 24→27) ·
frozen guard uses interpret-trailers --parse, HEAD fallback, first-parent merge
diffs · make_nidhi_repo pins commit.gpgsign=false · F2 drains post-READY ·
classify_frame validates exact RGB · G1 single-decode · D-tests assert
from/to_revision. Hardening exposed a real fixture bug: PTY echo of token
newlines corrupted token-paced frames — all token-paced fixtures now set
`stty -echo` (like F4).

## P0 phase-diff council round 2 (2026-07-05) — hardening applied

3 majors adopted (PRD §A1 round-2 entry): (1) EOF busy-spin — the PRD's own
`while :; do read || :; done` prescription spun at 100% CPU on EOF; F2 (and the
F1/F5 blockers) now drain via `cat >/dev/null`, F7 uses the signal-safe
`while read -r _ || [ $? -gt 128 ]; do :; done` (WINCH-interrupt continues, EOF
exits), F4's dd loop breaks on empty read; F2/F7 smoke tests prove
signal-survival and EOF-exit with zero residual processes. (2) G1 pump loops on
a shared done-flag set after all glance threads join (outlives the slowest
glance); 10k-token cap + 120s deadline are panic bounds only; joins collected
non-panicking so the flag is always stored. (3) R8 CLI twin repeats the RPC
twin's daemon-state assertions (zero residual scratch + health).

## P0 phase-diff council round 3 (2026-07-05) — micro-fixes applied

Codex CONVERGED (1 minor) + 1 live-found robustness bug: (1) count_procs
substring match false-matched co-tenant processes whose argv merely mentioned a
fixture filename (proven A/B under a parallel dootsabha run: 8/29 vs 10/27) —
fixture spawns now use the absolute repo-root-anchored path and
count_fixture_procs matches argv anchored at start (`sh <abs>/…/<script>`).
(2) F4's empty-read-as-EOF conflation made explicit: normative input contract
(a/s/Tab only; LF/NUL never sent) added to the fixture header and smoke test.
