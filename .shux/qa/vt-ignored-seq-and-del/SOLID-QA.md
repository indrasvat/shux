VERDICT: PASS

# SOLID VT QA — vt-ignored-seq-and-del (#126, #127)

- Branch `claude/project-thread-hm3b0n`, HEAD `23ffecf`, base `origin/main` `9037784`, 3 commits.
- Scope: `crates/shux-vt/src/parser.rs` (ignore flag on CSI/ESC/DCS, width-less scalars,
  split-read C1, OSC 8 URI with a control), `crates/shux-raster/src/lib.rs` (combining
  mark pen), re-promoted golden `synthetic-grapheme-storage-current`, new tests, evidence script.
- Issues #126 and #127 read via the GitHub MCP. Neither states a Testing Matrix or DoD;
  the checklist below is their stated asks plus what the diff touches.
- Auditor: independent gate run; every artifact below was regenerated in this audit.
  Logs: `.shux/out/vt-ignored-seq-and-del/qa/` (scratch).

## Acceptance checklist

| # | Criterion (source) | Evidence (this audit) | Result |
|---|---|---|---|
| 1 | Overflowed CSI discarded, not run truncated (#126) | `ignored_sequences.rs` REP/ED/DECSTBM/SGR past-limit tests green at HEAD, red on base src (`QQQQRRR` vs `QRRR`) | PASS |
| 2 | Both directions: at-limit still works (#126) | `rep_at_the_parameter_limit_repeats`, `dcs_at_the_parameter_limit_is_answered` green at HEAD | PASS |
| 3 | Intermediate limit (#126) | `decrqm_with_a_third_intermediate_is_not_answered` red on base, green at HEAD; base oracle shot shows the DECRQM reply `^[[?2026;2$y` echoed | PASS |
| 4 | ESC + DCS handlers honour flag (#126) | `esc_dispatch_flagged_ignore_is_discarded` (red base: `'E'` vs `' '`), `dcs_past_the_parameter_limit_is_discarded`, `a_discarded_dcs_does_not_poison_the_next_one` | PASS |
| 5 | Discarded sequence still ends a grapheme cluster | `a_discarded_{csi,esc}_ends_the_cluster_like_an_unrecognised_one` green | PASS |
| 6 | DEL: no cell, no cursor move (#127) | `del_occupies_no_cell_and_moves_no_cursor` red base `A\u{7f}B`, green HEAD | PASS |
| 7 | REP after DEL repeats the glyph (#127) | `del_is_never_what_rep_repeats` red base `A\u{7f}\u{7f}\u{7f}` | PASS |
| 8 | Capture text + JSON without DEL (#127) | `del_never_reaches_capture_text`; e2e `a_real_pane_writing_del_captures_without_it` (real daemon+PTY, both formats) red on base: capture held raw U+007F | PASS |
| 9 | DEL at margin neither wraps nor cancels wrap | `del_at_the_right_margin_...` + e2e rows 5/6 | PASS |
| 10 | Split-read UTF-8 C1 == whole-read | `c1_controls_render_the_same_however_the_read_was_split`, `..._they_are_chunked`; live probe: base `A\u{85}B`, HEAD `AB` both ways | PASS |
| 11 | OSC 8 URI with a control dropped | `a_hyperlink_carrying_a_control_character_is_dropped` red base (stored `http://a\x7fb`) | PASS |
| 12 | Raster: combining mark on its own base | `combining_mark_lands_on_its_own_base_cell` red base "the mark spilled into the previous cell", green HEAD | PASS |
| 13 | Re-promoted golden is correct | old vs new opened at 4x: old drew U+0301 over `f`/`m`, new over `e`; 23 px differ, rows y=3..4 only | PASS |
| 14 | No regressions in touched crates | `make test-focus PKG=shux-vt` 707/707, `PKG=shux-raster` 30/30; `make lint` clean; `make shellcheck` 118 clean | PASS |

Red runs: `origin/main` source extracted with `git archive` into scratch, HEAD's test files
and test-only hunks applied, own `CARGO_TARGET_DIR`. vt: 20/40 failed (all new-behaviour
tests; each message names the defect), raster 0/1, e2e 0/2. Logs `qa/base-focus-*.log`.

## Testing matrix

| Layer | Evidence | Result |
|---|---|---|
| Unit | parser `esc_dispatch_flagged_ignore_is_discarded`; raster `combining_mark_...` | PASS |
| Integration | `ignored_sequences.rs` (14), `del.rs` (8), `graphics_apc_neutrality.rs` (17) | PASS (40/40 focused) |
| Raw replay | `make test-vt-corpus`: 19/19 cases 0 px; rich-tui btop, lazygit, nvim, vicaya, vivecaka each `changed_pixels=0`, thresholds 0/0 | PASS |
| Shux automation | e2e `vt_discard_pane_e2e` 2/2 (colour probes asserted on cells); oracle script under `no_leak_guard`; live byte probes (split C1, IRM+DEL, overflowed `?2026h`, OSC 8+DEL) at HEAD vs base | PASS |
| Live rich TUI | vim 9 (only one installed; nvim/htop/btop/lazygit/vicaya/vivecaka absent — corpus replays cover them) in a real pane, 80x24/120x40/200x60, file with DEL, precomposed vs combining é, ✓ | PASS |
| Visual inspection | opened qa-head 80x24 + 200x60 actual, qa-base 80x24 actual, vim 80x24 + 200x60, probe 120x40, golden old/new | PASS |
| Pixel | oracle (same binary, stream with vs without DEL/overflow noise; truecolor+256+basic probes) | PASS |
| Council design / diff | `dootsabha` not installed; documented fallback in `council-record.md` (parallel adversarial agents, round two approved the golden) | PASS (substitute) |

## Screenshot / pixel matrix

| Viewport | Scene | HEAD (qa-head) | Base (qa-base, EXPECT_DEFECT=1) |
|---|---|---|---|
| 80x24 | oracle 720x456 | 0 px, pass | 4405 px, defect reproduced |
| 120x40 | oracle 1080x760 | 0 px, pass | 4405 px, defect reproduced |
| 200x60 | oracle 1800x1140 | 0 px, pass | 4405 px, defect reproduced |

Committed metrics: `pixel-scene-{80x24,120x40,200x60}.json` (regenerated by this audit,
paths repo-relative). No committed baseline backs the scene metrics, so no PNG is committed.
Base binary was built by this audit from `origin/main` (not the implementer's copy).

## Findings

- P0/P1/P2: none.
- P3 (note, no change required): an OSC 8 whose URI carries a control is ignored whole, so a
  hyperlink already open stays open over the following text. Consistent with "a malformed
  OSC changes nothing"; recorded so it isn't mistaken for a close.
- P3 (scope note): a UTF-8 C1 such as NEL (U+0085) is executed as nothing on both paths
  (live probe: `AB` on one line). Unchanged from the base's whole-read path; the change only
  makes the split path agree.

## Residual risk

- A CSI over 32 values is now dropped whole; xterm may keep the leading parameters. The
  committed rich-TUI corpus peaks at 10 values. Stated in council-record.md.
- Raster mark heuristic (`2*xmin + width < 0`) applies to primary-font glyphs only; fonts that
  position marks by anchors need a shaper (declined in council-record.md).

## Cleanup

All daemon runs used isolated `mktemp` runtimes under `.shux/scripts/no_leak_guard.sh` with
pidfile-based stop (`shux_harness_stop_daemon` + `shux_harness_assert_no_daemon`). After the
audit: `ps -eo pid,args` shows no shux process; no `/tmp/sx126.*`, `/tmp/sxv.*`, `/tmp/sxp.*`
runtimes left.
