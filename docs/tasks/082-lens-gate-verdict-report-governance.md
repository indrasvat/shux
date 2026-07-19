# Task 082: lens gate — verdict, report.json, xfail governance, bless

**Status:** Done
**Priority:** High
**Milestone:** M3
**Depends On:** 081
**Quality Gate:** shux-tui-qa
**Touches:** gate verdict/report module (CLI-side), `crates/shux/src/cli.rs`, `crates/shux/src/style.rs`, `.shux/fixtures/lens-gate/`, `BASELINE-APPROVAL.md` flow

> `shux lens gate` initiative. The layer that turns per-frame compares into a
> governed CI verdict — the reason the gate exists (idea 03).

## Problem

The runner (081) can capture and compare frames, but there is no verdict rollup, no
machine-readable report, no exit contract, no first-run behavior, and no safe way to
update goldens. `diff` is "data, not a verdict" — this task supplies the verdict.

## Scope

1. **Verdict model** (the full closed disjoint status set frozen in 078, council #3):
   `pass/fail/xfail/xpass/missing_golden/xfail_expired/stale_golden/child_error/`
   `settle_never_stable/scenario_error/infra_error/update_refused`. Scenario status =
   worst frame. `stale_golden` (from 080's fingerprint check) is owned here in the
   verdict/exit model; `palette_unportable` is a `fail` **reason string**, never a
   status. This task also maps 081's raw signals (child died / never-settled / step
   timeout) to their statuses + exit codes.
2. **report.json** — top-level array, snake_case, skip-if-none, pretty (CI-greppable):
   per-scenario `{scenario, status, os, arch, font_chain_sha256, font_size_px,
   started_at_ms, duration_ms, frames[], note?}`; per-frame `{name, status, golden,
   diff{changed_cells,total_cells,max_channel_delta?,heat_png?,regions?},
   capture_png?, capture_json, child_exit?}`. This is the **source of truth**; exit
   codes are the coarse signal.
3. **Plain-ASCII `summary_table`** to stdout (grok-adapted): one row per frame —
   `NAME | STATUS | CHANGED-CELLS | TIME | DETAIL` — for `| tee` in CI. Separators are
   **ASCII only** (council #4: no middle-dots/box-drawing — the table must survive a
   `NO_COLOR`, non-UTF-8 CI log).
4. **Exit contract** exactly per §7.4 (0 pass · 1 regression{fail/xpass/missing(CI)/
   xfail_expired/**stale_golden**/**settle_never_stable**} · 2 usage · 3 infra · 4 perm
   · 5 child_error · 6 update_refused). Status names are exactly the frozen 078 set —
   `settle_never_stable`, never the short form (council #4 drift fix). No collision with
   `lens run --wait` (the gate owns the child). **Report privacy** (council #3): reports never dump full `env`/`argv` —
   only hashed provenance (`cmd_env_hash`); a test asserts no secret-bearing env/argv
   text appears in `report.json` or the summary.
5. **First-run / `--on-missing fail|create`**: default `fail` (CI-safe) → exit 1;
   `create` writes a first golden locally (never in CI by default).
6. **xfail governance** (council #1 MAJOR): xfail declared inline per frame with
   mandatory `reason/owner/issue/expiry` + optional `fingerprint`. **xfail** = green;
   **xpass** (now matches) = force-promote (exit 1); **xfail_expired** (past expiry) =
   exit 1. A fingerprinted xfail only holds for THAT diff — a different mismatch fails.
7. **`--update` safety** (council #1 MAJOR): refuses on dirty worktree; defaults to
   **failing-frames-only**; runs a pre-bless secret scan (`update_refused` on hit);
   never silently blesses an xfail; appends who/when/why to `BASELINE-APPROVAL.md`;
   writes a **changed-golden manifest** (names + old/new fingerprints + heat PNG links)
   for PR review.
8. **`shux lens gate review`** — **insta-style, made visual** (DECIDED, Ārya 2026-07-17,
   proposal §12/§14). Adopt `insta`'s interaction model (step through each changed frame,
   accept/reject/skip) — NOT its cargo/`#[test]` coupling — and elevate it with shux's
   rasterizer: **render the before/after golden + heat overlay inline** via the
   kitty/iTerm2 graphics protocol when the terminal supports it (this folds in "inline
   snapshot display", field-note idea 05), else write the PNGs to `.shux/out/` and print
   paths. This is the local human path; CI uses non-interactive `gate` (fail on drift) and
   agents/scripts use guarded `--update [failing|<name>]` (`insta`'s `--accept` analog).
   The inline-graphics rendering itself may land as a thin follow-on if it risks 082's
   scope, but the review loop + accept/reject + PNG-path fallback ship here.
9. **`shux lens gate init <scn>`** (council #3 — proposal §16 had no implementing owner):
   scaffold a scenario `.toml` from an interactive run and write its first goldens under
   the same approval-gated path as `--on-missing create`. Owned HERE (golden creation is
   this task's domain); 085 only *documents* it.

## Non-Goals

- No new capture/compare/tolerance logic (080) or settle modes (083).
- No parallel scenarios.

## Design Review Decisions

DootSabha design review MUST confirm: the exit-code table is disjoint and complete,
the xfail metadata contract + expiry semantics, the `--update` guard set, and the
`gate review` interaction model.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 verdict | Worst-frame rollup; every status maps to the correct exit code; report.json validates against the frozen schema (078). |
| L1 xfail | xfail=green; xpass→exit 1 + force-promote message; expired xfail→exit 1; fingerprinted xfail rejects a *different* mismatch. |
| L1 first-run | `--on-missing fail` → exit 1 + `missing_golden`; `create` writes a golden locally and refuses in CI mode. |
| L1 update | `--update` refuses dirty tree, refuses on secret hit (`update_refused`), never blesses xfail silently, writes manifest + approval log. |
| L1 summary | ASCII summary table is deterministic, ANSI-free, aligned. |
| L1 stale | `stale_golden` (fingerprint mismatch from 080) maps to exit 1 and a distinct report status; not confused with `fail`. |
| L1 privacy | `report.json` + summary carry no raw `env`/`argv`/secret text — only `cmd_env_hash`. |
| L2 CLI flags | `--report PATH`, `--format json|text`, `--out DIR`, global `--tol`, `--update <name>` (named), `--on-missing` each covered with a CLI test. |
| L2 retries (parse/plumb only) | `--retries N` parses, plumbs to the runner, and is exposed in `report.json`. Retry **behavior** + anti-masking is 083's (council #4 split); 082 asserts only that the value is carried and reported. |
| L2 init | `gate init <scn>` scaffolds a scenario + first goldens under the approval-gated path; refuses in CI mode. |
| L2 review | `gate review` accepts/rejects changed frames; a rejected frame stays failing. |
| L3 dogfood | Full gate run on a fixture TUI: green run (exit 0), seeded regression (exit 1 + heat PNG + report), intended change → bless → green. |
| L3 QA | `shux-tui-qa` gate `VERDICT: PASS`. |

## Acceptance Criteria

- [ ] Complete disjoint status model + collision-free exit contract implemented.
- [ ] report.json + ASCII summary emitted and schema-valid.
- [ ] First-run/`--on-missing` behavior correct in CI and local modes.
- [ ] xfail governance (metadata, expiry, xpass-promote, fingerprint) enforced.
- [ ] `--update` is safe by construction; `gate review` works.
- [ ] Exit codes never collide with the target child's exit.

## Definition of Done

- [x] DootSabha design review incorporated before coding. (verdict REVISE → all pin-downs
      folded; `.shux/qa/082-*/dootsabha-design.json`.)
- [x] Red tests captured before implementation. (Frozen RED `lens_gate_contract` lane +
      per-module unit tests written test-first.)
- [x] L1/L2/L3 tests pass. (105 gate unit + `lens_gate_verdict` 17 daemon-backed + contract 5/5.)
- [x] `make check` + `make test-lens-gate-verdict` pass. (Full workspace green; clippy+fmt clean.)
- [x] `shux-tui-qa` `VERDICT: PASS`; scratch evidence under `.shux/out/082-qa/` (gitignored).
- [x] Implementation-diff DootSabha convergence review clean or addressed.
      (`.shux/qa/082-*/dootsabha-impl.json`; #6 clean-exit fix folded, rest documented.)
- [x] `docs/PROGRESS.md` + this task updated; learnings appended.

## Notes

- **Adversarial review (4 agents, real system):** fixed a BLOCKER delayed-post-compare-crash
  false-pass (grace `500ms → min(deadline, 2s)` + abnormal-exit-only), a MAJOR secret-scanner
  blind spot (scanned serialized JSON not reassembled visible text → wrapped/styled secret
  slipped through), a MAJOR note secret-leak + summary `|`/ANSI/non-ASCII injection, a MAJOR
  pixel-only-diff `max_channel_delta` loss, non-transactional bless audit, and several nits —
  each pinned with a regression test.
- **Divergence (impl-review #1):** on a MATCH, xfail metadata validation is skipped and the
  frame is `xpass` regardless of expiry/accountability. Kept per the DESIGN council's rule
  (accurate primary signal "remove the obsolete xfail"; `xpass` is exit 1 so no regression
  escapes). Documented at `verdict.rs` frame_status. Flag for future review if consistency is
  preferred over the match-path signal.
- **Residual (→ task 083 settle-hardening):** the post-compare crash-watch is a bounded 2s
  grace; a crash beyond it while the child is idle is still missed. Robust liveness monitoring
  is 083's domain; the scenario deadline is the ultimate bound.
