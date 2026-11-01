VERDICT: PASS

# SOLID VT QA: kitty-put (issue #234, part 1)

## 1. Change under audit

- Branch `fix/kitty-put` at `c2a5c9c`, base `origin/main` `96d6cd3`. Worktree the branch worktree.
- This is a re-audit. The first pass, at `5e9ebe3`, returned FAIL on P1-1; section 5 records how that was resolved.
- Since `5e9ebe3`, the only change is `c2a5c9c`. It adds a `#[doc(hidden)]` `VirtualTerminal::set_sync_deadline` (default `true`) and calls it from the two differential tests. Grep confirms no production caller.
- Binaries:
  - New: `make release` at `c2a5c9c` (CARGO_TARGET_DIR=/tmp/shux234bin/tgt-b, cleaned by the lead first), copied to `/tmp/shux234bin/vtqa/shux-new`.
  - Base: `/tmp/shux234bin/vtqa/shux-base`.
- Scratch:
  - `.shux/out/kitty-put/`: harness in `bin/`; arms in `ab/<arm>/<scene>-<WxH>/`; metrics in `metrics/`; diffs in `diff/`.
  - The 5e9ebe3 run is archived as `ab-5e9ebe3/` and `metrics-5e9ebe3/`.

## 2. Stated DoD matrix

| # | Acceptance criterion | Status | Evidence |
|---|---|---|---|
| 1 | No regression for `a=T` pictures | PASS (re-scoped clause) | Pane, window and session snapshots are 0 px against base, for icat, aT and nonmultT at 80x24, 120x40 and 200x60. Attach matches the pane exactly for cell-multiple pictures. The kitty commands the emitter writes for every `a=T` picture are identical to base, non-cell-multiple ones included (`emitter-bytes-ab.json`). A non-cell-multiple picture is fitted to its ceil cell footprint and keeps its origin (`fit-new-*.json`). |
| 2 | Puts agree across paths and match the expected geometry; `img-over`/`img-last` fixed | PASS (re-scoped clause) | Transmit+put, crop, move (re-put of the same i,p), additive p=0 and delete match base-rendered `a=T` references, 0 px on every path (`pixel-ref-*`). Scale geometry is exact at 3 breakpoints: c+r letterbox 54x57 top-left, c-only 36x38, r-only 108x114, crop+c 72x76. Attach equals the pane for every cell-multiple scene (`pixel-xp-new-*`). `img-over`/`img-last`: position correct at all 3 breakpoints, size is the fit 72x54 (`img-over-last-position.jsonl`). Base was at the wrong row at 80x24 and 120x40. |
| 3 | Rich TUIs 0 px against base | PASS | vim -u NONE, nvim --clean, less -R, lazygit, and htop (private HTOPRC, deterministic config) at 80x24 and 120x40, each beside a colour-probe pane. Replays of btop, lazygit, nvim, vicaya and vivecaka at 120x40 and 200x60. 90/90 comparisons at 0 px. |
| 4 | `?2026` and the alternate screen keep their semantics with puts and deletes | PASS | `sync` and `alt-1` (2 phases) equal their references on every path, at 3 breakpoints. 9/9 VT probes rerun at HEAD (`.shux/out/kitty-put/zz_qa_kitty_put_probe.rs`): a put, a move, a delete, a re-transmit drop and `d=I` are each held by a sync window; alt store separate and emptied; alt entry inside sync; `d=a` keeps data and `d=A` frees it; RIS clears both stores. |
| 5 | Zero leaked daemons and processes | PASS (product) | Every daemon the change starts stops on `daemon stop`. Final check: 0 `vtqa/shux-*` processes, 0 live pidfiles, `/tmp/kq*` removed, real htoprc untouched (mtime 11:43). The leaks the harness itself caused, and one pre-existing out-of-scope defect, are recorded in section 9. |

### Re-scope accepted for AC1 and AC2, attach size

The re-scoped clause, which must appear in the PR description:

> "Live attach matches the pane snapshot exactly for cell-multiple pictures; others are fitted to their ceil cell footprint (at most one cell larger per side), as real kitty has drawn them since before this PR; positions match everywhere."

I re-derived the reasoning rather than taking it on trust:

- **Real hosts see the same bytes as before.** The raw PTY bytes a client writes on a shux host (`pane.record`) contain the same kitty control keys at base and HEAD for every `a=T` scene, including 64x48 (`c=8,r=3`), the 32x32 icat PNG (`c=4,r=2`) and 40x30 (`c=5,r=2`) (`emitter-bytes-ab.json`). So on a real kitty, ghostty or wezterm host this branch changes nothing. The enlargement is base behaviour, pinned by base's `a_picture_that_is_not_a_whole_number_of_cells_still_claims_the_cells_it_owns`.
- **Only a shux host changes.** It now honours `c,r` per the spec instead of ignoring them. Before, it also ignored `y/w/h` and `a=p`, which is why base misplaced `img-over`/`img-last`, dropped clipped tall pictures (icat 80x24), and lost the primary screen's picture after the alternate screen. All three are fixed here; in each, attach equals the pane.
- **Without `c,r`, pictures spill.** A host draws the source at its own pixel size, so on hosts with cells smaller than 9x19 the picture runs past the pane border. That is the defect class the GUI rig's containment check exists for.
- **Measured fit:** `fit-new-*.json`.
  - Every non-cell-multiple picture keeps its origin and is exactly the aspect-kept fit into its ceil footprint: 64x48 becomes 72x54, 40x30 becomes 45x33, and 32x32 becomes 36x36.
  - Scaled puts are exact.
  - The check fails on mismatched scenes and on empty input.

One counter-case, recorded but not blocking, because it changes nothing this PR touches:

- **Exact size is possible without spill.** The protocol can draw a non-multiple bitmap at its exact size inside whole cells: the emitter would pad the decoded bitmap with transparent pixels to the footprint and re-encode it. The host then draws it like the pane raster does, at any host cell size.
- **It costs a decode and re-encode** per transmit, and it breaks the emitter's "payloads leave as they arrived" invariant.
- **That makes it a follow-up**, not a condition of this PR.

Also: kitty 0.32 stretches to the full footprint (72x57), while a shux host letterboxes (72x54). Both stay within the clause's one-cell bound.

## 3. Testing matrix

| Layer | Status | Evidence |
|---|---|---|
| Unit + integration | PASS | `make check` at c2a5c9c, exit 0. nextest: 2739 run, 2739 passed, 2 skipped. Log: `/tmp/shux234bin/vtqa/make-check2.log` |
| VT probes | PASS | 9/9 at HEAD. Log: `/tmp/shux234bin/vtqa/probe-test2.log`. Scratch file removed from the tree after the run. |
| Raw replay | PASS | `.shux/fixtures/vt-corpus/rich-tui/*.raw` × 2 breakpoints × 4 paths, 0 px against base |
| Raw emitter bytes | PASS | `pane.record` of the host pane: kitty control keys identical base vs HEAD for aT ×3, icat ×2 and nonmultT ×2. icat-80x24 differs only by one intermediate frame; final state attach == pane. |
| Shux automation | PASS | 121 arms at HEAD (`batch4.log`, plus one rerun). Nested attach (`SHUX_GRAPHICS=1`) inside an outer daemon running the same binary. Every scene carries a truecolor, 256-colour and basic-colour probe. |
| Visual inspection | done | First pass: put, scale, del, alt (both phases), nonmult attach and pane, nvim, vim, less, lazygit, htop, btop. Re-opened at HEAD: nonmultT-120x40 attach (fit visible, origins kept) and put-200x60 window (crop, move, additive p=0). Numeric geometry was re-measured at HEAD for scale at 3 breakpoints, img-over/img-last and nonmult. |
| Pixel comparison | PASS | 414 pixel_verify runs at HEAD (A/A and the alt new-reference checks included), every one at exact 0/0 thresholds (`comparisons-summary.txt`). Every non-pass is classified there: an expected A/B difference (base has no `a=p`); a base attach defect, resolved by base attach != base pane while new attach == new pane; the alt phase-2 reference, re-checked against new's own reference (0 px); or a re-scoped fit (`PASS fit`). 133 passing metrics are committed here. |
| A/A determinism | PASS | new vs new2 on aT, tui-htop, img-over and img-last: 0 px |
| DootSabha design | PASS | `council-design.md` |
| DootSabha impl diff | PASS (with caveat) | `council-implementation.md`: 3 rounds covering 96d6cd3..c2a5c9c; round 3 returned AGREE. agy failed on quota in rounds 2 and 3, so those rounds rest on codex alone. |
| GUI terminal (kitty/Xvfb) | not run | `docker run --rm shux234-gui true` still fails with a containerd blob input/output error (sha256:af7bef07…). Last pass was at 8bb054f. The emitter is unchanged since 0446e8d, and the emitter bytes match base. The lead has offered to rerun it. |

## 4. Screenshot matrix (scratch only, none committed)

Paths: `.shux/out/kitty-put/ab/<base|new|new2>/<scene>-<WxH>/{pane1,pane2,window,session,attach,attachbody,attach-pane1,ph2-*}.png`

| Scene | Breakpoints | Base vs new | New vs reference | Attach vs pane (new) |
|---|---|---|---|---|
| icat | 80/120/200 | 0 px. At 80x24 attach differs; that is a base defect (base attach != base pane). | n/a | PASS |
| aT | 80/120/200 | 0 px | n/a | PASS |
| nonmultT, nonmult | 80/120 | snapshots 0 px (nonmultT); attach differs | n/a | fit PASS (re-scoped) |
| put / del / sync / alt-1 | 80/120/200 | differ (base has no `a=p`) | 0 px | PASS |
| scale | 80/120/200 | differ (base has no `a=p`) | geometry exact | PASS |
| img-over / img-last | 80/120/200 | snapshots 0 px | position correct, fit size | fit PASS |
| tui-* | 80/120 | 0 px | n/a | n/a |
| replay-* | 120/200 | 0 px | n/a | PASS |

## 5. Findings

- **P1-1 (first pass): resolved by an accepted re-scope** (section 2). It is not a regression on real hosts: the bytes are identical to base. A shux host now honours `c,r` and stays within one cell.
- **P2-1 (first pass): resolved.** The council's round 3 covers 8bb054f..c2a5c9c and returned AGREE (codex; agy hit its quota).
- **P3-1: open.** No `attach_image_emit` test uses a picture that is not a whole number of cells, so the re-scoped one-cell bound is not pinned by a test. Recommended: emit a 64x48 `a=T`, feed the bytes to an outer `VirtualTerminal`, and assert same origin and `drawn_px` = the fit.
- **P3-2: out of scope, pre-existing.** `shux pane record` keeps running after its daemon is stopped. Reproduced on base: the recorder was still alive 6 s after `daemon stop`. Not in this diff.

## 6. Passed evidence

- Pixel metrics: the `pixel-*.json` files in this directory, all `pass` at 0/0.
- Supporting files:
  - `comparisons-summary.txt`
  - `emitter-bytes-ab.json`
  - `fit-new-*.json`
  - `img-over-last-position.jsonl`
  - `council-design.md`
  - `council-implementation.md`

## 7. Residual risk

- The GUI rig was not run at HEAD (Docker store broken on this machine).
- Live htop is held deterministic only by its config (no meters, one Command column, one pid).
- `window`/`session` snapshots carry an `up Ns` cell. They compared at 0 px both A/B and A/A, but that comparison is timing-sensitive.

## 8. Shux sessions

All inner and host sessions were killed and their daemons stopped by arm teardown or, for the one timed-out arm, by hand. See section 9.

## 9. Harness note (audit's own leaks, cleaned)

At HEAD, the recording arm stopped the background `pane record` by signalling its wrapper subshell, not the recorder. That orphaned 18 recorders. One arm that hung on the first version (SIGINT is ignored in background jobs) also left its two daemons behind.

All of them were stopped by pid or by `daemon stop` on their own runtime dir. They are artifacts of the audit harness, not of the change.
