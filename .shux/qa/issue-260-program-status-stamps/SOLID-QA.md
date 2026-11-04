VERDICT: PASS

# SOLID VT QA: issue #260, per-report OSC 7501 stamps

## 1. Change under audit

- Issue #260: `pane wait-status` can wait for one named report that is newer than one the caller has already seen.
- Branch `fix/wait-status-for-one-report` at `f0c646b`. The commits are `1d722fe`, `3c05740` and `f0c646b`, on base `origin/main` = `d479e60`.
- VT-path scope (`git diff d479e60..f0c646b -- crates/shux-vt crates/shux-program-status`):
  - `VtProgramStatus::on_report` takes a fresh process-global stamp for every accepted OSC 7501 report. It passes the stamp to `StatusTable::apply(report, stamp)`.
  - `program_status_as_of()` is a new re-export.
  - `RecordView` gains `reported_generation` (serialized) and `reported_in` (`serde(skip)`).
  - The chunking proptest now normalizes stamps.
- Claim under test: stamping changes nothing the VT renders, replies or publishes.
- The RPC, CLI and docs parts of the diff are outside this gate, except where they expose the VT state above (`pane status` JSON).

The lead stated the testing matrix, and each row below is checked against it. Every artifact was produced by this audit from binaries built here: release builds of `f0c646b` and `d479e60` in separate scratch worktrees, copied out of `target/`.

## 2. Stated DoD matrix

| # | Requirement (from the gate request) | Status | Evidence |
|---|---|---|---|
| 1 | Real panes emit interleaved OSC 7501 and colour output (truecolor, indexed and basic), split across reads, and are pixel-verified against the same workload on `d479e60` | PASS | Five live workloads ran through both binaries. They were split into seeded 1..17-byte writes 2 ms apart. 11 head-vs-base metrics are committed (`pixel-ab-*.json`, all 0/0). All 19 scratch PNG pairs (pane and window, 80x24, 120x40, 200x60 and 120x36) and all 14 `pane capture` texts are byte-identical between the builds. |
| 2 | The OSC 7501 query reply is byte-identical on both builds | PASS | W2 sends 4 queries (ST, BEL, ST, BEL), some inside SGR runs and around reports. The reply bytes the pane program read on stdin have the same sha256 `6d4fa00e…` on both builds, are 318 bytes, mirror each terminator in query order, and have payload `states=idle,working,done,blocked,error:kinds=permission,question,auth`. |
| 3 | A rich TUI renders the same with status traffic around it | PASS | Two kinds of proof. (a) Committed recordings of btop, lazygit, nvim, vicaya and vivecaka, with 380/201/151/149/17 reports injected and fed in chunks with status ON, match the committed goldens at exact 0/0 (`pixel-replay-*.json`), and their text equals the golden text. (b) Live nvim and lazygit, with split 7501 traffic written to their tty from outside, are 0/0 head vs base for pane, window and pre-inject frames. htop was inspected visually. |
| 4 | Grid, cursor, colours and alt screen are unchanged by OSC 7501 traffic | PASS | Rows 1 and 3. `status_is_invisible_to_everything_else` passes: 600 cases compare frame, scrollback, cursor, title, revision and responses with status on vs off, and its pieces include `?1049h/l` and `?2026h/l`. `make test-vt-corpus` passes. |
| 5 | The table revision and the pane publication generation do NOT move on an identical report | PASS | Live W3 on head: generation 2149 → 2149 on an identical `done:id=job`, while the record's stamp moves 2146 → 2150. A changing `working` then moves the generation to 2152. Base: 899 → 899 → 900, the same shape. Live nvim: the final identical `done:id=tui` is stamped 13 while the generation stays at 12. Unit test `every_report_restamps_its_record_without_moving_the_revision` passes. A scratch proptest asserts `publication()` is unchanged across 1..5 identical repeats. |
| 6 | Chunking does not change records beyond the stamps | PASS | The committed `records_do_not_depend_on_chunking` passes. A stronger scratch proptest also passes (2000 cases, chunk sizes 1..8). It compares which states carry a stamp and each stamp's rank, and asserts `reported_generation == max(reported_in) <= as_of`. A sabotaged `stamp_in` update makes it fail ("last stamp is not the newest per-state stamp"). |
| 7 | No rich-TUI regression (vim or nvim, htop or btop, lazygit) with private rc copies | PASS | Live runs of nvim (`--clean -i NONE -n`), lazygit (`CONFIG_DIR`, `LG_CONFIG_FILE` and an empty `GIT_CONFIG_GLOBAL`, all private) and htop (`HTOPRC` private). The real `~/.config/htop/htoprc` mtime is still Oct 31. Replays of all five corpus TUIs are 0/0 against the goldens. |
| 8 | Zero leaked daemons, checked by pidfile, with a short isolated `XDG_RUNTIME_DIR` | PASS | `/tmp/q260head` and `/tmp/q260base` were used. After each run: pid from `daemon status`, then `daemon stop`, then `kill -0` fails. Final state: both report `running:false`, the runtime dirs hold only lock files, and `ps` finds no process with the per-run needle `qa260`. One daemon leaked by an aborted first attempt (pid 27293) was found the same way and stopped. That also served as a negative control for the check. |

## 3. Testing matrix

| Layer | Status | Evidence |
|---|---|---|
| Unit | PASS | `make test-focus PKG=shux-program-status`: 43/43, including `every_report_restamps_its_record_without_moving_the_revision`, `table_matches_the_reference_model` and `table_matches_the_reference_model_past_the_cap`. |
| Integration | PASS | `make test-focus PKG=shux-vt`: 734/734, including all 13 tests in `tests/program_status.rs`. This count includes the 2 scratch tests in the scratch worktree only. `make test-focus PKG=shux FILTER=program_status`: 31/31. The `pane_wait_status_id_e2e` suite: 2/2. |
| Raw byte / replay | PASS | 5 committed `rich-tui/*.raw` recordings with injected 7501/133 sequences, fed in seeded 1..32-byte chunks with status ON. Each matches its golden 0/0 on head, and head equals base 0/0. `make test-vt-corpus` passes. Fuzz results are below. |
| Shux automation | PASS | Live daemon panes driven through `session create`, `pane set-size`, `wait-for`, `pane capture`, `pane snapshot`, `window snapshot`, `pane status --format json` and `send-keys`, at 80x24, 120x40, 200x60 and 120x36. |
| Visual inspection | PASS | Opened at native resolution: w1 window 120x40 (split-SGR variant); w1c window 120x40 (clean colours); btop replay; live nvim window, lazygit pane, and htop window on head and base. See §5. |
| Pixel comparison | PASS | 16 committed metrics, all `pass` at 0/0. Negative controls fail as they must: an empty PNG gives rc=1, the wrong golden gives 738714 px, and the live-vs-harness frames differ by 712497 px and 171 px. |
| DootSabha design | PASS | `council-design.json`: agy and codex both ok, verdict ACCEPT-WITH-CHANGES. Requirement 6 says to keep report stamps out of publication equality, revision changes and graph watermarks, and to normalize stamps in the chunking test. Both were verified above. |
| DootSabha implementation diff | PASS | `council-impl.json` (on `1d722fe`) and `council-r2.json` (on `3c05740`), both with agy and codex ok. Neither found a merge-blocking defect. r2 asked for documentation of prompt-start pruning and the repoll comment, which `f0c646b` delivers (docs and comment only, no VT change). |

Fuzz (`make fuzz-program-status FUZZ_SECONDS=45`, on head): PASS. Runs with no crash: `program_status_decode` 6,953,029; `program_status_table` 1,474,262; `scanner_vs_vte` 2,393,726; and `vt_program_status_stream` 568,035. The last one asserts that a terminal with status ON is indistinguishable from one with it OFF, apart from the 7501 replies, under three-way chunking. `fuzz/artifacts/` holds no new crash.

## 4. Screenshot matrix

| Viewport | Command / app | Screenshot | Baseline | Diff / metric | Status |
|---|---|---|---|---|---|
| 120x36 | btop recording with 380 injected 7501/133, chunked | `replay-status-on-chunked-120x36-btop-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-btop-expected.png` (committed, untouched) | `pixel-replay-btop.json` 0/0, `…-btop-diff.png` | PASS |
| 120x36 | lazygit, 201 injected | `…-lazygit-actual.png` | committed golden | `pixel-replay-lazygit.json` 0/0 | PASS |
| 120x36 | nvim, 151 injected | `…-nvim-actual.png` | committed golden | `pixel-replay-nvim.json` 0/0 | PASS |
| 120x36 | vicaya, 149 injected | `…-vicaya-actual.png` | committed golden | `pixel-replay-vicaya.json` 0/0 | PASS |
| 120x36 | vivecaka, 17 injected | `…-vivecaka-actual.png` | committed golden | `pixel-replay-vivecaka.json` 0/0 | PASS |
| 80x24 / 120x40 / 200x60 | W1c: colour probes with 7501 at token boundaries, pane snapshot | scratch `live4/head-w1c_pane-snapshot_*.png` | the same stream on `d479e60` | `pixel-ab-live-w1c-colour-status-pane-*.json` 0/0 ×3 | PASS |
| 80x24 / 120x40 / 200x60 | W1: 7501 inside CSI and UTF-8 (aborts them on purpose), pane | scratch `live4/head-w1_pane-snapshot_*.png` | `d479e60` | `pixel-ab-live-w1-split-sgr-status-pane-*.json` 0/0 ×3 | PASS |
| 120x40 | W1c window snapshot (border badge and status bar) | scratch `live4/head-w1c_window-snapshot_120x40.png` | `d479e60` | `pixel-ab-live-w1c-colour-status-window-120x40.json` 0/0 | PASS |
| 80x24 | W2 query replies | scratch `live4/head-w2_pane-snapshot_80x24.png` | `d479e60` | `pixel-ab-live-w2-query-pane-80x24.json` 0/0 | PASS |
| 80x24 | W3 identical-report phases | scratch `live4/head-w3_pane-snapshot_80x24.png` | `d479e60` | `pixel-ab-live-w3-identical-report-pane-80x24.json` 0/0 | PASS |
| 120x40 | live nvim with tty-injected 7501, window | scratch `tui/head-nvim_window-snapshot_120x40.png` | `d479e60` | `pixel-ab-live-nvim-status-traffic-window-120x40.json` 0/0 | PASS |
| 120x40 | live lazygit with tty-injected 7501, window | scratch `tui/head-lazygit_window-snapshot_120x40.png` | `d479e60` | `pixel-ab-live-lazygit-status-traffic-window-120x40.json` 0/0 | PASS |
| 120x40 | live htop with tty-injected 7501 | scratch `tui/{head,base}-htop_window-snapshot_120x40.png` | none (live data) | inspected | PASS |

Scratch-only metrics are in `.shux/out/issue-260-program-status-stamps/live4/metrics/` and `tui-metrics/`. The 19 + 6 head-vs-base pairs are all 0/0. The 3 failing files there compare live panes with harness goldens; they are the negative controls described in §5.

## 5. Visual inspection notes

- No OSC 7501 or 133 text reaches any screen.
- The border badge reads `✓` for done and `⚠` for blocked. The status bar shows `1 waiting` on the live TUIs.
- W1c renders the colour probes as colour on both builds: a truecolor background gradient, 38;5 indexed `#` runs, basic red, green-on-blue, bold yellow, reverse and underline. DEC `lqqk` renders as `┌──┐`.
- In W1, reports inserted mid-CSI print the leftover parameters as literal text. That is the expected vte abort, and it is identical on base.
- nvim shows Rust syntax colours. lazygit shows its panels, the diff colours and both fixture commits. htop shows its meters and the process table.
- There are no ghost cells, no colour bleed past SGR resets, and no cursor artifacts.
- Two things look wrong but are identical on base, so they are not caused by this diff:
  - CJK `日本語` renders as tofu, because the font has no CJK coverage.
  - 👍🏽 renders as a thumb plus a separate swatch.
- Live nvim and vicaya differ from their goldens by 171 px because the live pane draws the cursor and the harness disables it. vivecaka's background differs from its golden (712497 px). All three differences are identical on base. That is why the committed baseline comparisons use the harness raster options rather than live panes.

## 6. Findings

No P0, P1 or P2 findings.

- **P3, test strength:** `crates/shux-vt/tests/program_status.rs:116-130` zeroes `reported_generation` and `reported_in` before comparing. As a result, no committed test checks that split reads leave the new per-state stamps alone. A split-dependent loss of a `done` stamp would pass it, and that stamp is exactly what `--id` waits read. The scratch test in §3 shows the property holds today. Comparing which states carry a stamp, and their ranks, would keep it covered.
- **P3, fuzz reach:** `fuzz/fuzz_targets/program_status_table.rs` applies every report with stamp `0`, so libFuzzer never exercises the stamp bookkeeping. `tests/fuzz_table.rs` does, with stamp `step`. The council noted the same point.
- **P3, outside the VT render path:** `RecordView` derives `Deserialize`, and the new `reported_generation` has no `#[serde(default)]`. No in-tree code deserializes it. An out-of-tree consumer that parses older `pane status` JSON into this type would fail.

## 7. Passed evidence (summary)

- Head and base produced byte-identical captures and PNGs, and identical query-reply bytes, on every deterministic workload.
- Five rich-TUI recordings with injected status match the committed goldens at 0/0.
- An identical report moves only its stamp. The publication generation and the revision stay put.
- The focused suites, the corpus harness and the scratch stamp-shape proptest are green. The scratch proptest was also seen to fail against a sabotaged tree.

## 8. Residual risk

- `as_of()` is a Relaxed load of a process-global counter. The fact that every report stamp for a pane sits at or below `as_of`, and every later one above it, depends on that pane's reports and `pane.status` being ordered by the same io lock. That was established by reading the code and by the councils, not by a forced interleaving in this audit.
- The live-TUI head-vs-base metrics have no committed baseline, so they stand alone. The committed baseline proof is the harness-raster replay.

## 9. Cleanup

- This audit started 2 isolated daemons (one per binary, per run). All were stopped and their pids proven gone.
- The one leak from an aborted attempt was found and stopped.
- No shux sessions are left. `ps` shows no `qa260` processes.
- The scratch worktrees `qa260-head` and `qa260-base` remain in the session scratchpad. They are detached and not shared.
