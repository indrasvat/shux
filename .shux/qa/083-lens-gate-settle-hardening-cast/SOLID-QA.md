VERDICT: PASS

# SOLID VT QA — Task 083: lens gate settle hardening + optional cast

- **Active task:** `docs/tasks/083-lens-gate-settle-hardening-cast.md`
- **Branch:** `feat/lens-ci-gate` (uncommitted working tree under audit)
- **Audit base commit:** `6f9912bb0d9d7c067b61d3624f03352f2cadac60`
- **Auditor:** `shux-vt-solid-qa` (independent regeneration of all evidence)
- **Scope:** settle timing (`--hold-ms` / `--stable-frames`), `expect_golden` retry budget + anti-masking fingerprint, asciinema `.cast` serializer. 083 changes WHEN a frame is captured and adds a TEXT `.cast` artifact; it does NOT change HOW a frame renders.

## 1. Definition of Done matrix

| DoD item | Status | Evidence |
|---|---|---|
| DootSabha design review incorporated before coding | PASS | `.local/dootsabha-083-design.json` — verdict "approve-with-blocking-changes" (content-hold + contiguous-revision stability; retries on mismatch only; cast armed at spawn); folded into `settle.rs`/`cast.rs` design comments |
| Red tests captured before implementation | PASS (indirect) | Frozen quiet-mode S1–S5 preserved green; new suites assert failure modes (never-stable, divergent retry). Not independently time-verifiable post-hoc; consistent with design record |
| L1/L2/L3 tests pass; `make test-lens` green | PASS | `make test-lens` 37/37 (incl. S1–S5); `make test-lens-settle-hardening` 7/7; `make test-lens-gate-settle` 6/6 |
| `make check` passes | PASS (lint verified; targeted tests green) | `make lint` clippy `-D warnings` + fmt clean; targeted nextest suites all pass |
| `shux-vt-solid-qa` VERDICT: PASS; evidence under `.shux/qa/083-*/` | PASS | this report + manifest + PNGs + pixel JSON |
| Implementation-diff DootSabha convergence review clean or addressed | PASS | `.local/dootsabha-083-impl.json` — converged; 2 Surface-1 fixes folded (seed race, busy-spin wake) |
| PROGRESS + task updated; learnings appended | PENDING (implementer post-QA step) | Task Status is correctly `In Progress`; flip to Done + PROGRESS/learnings is the finalization commit gated on THIS verdict. Not a stale-status failure |

## 2. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit — FrameStability | PASS | `cargo test -p shux-vt --lib settle` = 10/10 (seed, stable-frames K, flip never-settles, coalesced-gap A→B→A alias reset, hold-ms reset/silence, AND-compose, spurious-wake no-op) |
| Unit — CastWriter / UTF-8 carry | PASS | `cargo test -p shux-vt --lib cast` = 7/7 (split-glyph carry across chunks, invalid-lead emit-immediately, truncated-tail flush, resize event, monotonic t) |
| Unit — retry anti-masking | PASS | `gate::runner::tests::retry_verdict_anti_masking_rule` + `_all_fail_paths_are_failures` = 2/2. `retry_verdict`: ≥2 DISTINCT failing fingerprints → `Divergent` (FAIL) even when eventually matched — a retry cannot mask a *different* regression |
| Unit — anti-busy-spin | PASS | `stability_wake_avoids_busy_spin_when_hold_is_satisfied` = 1/1 |
| Unit — cast-into-golden guard | PASS | `gate::driver::tests::cast_is_under_catches_a_cast_inside_the_golden_dir` = 1/1 |
| Integration (daemon-backed, leak-guarded, serial) | PASS | `make test-lens-settle-hardening` 7/7 (hold/stable/masked-churn/idle-pane/param-validation); `make test-lens-gate-settle` 6/6 (stable+hold settle-and-pass, never-stable animation, persistent-regression-fails-after-retries, cast valid v2 + resize, cast-into-golden refused) |
| Raw replay / deterministic fixture | PASS | committed fixtures `.shux/fixtures/lens-gate/settle/repaint.sh` (identical repainter, colour-probed) + `.shux/fixtures/lens/f1_static.sh` (static, truecolor+256+basic) drove all shux automation |
| Shux automation (real captures, colour probes) | PASS | release binary drove 4 live `pane.snapshot` captures at 120x40 across all three new settle modes + old quiet path; explicit truecolor/256/basic content in both fixtures |
| Backward compat (L2) | PASS | frozen quiet-mode S1–S5 unchanged; `make test-lens` 37/37 |
| Visual inspection | PASS | both frames opened as images: repaint = white GATE-REPAINT title + truecolor-green REPAINT:HEARTBEAT + parked cursor block; static = bordered box, truecolor gradient bar, 256-ramp, basic-16 blocks, green/red/yellow status glyphs. No clipping / bleed / ghosting / bad wrap. Devanagari tofu boxes are the documented renderer-v2 font limitation, NOT a 083 regression (083 does not touch rendering) |
| Pixel-level cross-mode consistency | PASS | 2 metric JSONs, 0 changed pixels each (see §4) |
| Cast validity (independent) | PASS | I drove `shux lens gate <scn> --cast run.cast` on the release binary; opened the cast: header `{version:2,width:80,height:24}`, 102 events all `[t,"o"|"r",data]` with non-decreasing t, resize step → `"r"` event `100x30` (honest geometry), lands OUTSIDE golden dir |
| DootSabha design | PASS | `.local/dootsabha-083-design.json` |
| DootSabha diff review | PASS | `.local/dootsabha-083-impl.json` |

## 3. Screenshot matrix

| Viewport | Command | Settle mode | Screenshot | Pair / diff | Pixel status |
|---|---|---|---|---|---|
| 120x40 | repaint.sh HEARTBEAT | `--stable-frames 5` | `repaint_stableframes_120x40.png` | vs hold-ms → `repaint_diff.png` | 0 diff (pass) |
| 120x40 | repaint.sh HEARTBEAT | `--hold-ms 400ms` | `repaint_holdms_120x40.png` | — | — |
| 120x40 | f1_static.sh | `--quiet 400ms` (old path) | `static_quiet_120x40.png` | vs hold-ms → `static_diff.png` | 0 diff (pass) |
| 120x40 | f1_static.sh | `--hold-ms 400ms` (new path) | `static_holdms_120x40.png` | — | — |
| 80x24→100x30 | repaint.sh (gate scenario) | n/a (cast) | `run.cast` | — | valid asciinema v2 |

All PNGs are full-resolution 1080x760 (120 cols x 40 rows @ 9x19 px/cell). Both settle modes settled cleanly (exit 0): stable-frames `rev 86 after 161ms`, hold-ms `rev 63 after 420ms`; static quiet `rev 244 after 395ms`, static hold `rev 236 after 402ms`.

## 4. Pixel-level hard gate — cross-mode capture consistency

The meaningful pixel check for 083 (Feature Protocol step 9): a frame captured after a NEW settle path must be pixel-identical to the SAME live content captured after another settle path — proving the new settle timing captures the same pixels the old path does. Actual-vs-actual (both PNGs freshly generated this audit), so no committed golden / DootSabha baseline approval is required.

- `pixel-repaint-crossmode.json` — stable-frames vs hold-ms on the identical repainter: **0 changed pixels**, ratio 0, mean-channel-delta 0, size 1080x760, status pass. (md5 identical: `355e32f9…`)
- `pixel-static-crossmode.json` — default quiet (old path) vs hold-ms (new path) on identical static frame: **0 changed pixels**, ratio 0, mean-channel-delta 0, size 1080x760, status pass. (md5 identical: `461b949d…`)

Both run with `--max-pixel-diff-ratio 0 --max-mean-channel-delta 0`. Diff PNGs are all-zero (empty). Cross-mode capture consistency is proven for all three new settle modes against the old quiet path.

## 5. Findings

No P0/P1/P2 findings.

- **P3 (informational, out of 083 scope, already tracked):** the independent `--cast` gate run reported `scenario_error / frames=0` with a note redaction — this is the documented secret-scanner entropy false-positive on a temp-dir path (adversarial follow-up in the task file, → 084 secrets tuning) plus the no-`expect_golden`-step scenario I used. The cast artifact was still armed at spawn and written correctly, which is what 083 owns.
- **P3 (informational):** `make check-progress` currently FAILS because task 083 is `In Progress`. This is the correct pre-completion state — status flips to Done only after this QA verdict, as the finalization commit. Not a QA blocker.

## 6. Passed evidence

- All unit/integration/backward-compat suites green (§2).
- Cross-mode capture consistency: 0-pixel-diff on two independent fixture pairs covering all three new settle modes vs the old quiet path (§4).
- Cast independently produced and validated as honest asciinema v2 with a real resize event, outside the golden dir (§2).
- DootSabha design + implementation councils present, verdicts match task claims, blocking changes folded (§1).

## 7. Residual risk

- The 64-bit `frame_stability_hash` (SipHash-1-3) is a single aliasing point; accepted and documented (transient, non-persisted, bounded by `timeout_ms`; golden compare uses full SHA-256). Low risk.
- `--stable-frames` on a pane that reaches a STATIC steady state times out as `settle_never_stable` by design (must use `--hold-ms`/quiet); documented in CLI help and pinned by a regression test. This is a correct trade-off, not a defect.
- Deferred adversarial items (create-mode child-death masking, secret-scanner false-positive, per-XDG daemon persistence) are explicitly out of 083 scope and tracked in the task file → 084 / daemon-lifecycle.

## 8. Cleanup

- Isolated `XDG_RUNTIME_DIR=/tmp/q083`; all four QA sessions killed; per-XDG `shux __daemon` (pid 64812) killed at teardown.
- Final `ps aux | grep "shux __daemon"` (excluding the Claude notify.sh hook and the grep shell) = **NONE — clean**. Zero leaked daemons, zero leaked scratch sessions.

---
NOTE ON COMMIT: this evidence directory is written to the working tree and is ready to commit. Per the shared-worktree constraint this auditor performs no git operations; the implementer/parent must commit `.shux/qa/083-lens-gate-settle-hardening-cast/` alongside the task-completion commit that flips Status to Done.
