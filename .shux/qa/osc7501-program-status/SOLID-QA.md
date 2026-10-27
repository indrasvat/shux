VERDICT: PASS

# SOLID VT QA — OSC 7501 program status (scope `osc7501-program-status`), final run

## 1. Change under audit

- **Branch and commits.** Branch `feat/osc7501-program-status`, tip `398a8ab` (`398a8abb51a4a669f1ff134ade7e6a740e13a37d`), base `45d6c56`
  (merge-base with `main`). There is no PR yet. The audit ran on `qa/osc7501-program-status-gate-r3`, which is `398a8ab` plus
  this folder only.
- **Previous gate.** It ran at `cf96627` (commit `cfbe7ff` on `qa/osc7501-program-status-gate-r2`) and FAILED on one
  finding, F4 (P1). `parse_hex_color` sliced a `&str` at byte offsets, so a 7-byte OSC 4/10/11/12 `#rrggbb` with a
  multi-byte character panicked the VT. In a live pane, that stopped output for good and orphaned the pane's child past
  `session kill` + `daemon stop`.
- **Delta since then.** I read it in full. There are 3 files and +58/−1 lines:
  - `cafe577` (`crates/shux-vt/src/parser.rs`): `parse_hex_color` now requires `hex.len() == 6` **and** six ASCII hex
    bytes before it slices. It adds the test `hex_colours_with_multibyte_characters_are_rejected_not_sliced`.
  - `398a8ab` (`crates/shux-pty/src/handle.rs`): adds `impl Drop for PtyHandle`. If `try_wait()` reports the child still
    running, Drop sends SIGKILL to its process group and then polls `try_wait` for up to 200 ms. It never signals a child
    that was already reaped. It adds the test `dropping_a_live_handle_kills_and_reaps_its_child`.
- **Behaviour delta, intended and stricter.** `#+a+b+c` used to parse as `(10,11,12)`, because `from_str_radix` accepts a
  leading `+`. It is now rejected. xterm rejects it too.

**Result in one line:** F4 is fixed and the fix is proven. The 4 hostile variants and the libFuzzer artifact no longer
panic, and text after the payload renders, both in the VT and in live panes. 17 valid colour set/query cases give the same
state and replies byte for byte as the pre-fix tree. `make fuzz` at 600 s per target is clean (5.65M execs on the
target that found F4). Neutrality holds: 14001/0 checks, 5 replays byte-identical to the committed goldens, and 12 live
pixel comparisons at 0/0. No pane child survives teardown. The one new observation is P3 (F5).

## 2. Stated DoD matrix

Source: PLAN §6 P1 VT portion, §7 and §7.1. The rows are the same as the previous runs, plus F4.

| # | DoD row | Status | Evidence (scratch = `.shux/out/osc7501-program-status/`) |
|---|---|---|---|
| F4 | Multi-byte `#rrggbb` on OSC 10/11/12/4 is rejected, not panicked on, and the pane keeps rendering | **PASS** | `repro/run3.sh`: pre-fix gives 5 PANIC, fixed gives 0, and `AFTER-PAYLOAD` renders on 7/7 hostile inputs. Live `run-live-f4.sh` (`-final`): `AFTER-<n>` renders for v11/v10/v12/v4/vfz. The test fails red on `cf96627` with "end byte index 2 is not a char boundary; it is inside 'é'". |
| F4b | A pane's child does not outlive teardown after its PTY task dies | **PASS** (F5 is a P3 note) | Drop test: red on `cf96627` ("the child outlived its dropped handle"), green now. A live A/B on the old parser plus the new Drop gives 0 children alive after stop (the prior gate saw an orphan). |
| C1 | Valid OSC 10/11/12/4 set and query behaviour is unchanged | **PASS** | `repro/valid-{pre,fix}.txt` are byte-identical (17 cases, BEL and ST). Live replies `rgb:e0e0/d0d0/c0c0` BEL, `rgb:2020/3030/4040` ST, `rgb:ffff/8888/0000` BEL, then DA1, identical on both trees. Valid-colour frames are 0/0 at 3 breakpoints, and the bg pixel is exactly `(32,48,64)`. |
| F1 | Scanner is linear on OSC-dense output | PASS (carried; no scanner change since) | Arm G re-run on `398a8ab`: OSC 8 1 MiB at 15.4–16.4 ms and CSI at 28.5–29.8 ms, flat across 4K–1M chunks (`ps-neutral-run.log`). `osc_dense_output_scans_in_linear_time` is in the 682. |
| 1–2 | Scanner grammar; differential oracle vs vte | PASS | `make test-focus PKG=shux-vt` 682/682. libFuzzer `scanner_vs_vte` ran 600 s / 25.6M execs clean. |
| 3–5 | Replies mirror ST/BEL, keep stream order, and only DA1 is sent when disabled | PASS | Harness D: 7501(ST), DA1, 7501(BEL). Live ON = BEL, ST, DA1. OFF = DA1 (`live-r2/reply-*.txt`). |
| 6–9 | RIS/DECSTR, 133;A, DEC 2026 / alt screen, no `content_revision` bump | PASS | Unit tests in the 682. Harness C. |
| 10 | Neutrality: enabled == disabled except for 7501 replies | PASS | `run-neutral.sh`: **checks=14001 failures=0**. Arm C `seen=true` on 6/6 workloads. |
| 11 | Rich-TUI pass with status on | PASS | 5/5 rich-TUI replays with injected 7501 are sha256-identical to the committed goldens (§5). `make test-vt-corpus` PASS. |
| 12 | Tier-1 fuzz in `make test` | PASS | Proptests in the 682. The F4 regression test is tier-1. |
| 13 | Tier-2 libFuzzer, 0 crashes | **PASS** | `make fuzz FUZZ_SECONDS=600` exit 0, and `fuzz/artifacts/` is empty (§3). |
| 14 | Colour probes in daemon-backed captures | PASS | Truecolor `38;2`, indexed `38;5;45`, basic `31` and `30;42` appear before and after every payload in `live-f4.sh`, and on line 2 of `emit.sh`. |
| 15–16 | DootSabha design and implementation-diff council | PASS for the VT slice. P3 note F3 | `council-record.md` |

## 3. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| L0 build | PASS | `make build` (`make-build.log`). The A/B tree `trees/pre` was built with `make build` (`make-build-pre-dropfix.log`). |
| L1 unit | PASS | `make test-focus PKG=shux-vt`: 682 run, 682 passed. The new test passes by name (`test-focus-vt-new.log`). |
| L2 integration | PASS | `make test-focus PKG=shux-pty`: 72 run, 72 passed. The new Drop test passes by name (`test-focus-pty-new.log`). `make test-vt-corpus` PASS. |
| L2b seen-red | PASS | `trees/pre` = `cf96627` plus **only** the two new tests (`prep-seen-red.sh` diff shows a test-only delta). The hex test FAILs with "end byte index 2 is not a char boundary; it is inside 'é'" at `parser.rs:2393`. The Drop test FAILs with "the child outlived its dropped handle" after 2.02 s. |
| L3 raw replay / differential | PASS | 14001/0. The F4 crash input replays clean (`repro/results3-r3fix.txt`). |
| L3b libFuzzer, 600 s per target | PASS | `run-fuzz-600.sh` exited 0. `program_status_decode` 77,049,084 runs, `program_status_table` 6,785,409, `scanner_vs_vte` 25,605,483, `vt_program_status_stream` 5,653,103. The stream corpus (3178 files) was seeded with the previous run's grown corpus **plus the F4 crash input**, and INITED without a crash. The 120 s run that found F4 had needed about 1.18M execs. |
| L4 shux automation | PASS | Three scripts, all under `.shux/scripts/no_leak_guard.sh` with an isolated `XDG_RUNTIME_DIR` and exit 0: `run-live-f4.sh` (`/tmp/sq78`), its A/B on `/tmp/sq79`, and `run-live-r2.sh` (`/tmp/sq76`, on/off + hot reload). |
| L5 visual inspection | PASS | PNGs opened at native resolution (§5) |
| L6 pixel | PASS | 5 replays vs committed goldens at 0/0 (committed). 3 valid-colour frames fix-vs-pre and 9 live on/off/hot-reload frames, all 0/0 (scratch, no committed baseline). Negative controls fail as they must, with ratio 1.0. |
| L7 independent verdict | this report | |
| L8 DootSabha | PASS for VT, P3 F3 | `council-record.md` |

## 4. F4 / F4b verification detail

- **VT level (`repro/main3.rs`).** The same binary was built against `cf96627` and `398a8ab`.
  - Hostile inputs: `ESC]11;#a\xc3\xa9\xc3\xa9b BEL`, the OSC 10 form with ST, OSC 12, OSC 4 `1;#…`, the aligned control
    `#ééé`, `#+a+b+c`, and the 85-byte libFuzzer artifact (`fuzz-crash-…-3a698c60.bin`, kept here as a regression input).
  - Pre-fix: 5 PANIC. Fixed: 0 PANIC, and `after_renders=true` on all 7. Default colours stay `None` after every hostile
    input.
  - The 17 valid cases cover `#hex` in both cases, `rgb:` with 1/2/3-digit components, the palette, bad lengths and a
    non-hex digit, queries with BEL and ST, set-then-query, and a 110/111 reset. They are identical across the two trees.
- **Live (`live-f4-final/`).** Five victim panes each print colour probes, `BEFORE`, the payload, `AFTER-<n>` and the
  probes again. Every `AFTER-<n>` renders (`v3_pane-snapshot_80x24_{v11,v10,v12,v4,vfz}.png`), and each child is still
  `sleep` under the daemon until it is killed. Three valid-colour panes (80x24, 120x40, 200x60) set fg/bg/cursor and query
  them. The replies are correct and their terminators mirror the request.
  - After `session kill` ×8 and `daemon stop`: 8/8 child pids are gone, `daemon not running`, `/tmp/sq78/shux` is empty,
    and the guard exits 0.
- **Drop A/B (`live-f4-pre-final/`, `live-drop-probe-2/`).** `trees/pre` uses the `cf96627` parser, which still panics,
  with the `398a8ab` `handle.rs`.
  - Every hostile pane stops rendering, and `pane set-size` reports "pane resize channel closed", so the task is gone.
  - The children are killed at the panic: `live-drop-probe-2` shows 2/2 GONE within about 1 s.
  - After stop, 0 children are alive and the guard exits 0. On `cf96627` the previous gate found `sleep 600` orphaned
    past `daemon stop`.

## 5. Screenshot matrix

| Viewport | Command / app | Screenshot | Baseline | Diff / metric | Status |
|---|---|---|---|---|---|
| 120x36 | btop replay with 380 injected 7501/133 | `replay-status-on-injected-120x36-btop-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-btop-expected.png` (committed, untouched by branch) | `pixel-replay-btop.json` 0/0, `…-btop-diff.png` | PASS, sha256 identical |
| 120x36 | lazygit, 201 injected | `…-lazygit-actual.png` | committed golden | `pixel-replay-lazygit.json` 0/0 | PASS |
| 120x36 | nvim, 151 injected | `…-nvim-actual.png` | committed golden | `pixel-replay-nvim.json` 0/0 | PASS |
| 120x36 | vicaya, 149 injected | `…-vicaya-actual.png` | committed golden | `pixel-replay-vicaya.json` 0/0 | PASS |
| 120x36 | vivecaka, 17 injected | `…-vivecaka-actual.png` | committed golden | `pixel-replay-vivecaka.json` 0/0 | PASS |
| 80x24 / 120x40 / 200x60 | valid OSC 10/11/12 set + query, `398a8ab` | scratch `live-f4-final/v3_pane-snapshot_*_valid-colours.png` | same pane on the `cf96627` parser | `live-f4-final/metric_valid-colours_*_fix-vs-pre.json` 0/0 | PASS x3 |
| 80x24 | 5 hostile payloads, `398a8ab` | scratch `live-f4-final/v3_pane-snapshot_80x24_{v11,v10,v12,v4,vfz}.png` | none (pre-fix never renders) | inspected | PASS |
| 80x24 / 120x40 / 200x60 | `emit.sh`, pane and window snapshot, status ON vs OFF | scratch `live-r2/v2_*` | status OFF | `live-r2/metric_*` 0/0 x6 | PASS |
| 80x24 | hot reload ON → OFF → ON | scratch `live-r2/v2_pane-snapshot_80x24_hot-*` | hot-0-on and fresh ON | 0/0 x3 | PASS |

Opened at native resolution: the btop replay; live v11, v4 and vfz; valid-colours 120x40 and 200x60; window 120x40 ON.
- No 7501 or 133 text reaches the screen, and the `[]` brackets are empty. The kitty image sits inside `A8`. The colour
  probes render as truecolor orange, indexed cyan, basic red and green bg.
- The hostile frames keep the default background `(16,16,24)`. The valid frames use bg `#203040`, fg `#e0d0c0` and an
  orange cursor.
- There is no tofu, ghost cell, colour bleed or cursor artifact.
- `pane capture` ON == OFF byte for byte at all 3 sizes. The hot-reload generations go 13 → 14 → 15.

## 6. Findings

There is no P0, P1 or P2.

- **F4 (P1, previous run): RESOLVED.** See §4 and the seen-red test.
- **F5 (P3, new, on a panic-only path).** `PtyHandle::drop` kills the child, but it can miss the reap.
  - Reproduction: run `run-live-f4.sh` with `SHUX_BIN=trees/pre/target/debug/shux` (the `cf96627` parser plus the
    `398a8ab` Drop). About 7 s after the panic, the v10 child showed `STAT Z, ppid = daemon` (`live-f4-pre-final.log`).
  - The child was dead, so the SIGKILL had landed. That fits an exit that finished after Drop's 200 ms reap window. I did
    not prove the cause: the PTY master is still open while Drop runs, which may delay the exit. The zombie stays until
    the daemon reaps a later child or exits. No running process survives, and every run ended with 0 children after `daemon stop`.
  - An earlier run reported a vfz pid as alive without recording its state, so that reading may be the same thing, or a
    reused pid. It did not reproduce in two later runs.
  - Impact: zero on `398a8ab`, because no input reaches a panic on that path. The rustdoc's "and reaps it" is
    best-effort. Reaping asynchronously, or after the master is closed, would make it exact.
- **F2 (P3, carried):** the 512-reply budget is shared, so ON and OFF responses can differ once it is exhausted. This is
  the documented PLAN §5.1 decision.
- **F3 (P3, process):** no council output is recorded for `f02af0e..398a8ab`, which includes the two fix commits audited
  here. For the VT-gated commits, this gate's independent checks stand in (`council-record.md`). The PR's step-7 row
  still owes it.
- **Probe defects of my own, for honesty.** None of these is a product finding.
  - The first `live-f4` attempt waited on `BEFORE-vfz`, which the artifact's RIS wipes. It now waits on a pid file.
  - The first `live-drop-probe` read stale pid files. It now uses a fresh directory.
  - The valid-colour panes first printed their PID, which breaks a 0/0 comparison across trees. They now keep it off
    screen.

## 7. Passed evidence

- **F4 is fixed and proven.** The fix is a byte-level ASCII-hex guard. It is red before and green after, a 600 s fuzz
  campaign seeded with the crash is clean, and live panes keep rendering.
- **Valid colour behaviour is unchanged.** VT state and replies are identical. Live replies are identical, and the
  frames are 0/0 at 3 breakpoints.
- **Teardown no longer orphans.** The Drop test is red before and green after, and a live A/B on the still-panicking
  parser shows children killed at the panic and 0 left after stop.
- **Neutrality holds on the final tree.** 14001/0 checks, 5/5 committed goldens byte-identical, and 9 live
  on/off/hot-reload comparisons at 0/0.

## 8. Residual risk

- `osc_dense_output_scans_in_linear_time` is a wall-clock ratio test. It passed alongside a concurrent fuzz campaign,
  but heavy CI contention could still flake it.
- The respawn-while-off race is verified at the VT level and by reading the code. The concurrent daemon interleaving is
  TUI-gate scope.
- F5 as above.
- **Out of scope.** `make test-vt-corpus` re-encodes the 19 tracked `.shux/qa/073-shux-vt-corpus-regression-harness/*-diff.png`
  files on every run, with all metrics still passing. This also happens on `main`. They are left unstaged and are not part
  of this commit.

## 9. Cleanup

- `run-live-f4.sh` (`/tmp/sq78`), its A/B runs (`/tmp/sq79`), `live-drop-probe.sh` (`/tmp/sq80`) and `run-live-r2.sh`
  (`/tmp/sq76`) all ran under `no_leak_guard.sh` and exited 0. Each stops its daemon in the script body and again in an
  EXIT trap.
- Afterwards, `daemon status` reports `daemon not running`, every `/tmp/sq7x/shux/` is empty, and `ps` shows no process
  from this worktree and no `sleep 30` or `sleep 600`.
- One deliberate exception: the seen-red Drop test on the unfixed tree left its own `sleep 30` (ppid 1). That leak is the
  defect the test exists to catch, and the process expired by itself.
- No product source was edited. `trees/pre` is a `git archive` copy under scratch.
