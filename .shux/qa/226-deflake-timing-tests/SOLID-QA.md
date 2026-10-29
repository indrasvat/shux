VERDICT: PASS

# SOLID VT QA: 226-deflake-timing-tests (#226)

- Branch `claude/project-thread-spylt3`, HEAD `38977fb`, base `origin/main` `946a066` (merge-base).
- VT-triggering diff: `crates/shux-vt/src/lib.rs` only. It adds a `sync_clock_ns()` seam over `monotonic_now_ns()` for the DEC 2026 deadline, a `#[cfg(test)] mod test_clock` (a thread-local frozen clock), and rewrites the five sync-deadline unit tests to advance that clock instead of sleeping.
- Context, exempt from the guard: `crates/shux-vt/tests/{scroll_region_bounds,program_status}.rs` (CLOCK_THREAD_CPUTIME_ID), `crates/shux-vt/Cargo.toml` (nix `time` dev-dep), and `crates/shux-pty/src/handle.rs` (`#[expect]` attributes only, not `capture.rs`). Nothing changed under `shux-raster`.
- Issue #226 was read over the REST API. Its "Done when" list: no test relies on elapsed time for correctness, repeated runs under load are green, and the guard is proven. The parent's acceptance criteria are AC1 to AC4 below.
- Every artifact was regenerated in this audit. Scratch, logs and PNGs are in `.shux/out/226-deflake-timing-tests/`.
- Base arm: a `git archive` of `946a066`. Each arm got its own `CARGO_TARGET_DIR` (`target-agents/qa226-{head,base}`). Cargo stores dep-info paths relative to the package, so a shared target dir would have reported the base tree as fresh against the head artifacts.

## Stated DoD / acceptance matrix

| # | Criterion | Evidence | Result |
|---|---|---|---|
| AC1 | Release/non-test sync output is byte-identical; the seam compiles to `monotonic_now_ns` outside `cfg(test)` | `cargo rustc --release -p shux-vt --lib -- --emit=asm -C codegen-units=1` at base and head. The asm diff is 3 lines, all panic `Location` line constants for `crates/shux-vt/src/lib.rs` (794→837, 789→832, 799→842; +43 is the size of the added block). The instruction stream is identical, and the release asm has no `sync_clock`/`test_clock` symbol. Evidence: `release-asm-base-vs-head.diff` | PASS |
| AC2 | Rewritten tests fail when the deadline logic breaks | 9 sabotages were applied one at a time to `lib.rs`, run, the failure read, then reverted (sha256 `47391a46…` re-checked after each). See the mutation table. | PASS |
| AC3 | Rich-TUI and coloured-workload sync behaviour unchanged vs main | Raw replay: 19/19 corpus cases render byte-identical PNG and TXT at base and head, both match the committed goldens, and the 19 pixel metrics are 0/0. Live daemon: 6 cases, 0/0, captures identical. | PASS |
| AC4 | Zero leaked daemons | Each live arm used an isolated `XDG_RUNTIME_DIR` (`/tmp/q226{head,base}`), ran `session kill` + `daemon stop`, and was then checked with per-run-unique `ps` needles (binary path, `QA226PROBE` argv tag, vim file path). All empty; no pidfile left. | PASS |
| #226 | No VT test relies on elapsed time | The 30 sync-related lib tests run in <1 s total. 40 stress iterations × 8 tests (5 sync-deadline, byte cap, 2 CPU-time cost ratios) under `stress-ng --cpu 8` on 4 CPUs: 320/320 passed. | PASS |

### AC2 mutation table (head, `make test-vt FILTER=<6 deadline tests>`)

| Mutant | Effect | Result |
|---|---|---|
| M1 time clause disabled (sweep never expires) | 4 FAIL: `outlives_its_deadline`, `silent_pane…swept` ("an expired window must be swept"), `class_b_osc_title…` ("the window must expire"), `timed_out_window…does_bump` | caught |
| M2 stamp not cleared on `?2026l` (window not re-armed per BSU) | FAIL `the_deadline_resets_with_every_window`: "a window was force-released 150 ms into the pane's life" | caught |
| M3 byte cap ignored | FAIL `absorbs_too_much_output`: "absorbed 4194304 bytes without being released; the cap is 2097152" | caught |
| M4 both call sites read `monotonic_now_ns` | does not compile: `sync_clock_ns`/`now` dead code under `-D warnings` | caught |
| M4a deadline read bypasses the seam | 4 FAIL | caught |
| M4b stamp bypasses the seam | 5 FAIL | caught |
| M5 deadline 1 ms early | FAIL `silent_pane…`: "a window 1 ms short of its deadline must not be swept" (the old sleep test had no lower bound) | caught |
| M6 `>` instead of `>=` (late) | 4 FAIL | caught |
| M7 window restamped on every batch | all 6 pass on head, and all 6 pass on base's sleep tests too | not caught; pre-existing, see F1 |

## Testing matrix

| Layer | Evidence | Result |
|---|---|---|
| Unit | `make test-focus PKG=shux-vt`: 706/706 passed. Sync lib subset: 30/30 | PASS |
| Integration | shux-vt `tests/` (part of the 706), `make test-vt-corpus-unit` 3/3 at both arms | PASS |
| Raw byte / replay | `make test-vt-corpus` at head and at base: 19/19 vs `.shux/goldens/073-vt-corpus`. btop, lazygit and nvim streams contain `?2026h`; nvim ends inside an open window | PASS |
| Shux automation | Debug `shux` built per arm. Colour probe (truecolor bg/fg, 256-indexed, basic SGR, bold+underline) and an open, silent `?2026h` window at 80x24, 120x40 and 200x60. The window's text becomes visible only through the daemon's deadline sweep: `capture_text` reads the frozen grid, and the workload is blocked in `read`. Also raw `nvim`/`btop` replays at 120x36 and live `vim` with syntax at 120x40. `pane.capture` + `pane.snapshot` (two snapshots per case byte-identical) | PASS |
| Visual inspection | Opened: corpus nvim, btop, synthetic-sync-output; live probe 80x24 (head), probe 200x60 (base), vim 120x40, nvim replay. No clipping, bleed, tofu, ghost cells or cursor artifacts. Colours distinct; the sync text is drawn in bold truecolor orange | PASS |
| Pixel comparison | 25 `pixel_verify.py` metrics at exact 0/0, all `pass`, `changed_pixels` 0. Head run 2 vs run 1: 6/6 0/0 (self-determinism control). Negative controls: btop vs nvim gives exit 1, `fail`, 738720 px; an empty PNG gives exit 1 | PASS |
| DootSabha design | `council-record.md` (dootsabha not installed; fallback agents) | see record |
| DootSabha implementation diff | `council-record.md` | see record |

## Screenshot matrix

| Viewport | Workload | Actual (scratch) | Compared against | Metric | Status |
|---|---|---|---|---|---|
| 80x24 | colour probe + silent sync window | `live-head/probe-80x24.png` | `live-base/…` | `pixel/pixel-live-pane-probe-80x24-head-vs-base.json` | pass 0/0 |
| 120x40 | same | `live-head/probe-120x40.png` | base | `…probe-120x40…` | pass 0/0 |
| 200x60 | same | `live-head/probe-200x60.png` | base | `…probe-200x60…` | pass 0/0 |
| 120x36 | nvim raw replay | `live-head/replay-nvim-120x36.png` | base | `…replay-nvim…` | pass 0/0 |
| 120x36 | btop raw replay | `live-head/replay-btop-120x36.png` | base | `…replay-btop…` | pass 0/0 |
| 120x40 | live vim, syntax on | `live-head/vim-120x40.png` | base | `…vim-120x40…` | pass 0/0 |
| fixture sizes | 19 corpus cases | `corpus-head/rendered/*` | `corpus-base/rendered/*` (and committed goldens via `make test-vt-corpus`) | `pixel/pixel-corpus-*-head-vs-base.json` | 19× pass 0/0 |

No PNG is committed. Every committed metric compares the head arm against the base arm, and neither side is a repo-tracked baseline. Head also matched the committed `073-vt-corpus` goldens through the corpus harness. This change cannot move pixels (AC1 shows identical code), so a new baseline would prove nothing. The metric JSON stands alone, per `.shux/qa/README.md`.

## Addendum after the audit

Since `38977fb` the VT-triggering diff changed only by
`output_inside_a_window_does_not_extend_its_deadline` in `crates/shux-vt/src/lib.rs`
(083e122), which closes F1: it fails against M7 ("streamed into for 150 ms; the
deadline is 150 ms") and passes on head; shux-vt 707/707. The thread-CPU helper
moved from the two `tests/` files into `shux-testkit`, so `crates/shux-vt/Cargo.toml`
swaps its `nix` dev-dependency for `shux-testkit`. `council-record.md` is the step 1 and step 7 record.

## Findings

- **F1 (P2, pre-existing, not a regression).** No test pins the deadline of a window that keeps receiving output. If the window were restamped on every batch (M7), a TUI streaming output inside an open `?2026h` window would never time out, and only the 2 MiB byte cap would release it. The rewritten tests and base's sleep tests both pass M7. With the frozen clock this is now cheap to test: open a window, then in a loop feed a byte and advance 10 ms, and assert release at 150 ms. Recommended for this PR, since it touches exactly these tests.
- **Examined, no defect.** `test_synchronized_output_freezes_capture_while_active` (crates/shux/tests) could look like a race against the 150 ms deadline. It is not one. That test server's timeout task does not call `release_expired_sync`, and the probe emits nothing between the DSR reply and its release, so no batch can enforce the deadline.
- **Note.** A raw replay through `cat` with tty echo on shows the daemon's DA/DECRQM replies echoed by the line discipline. The workload sets `stty -echo` before replaying.

## Residual risk

- Live btop, lazygit, nvim, vicaya and vivecaka are not installed here. Rich-TUI evidence is the committed raw corpus plus live vim.
- `make check` and full-workspace lint were not run by this gate. That is the implementer's gate.

## Cleanup

All `q226-*` sessions were killed, both daemons were stopped, the runtime dirs were removed, and the `ps` needles came back empty. Every sabotage edit was reverted and `git status` is clean. The base tree and the per-arm target dirs are scratch.
