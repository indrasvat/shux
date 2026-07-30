VERDICT: PASS

# SOLID VT QA — Task 086 Mouse Wheel Scroll

- Active task: docs/tasks/086-mouse-wheel-scroll.md
- Branch: claude/shux-mouse-scrolling-eobxoh
- Commit under audit: f582f08 (tip; feat a9e3796, tests ec590c8, fix f582f08)
- Binary: target/debug/shux (prebuilt; NOT rebuilt per instruction)
- Runtime dir: /tmp/shux-vtqa (isolated)

## Scope
SOLID VT gate applies because parser.rs gained DEC private mode ?1007
(alternate_scroll, default on) + DECRQM reporting, and attach.rs gained wheel
dispatch that reads live VT state. Gate focuses on: (1) ?1007 tracking + DECRQM,
(2) no VT/raster/snapshot regression, (3) live VT-state reads, (4) cleanup.

## Task DoD / Acceptance Criteria Matrix
| Criterion | Status | Evidence |
|---|---|---|
| ?1007 tracked; alternate_scroll defaults on | PASS | parser.rs:39,62,753; unit test tests::test_alternate_scroll_mode (ran, ok) |
| DECRQM \x1b[?1007$p reports it | PASS | parser.rs DECRQM handler 1360-1363 -> report_mode(1007,true) -> private_mode_report_value 1007 (parser.rs:896) -> mode_report 1/2; shared path asserted for siblings in process_with_responses_answers_decrqm_for_modes |
| Wheel scrolls scrollback on primary screen | PASS | integration test handle_wheel_enters_scrollback_on_primary_screen (ok) |
| Wheel reaches mouse-aware apps (SGR/X10) | PASS | handle_wheel_forwards_encoded_report_to_mouse_aware_app + encode_mouse_wheel_sgr/x10 (ok) |
| Wheel -> arrows on alt-screen non-mouse | PASS | handle_wheel_translates_to_arrows_on_alt_screen_without_mouse + wheel_arrow_seq_honors_application_cursor_keys (ok) |
| Copy-mode wheel behavior unchanged | PASS | manual_copy_mode_survives_wheel_back_to_bottom + wheel_initiated_scrollback_exits_when_wheeled_back_to_bottom (ok) |
| Live VT state reads (mouse_tracking/sgr/alt/alt_scroll/app_cursor) | PASS | attach.rs handle_wheel snapshots io_state.vts.get(pane_id).modes() per event; integration tests seed ?1000h/?1006h/?1049h into the live VT and assert routing |
| No VT/raster/snapshot regression | PASS | htop/vim/less/probe render correctly; two-glance static pane byte-identical (0-diff) |
| new unit + regression tests | PASS | 4 unit + 5 behavioral (red->green) tests, all green |

## Testing Matrix (layers)
| Layer | Status | Evidence |
|---|---|---|
| Unit (shux-vt) | PASS | cargo test -p shux-vt: 365 passed; test_alternate_scroll_mode ok |
| Unit (encoder/router) | PASS | cargo test -p shux --bins wheel: 9 passed (route/encode/arrow) |
| Integration (handle_wheel vs live graph+VT) | PASS | 5 wheel behavioral tests green |
| Raw replay / DECRQM responses | PASS | vt_corpus_replay 3/3; process_with_responses_answers_decrqm_for_modes |
| Shux automation (real panes) | PASS | htop, vim, less, colored probe via daemon at 80x24 / 120x40 |
| Visual inspection | PASS | opened all 4 PNGs full-res; no tofu/bleed/clip/corruption |
| Pixel verification | PASS | pixel_verify.py exact: changed_pixels=0, ratio=0.0, mean delta=0.0 |
| DootSabha | N/A-substituted | task file DoD explicitly N/A (not installed); substituted by adversarial-review (4 breakers, documented in task) |

## Screenshot Matrix
| Viewport | App | Screenshot | Pixel metric | Diff | Status |
|---|---|---|---|---|---|
| 80x24 | colored probe glance1 | probe-glance1-80x24.png | pixel-probe-twoglance.json | probe-glance-diff.png | PASS (colors correct) |
| 80x24 | colored probe glance2 | probe-glance2-80x24.png | (byte-identical, md5 match) | - | PASS |
| 120x40 | htop | htop-120x40.png | visual | - | PASS |
| 80x24 | vim | vim-80x24.png | visual | - | PASS |
| 80x24 | less -R | less-80x24.png | visual | - | PASS |

Color probes: truecolor (fg 255;0;0 / bg 0;128;255), indexed-256 (green 46),
basic (blue). All rendered correctly — no monochrome/NO_COLOR regression.

## Findings
- P3: DECRQM \x1b[?1007$p has no dedicated assertion in
  process_with_responses_answers_decrqm_for_modes (query list omits 1007). The
  report is code-verified and structurally identical to 15 sibling private modes
  that ARE asserted; test_alternate_scroll_mode covers set/reset/default. Not a
  DoD failure (AC only requires "?1007 tracked; defaults on"). Recommend adding
  1007 to that query list for completeness.
- P3: No cross-version pixel baseline (old binary cannot be rebuilt per
  instruction). Mitigated: the diff touches only a VT bool field (default true,
  no render path) + attach wheel dispatch (no rasterizer path); 4 real workloads
  render correctly and two-glance is byte-identical.

## Passed Evidence
- Frozen: cargo test -p shux-vt (365+3+1 ok); cargo test -p shux --bins wheel (9 ok).
- Two-glance static pane byte-identical: md5 ce1483862cbc6eb23325de0a8b042a58 (both).
- pixel_verify.py exact-equality pass (0 changed pixels of 328320).
- Real TUIs render un-regressed at 80x24 and 120x40.

## Residual Risk
Low. Wheel end-to-end via a live attach mouse client was not driven from the CLI
(no CLI mouse-frame injection); covered instead by integration tests that drive
handle_wheel/handle_copy_mode_mouse against a real graph + live VT. Full live
attach + adversarial drive is documented in the task's adversarial-review section
and is the tui-qa/dogfood gate's domain.

## Cleanup Status
Zero leaked daemons from this runtime dir. My daemon pid 13957 stopped; pidfile
removed. Sessions vtqa-probe/htop/vim/less killed. One remaining shux process
(pid 27664) belongs to XDG_RUNTIME_DIR=/tmp/shux-agent2 (another agent) — left
untouched per instruction.
