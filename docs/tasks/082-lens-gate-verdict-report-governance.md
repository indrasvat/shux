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

### Dogfood follow-up (real `bat` visual-regression run, 2026-07-18)

A parallel agent drove `shux lens gate` against a REAL installed tool (`bat` syntax-
highlighting a Rust file) through the full lifecycle (init → missing → create → pass →
seed-regression → catch → bless → CI-refuse). Verdict: **it works as a real visual-regression
gate** — correct frozen exit contract, localized cell-diff regions, governed bless with a real
audit trail; every seeded regression caught, zero leaks. The one reported "silent-pass
(frames=0, exit 0)" BLOCKER was **independently disproven** (exit-0 before compare →
`child_error`/5; exit-0 after a *matching* compare → pass with frames=1; no path to
pass-with-0-frames). Folded from the exercise:

- **Headless heat evidence (`gate::heat`)** — the pixel-perfect proof is now produced by the
  non-interactive gate path, not only `gate review`: on a fail, `<out>/<name>.heat.png` (changed
  cells overlaid / pixel-diff for pixel-only fails) is written and linked in `report.json`'s
  `diff.heat_png`. Fixes the `--out` help that promised heat PNGs it never wrote. Test:
  `fail_writes_a_heat_png_to_out_and_records_it`.
- `missing_golden` now carries a `--on-missing create` DETAIL hint; `BASELINE-APPROVAL.md`
  sections get a leading blank line.
- **Deferred (tracked):** sandbox `PATH` passthrough so real installed tools aren't invisibly
  "not found" + a `wait_for_text`-timeout PATH hint (→ 084 cold-agent DX / 085 docs); one-shot
  daemon spawn+teardown so a CI gate doesn't leave a persistent daemon (→ 083); secret-scanner
  entropy false-positive tuning on long `$PATH`-like strings.

## Defects found + fixed by task 084 (2026-07-19)

The 084 cold-agent gauntlet exercised this task's verdict/bless/report surface against a
real target and found two defects. Both were fixed here and re-gated with
`make test-lens-gate-verdict` (18/18) and `make test-lens-gate-contract` (5/5).

**1. BLOCKER — blessing laundered a scenario-level failure into `pass`/exit 0**
(`gate/bless.rs`, `gate/verdict.rs`). `build_reports` folds THREE contributions into the
scenario status: per-frame statuses, the scenario-level terminal disposition
(`step_timeout` / `child_error` / `infra_error`) and the `no_visual_check` guard.
`apply_blessed` re-rolled after a bless by folding over **frames only**, seeded at `Pass` —
and a terminal failure produces no frames at all, so the fold began and ended at `Pass`.
`shux lens gate scn.toml --on-missing create` therefore returned `pass`/exit 0 over a
scenario whose `wait_for_text` had timed out and which had rendered nothing, while blessing
zero goldens. The note still said `step_timeout`; only the machine-readable `status` and
the exit code — the two things CI reads — lied. It applied to `--update` too, via the same
shared path, so its blast radius was wider than the "create masks a child death" item 083
deferred forward.

Fixed by extracting `verdict::scenario_floor(outcome)` (the non-frame status) and having
`apply_blessed(reports, manifest, floor)` fold from it; `build_reports` derives its
non-frame contribution from the same helper so the two rollups cannot drift again.
Regression test `blessing_nothing_cannot_launder_a_step_timeout_into_pass` was proven to
FAIL against the old fold before the fix landed.

**2. A colour-only regression reported coordinates but never colours** (`shux-vt/src/gate.rs`,
`gate/compare.rs`). The gate's headline capability is catching what a text diff cannot — but
for a `bright_green` -> `green` change the report said only "50 cells changed at rows
4,5,7,9,11" while every text diff of the same frames showed nothing. `DiffReport` gained
`style_deltas: Option<Vec<StyleDelta>>` (`{row, col, expected, actual}`, terse descriptors
like `fg=bright_green bold`), computed in `compare_frame` where both envelopes are in hand,
only on a FAIL, one entry per CONTIGUOUS run of the same (expected, actual) pair, capped at
16. This is a deliberate addition to the frozen `deny_unknown_fields` schema.

The gauntlet proved the field **load-bearing rather than cosmetic**: with two greens on
screen it is what identifies which one the baseline blesses. Claude's run used exactly that
("the summary row had zero changed cells, which independently confirms the summary's green
was the correct half of the pair") to decide to raise the table rather than lower the
summary — a choice that is otherwise a coin flip.

Also: the `missing_golden` detail reached readers as `no committed golden ?`, because an
em-dash met the ASCII output-boundary sanitizer.


## Defects found + fixed by task 085 (2026-07-20)

Task 084's adversarial round recorded ten defects on this task's already-`Done` surface.
Per CLAUDE.md's `Correctness Is Never a Scope Question`, 085 fixed them here rather than
deferring. Each was reproduced by hand first, fixed with a regression test **proven to fail
against the old code**, and re-gated with `make test-lens-gate-verdict` (21/21),
`make test-lens-gate-run` (23/23) and `make test-cli-unit`.

Two were correctness holes in a gate whose job is refusing to let a regression through:

**1. F17 — `--update failing` collided with a frame literally NAMED `failing`**
(`gate/bless.rs`, `gate/scenario.rs`). `select_targets` matched the literal selector before
trying it as a frame name, and the parser accepted `name = "failing"`. Reproduced: a request
to re-bless one *passing* frame blanket-blessed every FAILING frame — the golden sha changed,
the verdict flipped `fail`→`pass`, exit `1`→`0`, and a real colour-only regression was
committed as the new baseline while the frame the author named was never touched. The name is
now reserved at the parser (the choke point that already validates names as path components),
with a defensive guard in `select_targets` for any other construction path. The legitimate
blanket-bless behaviour is unchanged and pinned.

**2. F16 — a bless REFUSAL erased the run's verdict** (`gate/driver.rs`). The orchestrator
discarded the computed reports for a synthetic empty `update_refused` one and returned early,
so a genuine regression became exit `6` with `frames: []` and no heat PNG — `report.json`, the
documented source of truth, recorded that nothing had failed. `shux_vt`'s own
`worst_never_masks_a_regression_with_an_error` forbids exactly this; the orchestrator bypassed
`worst()`. Post-run refusals now roll up through `worst()` (a regression outranks an
operational error, so the run exits 1 with every frame intact) and attach the refusal as a
note. The **CI** refusal deliberately keeps its early return — nothing has run, so there is no
verdict to preserve — and that distinction is now pinned by its own test so the fix cannot be
over-applied.

The rest were diagnostics that made real failures unreadable:

| # | Defect | Fix |
|---|---|---|
| F18 | an I/O error (bad `--report` path) on a PASSING run exited **1**, the frozen regression code, with a bare errno | exit **4**, reserved for CLI-level I/O and previously unreachable, with a message saying the scenario ran and only its output could not be written |
| F19 | a mid-batch bless failure left goldens partially written while reporting a refusal implying none were | the reason now names exactly which frames landed, and the doc comment no longer over-claims "no byte written" for that path |
| F20 | blanket `--update` blessed a fingerprint-mismatched xfail — defeating the one thing a fingerprint exists to do — while `--update <name>` on a valid xfail was refused | no xfail-bearing frame is blessable by either selector; the message says to change the waiver deliberately |
| F21 | `gate init .` wrote a stray `..toml`; `.` cleared every path check but names no file (`..` was already rejected) | dot-only names rejected in `validate_name`; `init` already routes through `parse`, so one fix closed both halves |
| F22 | `-v` wrote ANSI DEBUG to **stdout**, breaking the `--report -` purity contract exactly when someone was debugging | the client's tracing subscriber writes to stderr |
| F23 | heat evidence was dropped silently when `--out` could not be created or written, including when `--out` was explicitly requested | still best-effort (a heat failure must never change the verdict) but now warns on stderr with the path and errno |
| F24 | `missing_golden` never named the directory it searched — the blind spot that let 084's duplicate golden tree go unnoticed | `RunOutcome` carries the golden dir; the reason names it |
| F11 | a `cwd`-containment SECURITY refusal could be redacted to nothing, because `/` is an allowed token character so an ordinary host path is a high-entropy token | when entropy is the ONLY hit, redact the token and keep the sentence; a named rule still discards the whole note |
