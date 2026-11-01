VERDICT: PASS

## Change under audit

- Issue: https://github.com/indrasvat/shux/issues/242; branch under audit: `fix/gate-trace-write-errors`; this worktree is detached at `0ae920e23d00595fb0201207b7e0d758bee41846`.
- Merge base: `0af07f3fe0d2521c99ac303bb5854557d3cc8d9b`. Diff read: `git diff $(git merge-base origin/main HEAD)..HEAD`, scoped to VT/PTY/fixtures; full and scoped diffs saved in `.shux/out/sync-output-deadline/round2/implementation.diff` and `scoped.diff`.
- This is round 2 of the **VT-only** gate: DEC 2026 parse/sweep split, `PtyOutputProbe`, atomic read/mark, readiness-based slave release, F3/F7 fixtures. Gate-CLI trace, output validation, and issue #242's trace-write acceptance criteria remain outside this delegated verdict.
- Live `gh issue view 242` agrees with the round-1 issue text. `gh pr list --repo indrasvat/shux --head fix/gate-trace-write-errors --state all ...` still returned `[]`; there is no discoverable PR matrix. The explicit audit assignment and round-1 matrix govern this run.
- Since the prior audited `3c6bdff`, every scoped production/fixture byte is unchanged. `ca9b0f7` is **more than a test-name edit**: 32 added/6 removed lines restore the two #115 names, absorb the late-batch assertion, and exercise swept/unswept paired terminals. `scope-verification.json` records identical production-prefix SHA-256 values and the unchanged paths. The changed tests were rerun at HEAD.
- Round-2 execution follows the requested minimum: five G1 runs, fresh release checks and one real TUI at 120x40, full deterministic corpus replay. Round 1 already exercised 80x24, 120x40, and 200x60; its report is preserved at `.shux/out/sync-output-deadline/round2/round1-SOLID-QA.md`. Its unchanged-code coverage is carried forward explicitly, rather than represented as a fresh run.
- Audit-only: no production source, test, fixture, baseline, or tracked Makefile changed. Supplemental make targets and audit drivers live only in scratch. No commit was made. The user explicitly requested staging for the guard and then unstaging; the bundle must be committed by the caller with the implementation before shipping.
- Generated 2026-11-01T21:17:28.864455+00:00.

The two round-1 evidence failures are resolved for the delegated VT scope: relevant council records are now supplied, and this scope has a manifest, six exact metrics, six full-resolution baseline-backed actuals, and their diffs. This is not a whole-branch ship approval.

## Stated DoD Matrix

| Exact requirement / subcase | Status | Independent evidence |
|---|---|---|
| G1: completed sync frames are not torn by late parsing; round-2 minimum x5 | PASS | Five `make test-focus PKG=shux FILTER='--test lens_glance'` runs; 3/3 each. Each G1 checks 50 RPC + 50 CLI glances while F3 flips concurrently. `test-summary.txt` contains all log paths. Round-1 x10 retained in its report. |
| Late-batch unit, formerly `a_late_batch_never_releases_a_window_on_time` | PASS | `make test-vt FILTER=a_sync_window_is_released_once_it_outlives_its_deadline NEXTEST_STATUS=pass`: 1/1. The assertion now lives under the restored #115 name: a late parse stays frozen; only the sweep releases it. |
| Two terminals fed in turn agree unless one is swept | PASS | `make test-vt FILTER=the_sync_deadline_is_what_two_terminals_fed_in_turn_can_disagree_on NEXTEST_STATUS=pass`: 1/1; both swept and unswept cases exercised. |
| Probe tracks queued and read-but-unhandled output | PASS | Focused `the_output_probe_tracks_output_until_it_is_handled`: 1/1; broad PTY 77/77. `test-summary.txt`. |
| Real silent pane opens `?2026h`, partial content visible within about 1.2 s | PASS | Release binary: 0.789768 s, `live-results.json`; `.shux/out/sync-output-deadline/round2/silent-120x40.txt` and corresponding opened PNG. |
| Pane killed mid-window is released | PASS | Separate SIGTERM child: 0.826804 s, `live-results.json`; killed capture/PNG in scratch. |
| Streaming unclosed window releases by sweep | PASS | 20 ms paced output: 0.851327 s, visible tick 35 in PNG (capture tick 34); no closer, far below cap. Captures are separate observations of an updating pane, not a same-frame assertion. |
| >2 MiB is released by cap | PASS | 2,162,688 bytes, visible in 0.029371 s, below the 150 ms deadline; `CAP-DRAINED` in capture and PNG. `artifact-verification.json`. |
| Rich TUIs render correctly; raw real-TUI replay mandatory | PASS | Fresh nvim `--clean -u NONE -i NONE` at 120x40; committed btop/lazygit/nvim/vicaya/vivecaka raw fixtures replayed at HEAD and matched tracked goldens exactly. Fixture sizes/hashes independently verified. All images opened individually. Round-1 live nvim/lazygit/htop at all three sizes retained on unchanged code. |
| Truecolor + indexed + basic-color probes | PASS | Every fresh live launch includes all three. Dedicated recovery screenshots contain exact RGB pixel counts 43/39/11 for (120,220,180)/(255,135,0)/(36,114,200). `artifact-verification.json`. |
| DootSabha design council | PASS for VT scope | `dootsabha-design.json`; assessment below states the difference between the recommended design and implementation. No claim that rejected silence accounting was approved. |
| DootSabha implementation-diff council | PASS for VT scope | `dootsabha-implementation.json`: all seven rounds inspected, with explicit scope judgement below. No final whole-branch SHIP decision claimed. |
| No leaked daemon/children; leave `<worktree>r` alone | PASS | Serial daemon suites and fresh automation wrapped in `.shux/scripts/no_leak_guard.sh`; `cleanup-verification.json`, daemon PID 74602 absent, its pidfile/socket absent; follow-along PID 65858 remains alive and untouched. |
| Conforming bundle, `git add ... && make check-vt-qa`, then `git reset -q ...` | PASS | Bundle references are relative regular files. Guard execution/exit and unstaging receipt are appended below after checking the final staged files. No uncommitted evidence is represented as already committed. |

## Testing Matrix

| Layer | Status | Execution / evidence |
|---|---|---|
| Release build | PASS | `make release`; full log `$TMPDIR/shux-qa-r2-release.XXXXXX.log.q1spqvcGfK`. Fresh live commands use `target/release/shux`. |
| Focused Rust units | PASS | Late-batch 1/1, paired terminals 1/1; broad VT run includes 420 unit tests. `test-summary.txt`. |
| Integration | PASS | `make test-focus PKG=shux-vt NEXTEST_STATUS=leak`: 732/732; PTY 77/77; lens_run + lens_settle_hardening + pane_command_e2e 72/72. No leaky annotations. |
| G1 concurrency / CLI-RPC fidelity | PASS | Five serial lens_glance executions, 15/15 tests total; 500 concurrent glances in G1 across those runs. |
| Deterministic VT/raw replay | PASS | `VT_CORPUS_OUT=$PWD/.shux/out/sync-output-deadline/round2/corpus make test-vt-corpus`: 3/3 replay tests, 19/19 text/response cases and pixel comparisons; reports in scratch. `pending_pixel` in the harness text report is its intermediate state; the separate comparator JSON supplies the completed pixel result. |
| Committed real-TUI corpus | PASS | Five raw files, byte counts and SHA-256 equal `.shux/fixtures/vt-corpus/rich-tui/manifest.json`; `artifact-verification.json`. No recording refresh or baseline generation. |
| Real shux automation | PASS | `make -f Makefile -f .shux/out/sync-output-deadline/round2/audit.mk qa-r2-live`; full log `$TMPDIR/shux-qa-r2-live.XXXXXX.log.A6Xwydmqpk`; 41 successful commands in `.shux/out/sync-output-deadline/round2/live-commands.jsonl`. Private short runtime `/tmp/vq242.ixs4eynz`, private config/state. |
| `pane.capture` | PASS | Each final snapshot follows a content wait and capture; five text files in scratch, inspected. Live driver uses explicit `--timeout-ms`; nvim also waits settled after content. |
| Full-resolution PNG visual review | PASS | 19 corpus actuals + 19 corpus diffs + five fresh live actuals + negative-control diff opened individually with original resolution. No contact sheets. The six durable actuals/diffs are copies or equivalent regenerated diffs of those inspected frames. |
| Exact pixel comparison | PASS | 19 corpus comparisons and six scope-local reruns with `.claude/automations/pixel_verify.py`; changed pixels 0, mean delta 0, exact 0/0 thresholds, masks `[]`. `pixel-*.json`; each metric's actual/expected/diff paths are relative to this scope. |
| Comparator negative controls | PASS | Wrong same-size tracked baseline: exit 1, 864 changed pixels; missing actual: exit 2. `.shux/out/sync-output-deadline/round2/control-mismatch.json`, `control-missing.json`; mismatch diff visually inspected and visibly nonzero. |
| DootSabha design | PASS, scoped judgement | Supplied source review rejects idle/silence accounting and calls for checking actual quiescence before recovery. See assessment below. |
| DootSabha implementation | PASS, scoped judgement | Seven supplied records constitute implementation-review evidence, not seven approvals. No outstanding finding names the unchanged VT scope. |
| Leak guard | PASS | Every daemon-backed make suite and fresh live automation ran serially under the guard; final `qa-r2-cleanup` also passed. |
| Evidence guard | Recorded below | `make check-vt-qa` runs its negative-control self-tests and then checks HEAD + index. The final staged bundle, not scratch, is offered to it. |

### Council assessment

**The implementation-council layer is satisfied for this VT scope.** Round 1 explicitly reviewed probe lock ordering, incarnation ownership and descriptor leaks and found no concrete defect there. Rounds 2–7 concern gate output-validation safety; multiple rounds explicitly say probe/VT ownership is unchanged. Their DO NOT SHIP recommendations are real, but do not identify an unresolved VT defect. The supplied seven rounds therefore establish actual review of this implementation surface rather than an unrelated council file.

The design council is also relevant evidence, with a qualification: it recommends PTY-owner recovery after a nonblocking read returns WouldBlock; this implementation instead uses the daemon's state lock and a shared read/probe lock around a readiness poll plus read-but-unhandled flag. It implements the same queued-output protection without literally adopting that architecture. The read and marking are atomic under the probe lock; the mark clears after parsing under `PaneIoState`; the sweep holds `PaneIoState` while checking the flag/readiness and releasing. Thus a batch already read or still queued is protected. A subsequent arrival after quiescence is a different boundary, and an oversized frame still has the intentional byte-cap exception. This is independent QA's acceptance of the implemented variant, **not a fabricated council approval of its exact code**. The recovered wall-time semantics and byte bound are tested separately.

All seven implementation syntheses say DO NOT SHIP. The AGY provider was quota-limited; the JSON records the successful Codex review and synthesis rather than implying two successful providers. Round 7's output-path findings were followed by `0ae920e` and not council-reviewed again. This gate neither certifies those fixes nor changes the council's whole-branch conclusion. Their failing-first test history is recorded in the supplied fix commit messages; it was not independently rerun in this VT-only round. No discoverable PR exists here to verify the user's stated bounded-loop PR wording.

## Screenshot Matrix

Paths below are relative to the repository except bare durable filenames, which are relative to this scope. Live snapshots are 120x40 / 1080x760 px; rich corpus snapshots are 120x36 / 1080x684 px. Synthetic dimensions match each existing fixture. No new expected baseline was minted. `baseline-provenance.json` proves all six durable comparison baselines have the same tracked blob at merge base and HEAD.

| Viewport | Command/app | Screenshot | Baseline | Diff | Status |
|---|---|---|---|---|---|
| 120x40 | silent-120x40 (release) | `.shux/out/sync-output-deadline/round2/silent-120x40-actual.png` | — live workload | — | PASS, opened full resolution |
| 120x40 | killed-120x40 (release) | `.shux/out/sync-output-deadline/round2/killed-120x40-actual.png` | — live workload | — | PASS, opened full resolution |
| 120x40 | stream-120x40 (release) | `.shux/out/sync-output-deadline/round2/stream-120x40-actual.png` | — live workload | — | PASS, opened full resolution |
| 120x40 | cap-120x40 (release) | `.shux/out/sync-output-deadline/round2/cap-120x40-actual.png` | — live workload | — | PASS, opened full resolution |
| 120x40 | nvim-120x40 (release) | `.shux/out/sync-output-deadline/round2/nvim-120x40-actual.png` | — live workload | — | PASS, opened full resolution |
| 32x8 | synthetic-plain-crlf raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-plain-crlf-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-plain-crlf-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-plain-crlf-diff.png` | PASS exact 0/0; actual and diff opened |
| 32x8 | synthetic-wide-cjk raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-wide-cjk-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-wide-cjk-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-wide-cjk-diff.png` | PASS exact 0/0; actual and diff opened |
| 36x10 | synthetic-wide-cjk-ansi-dec-edit raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-wide-cjk-ansi-dec-edit-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-wide-cjk-ansi-dec-edit-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-wide-cjk-ansi-dec-edit-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-grapheme-storage-current raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-grapheme-storage-current-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-grapheme-storage-current-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-grapheme-storage-current-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-dec-special-graphics raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-dec-special-graphics-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-dec-special-graphics-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-dec-special-graphics-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-tabs-current raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-tabs-current-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-tabs-current-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-tabs-current-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-tab-stops raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-tab-stops-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-tab-stops-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-tab-stops-diff.png` | PASS exact 0/0; actual and diff opened |
| 80x24 | synthetic-origin-response raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-origin-response-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-origin-response-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-origin-response-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x10 | synthetic-origin-scroll-region raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-origin-scroll-region-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-origin-scroll-region-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-origin-scroll-region-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-osc-default-colors raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-osc-default-colors-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-osc-default-colors-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-osc-default-colors-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-alternate-screen raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-alternate-screen-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-alternate-screen-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-alternate-screen-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-scroll-region raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-scroll-region-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-scroll-region-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-scroll-region-diff.png` | PASS exact 0/0; actual and diff opened |
| 40x8 | synthetic-sync-output raw replay | `synthetic-sync-output-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-sync-output-expected.png` | `synthetic-sync-output-diff.png` | PASS exact 0/0; actual and diff opened |
| 50x10 | synthetic-resize-smoke raw replay | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-resize-smoke-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-resize-smoke-expected.png` | `.shux/out/sync-output-deadline/round2/corpus/qa/synthetic-resize-smoke-diff.png` | PASS exact 0/0; actual and diff opened |
| 120x36 | rich-tui-btop raw replay | `rich-tui-btop-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-btop-expected.png` | `rich-tui-btop-diff.png` | PASS exact 0/0; actual and diff opened |
| 120x36 | rich-tui-lazygit raw replay | `rich-tui-lazygit-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-lazygit-expected.png` | `rich-tui-lazygit-diff.png` | PASS exact 0/0; actual and diff opened |
| 120x36 | rich-tui-nvim raw replay | `rich-tui-nvim-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-nvim-expected.png` | `rich-tui-nvim-diff.png` | PASS exact 0/0; actual and diff opened |
| 120x36 | rich-tui-vicaya raw replay | `rich-tui-vicaya-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-vicaya-expected.png` | `rich-tui-vicaya-diff.png` | PASS exact 0/0; actual and diff opened |
| 120x36 | rich-tui-vivecaka raw replay | `rich-tui-vivecaka-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-vivecaka-expected.png` | `rich-tui-vivecaka-diff.png` | PASS exact 0/0; actual and diff opened |

The origin-response fixture intentionally draws no text: its assertions are terminal response bytes. Existing CJK missing-glyph/fallback rendering is unchanged from its tracked baseline; this audit does not claim expanded Unicode font coverage. The rich-TUI frames are readable, uncropped, colored, and have intact pane boundaries. Vivecaka's committed recording is its splash screen; deeper app interaction is not claimed by that replay. Fresh live nvim shows actual file contents and syntax colors.

## Findings (P0/P1/P2/P3)

No reproduced scoped runtime defect or unresolved required VT evidence gap. No P0, P1, P2 or P3 finding in the audited change. Round-1 council absence is resolved by the supplied records and scoped assessment above; round-1 bundle absence is resolved by the staged guard-checked bundle and explicit unstage handoff requested for this round. Whole-branch gate-CLI findings remain outside this verdict.

## Passed evidence

- Fresh focused tests, five G1 runs, broad VT/PTY tests, fixture/lifecycle suite, and 19-case replay at HEAD.
- Release-binary recovery for silent, killed, paced-streaming, and >2 MiB output; real installed nvim workload; capture plus opened PNG evidence.
- All three color classes remain present, verified both visually and through exact RGB pixel counts.
- Six durable full-resolution actuals compare exactly with pre-existing tracked goldens; metrics have 0/0 thresholds and no masks. Nineteen scratch cases also pass exactly.
- Comparator rejects a known mismatch and missing input. All audit sessions and the private daemon are gone.

### Test receipts and full-log paths

```text
PASS late-batch log=$TMPDIR/shux-qa-r2.XXXXXX.log.RKS2LQw9V5
     Summary [   0.008s] 1 test run: 1 passed, 419 skipped
PASS two-terminals log=$TMPDIR/shux-qa-r2.XXXXXX.log.qlOJWNyGVb
     Summary [   0.008s] 1 test run: 1 passed, 419 skipped
PASS probe log=$TMPDIR/shux-qa-r2.XXXXXX.log.NqeG7QpysE
     Summary [   0.019s] 1 test run: 1 passed, 76 skipped
PASS G1-1 log=$TMPDIR/shux-qa-r2.XXXXXX.log.W4KeMfujXI
     Summary [   2.016s] 3 tests run: 3 passed, 0 skipped
PASS G1-2 log=$TMPDIR/shux-qa-r2.XXXXXX.log.QRz6rfnTkX
     Summary [   2.001s] 3 tests run: 3 passed, 0 skipped
PASS G1-3 log=$TMPDIR/shux-qa-r2.XXXXXX.log.5w8O31CucU
     Summary [   1.990s] 3 tests run: 3 passed, 0 skipped
PASS G1-4 log=$TMPDIR/shux-qa-r2.XXXXXX.log.pdz1JhHf56
     Summary [   1.964s] 3 tests run: 3 passed, 0 skipped
PASS G1-5 log=$TMPDIR/shux-qa-r2.XXXXXX.log.KCoRFFgMWv
     Summary [   1.980s] 3 tests run: 3 passed, 0 skipped
PASS vt-integration log=$TMPDIR/shux-qa-r2.XXXXXX.log.yL5S0urbJe
     Summary [  14.325s] 732 tests run: 732 passed, 0 skipped
PASS pty-integration log=$TMPDIR/shux-qa-r2.XXXXXX.log.5zbMDfV2ab
     Summary [   0.146s] 77 tests run: 77 passed, 0 skipped
PASS F7-exit-capture log=$TMPDIR/shux-qa-r2.XXXXXX.log.8fCccqGHsp
     Summary [  10.632s] 72 tests run: 72 passed, 0 skipped
PASS corpus log=$TMPDIR/shux-qa-r2.XXXXXX.log.Vzr0MiFvWv
     Summary [   0.013s] 3 tests run: 3 passed, 0 skipped
```

## Residual risk

- Scoped VT approval does not certify issue #242's gate-CLI semantics, output alias validation, or the post-round-7 fixes. No final council SHIP decision exists for the whole branch.
- Fresh runtime evidence is macOS at one viewport, as explicitly allowed in round 2. Round-1 multi-viewport coverage is carried forward only because production and fixture bytes are unchanged. Linux readiness/HUP behavior was not independently rerun on Linux.
- The 20 ms paced stream leaves drainable gaps; a permanently backlogged stream is bounded by bytes, not by an unconditional wall-clock guarantee. Sweep scheduling can be delayed by host starvation. Probe-construction/poll errors retain the implementation's documented fallback to timed release; resource exhaustion was not fault-injected here.
- Raw recordings establish deterministic regression stability for their recorded state, not exhaustive live behavior of every app. No baseline approval is inferred from this audit; all compared goldens predate this branch.
- Files are intentionally left uncommitted and unstaged as requested. The caller must include the complete bundle in the implementation diff before shipping; running the guard on an untracked-only handoff is not itself release evidence.

## Cleanup status

PASS. All five fresh sessions were explicitly killed before `shux daemon stop`. Audit daemon PID 74602 under `/tmp/vq242.ixs4eynz` is absent; its pidfile/socket are absent. `cleanup-verification.json` records these checks. The follow-along daemon at `<worktree>r` remains PID 65858 and was never signalled or stopped. No `pkill` or machine-wide substring process kill was used. Daemon-backed suites were serial.

## Evidence guard receipt

`git add .shux/qa/sync-output-deadline && make check-vt-qa` exited **0**. Full log: `$TMPDIR/shux-qa-r2-guard.XXXXXX.log.mfWGePkT2N`. Guard self-tests and this scope's indexed artifact checks passed. `git reset -q .shux/qa/sync-output-deadline` followed; no commit was made. A final repeat after adding this receipt checks the final report content as well.
