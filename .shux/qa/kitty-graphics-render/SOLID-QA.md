VERDICT: PASS

# SOLID VT QA — kitty-graphics-render (re-audit)

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| Commit audited | `4238f6b` — *fix(vt): count a placement as a write, and delete through the freeze* |
| Previously audited | `8d4cc8f` — **VERDICT: FAIL** (P1-1, P1-2, P1-3, P2-1, P2-2, P2-3, four P3s) |
| Base | `de1fadb` |
| Design | `docs/designs/inline-images.md`, work item 4 |
| Scratch | `.shux/out/kitty-graphics-render/r2/` (gitignored) |

This is a re-audit. Every fix below was re-measured from scratch on `4238f6b`;
nothing is carried over from the previous run's numbers. Where the previous run
found a defect, the fix was A/B'd against a tree with that one hunk reverted, so
the probe is shown able to fail before it is trusted to pass.

**Every pixel in this report comes from `shux-raster`.** CLAUDE.md is explicit
that shux cannot audit its own emit path by rendering it itself. This change
emits nothing new to an outer terminal — attach draws no images (work item 6) —
so there is no new emit path to photograph, but the evidence carries that caveat
and `make test-gui-terminal` was not run.

## Verdict on each previously-reported finding

| # | Finding at `8d4cc8f` | Now | Independent evidence |
|---|---|---|---|
| P1-1 | placement survives alt-screen recycling into the next application | **FIXED** | library `recycled=0`, 0 px in the recycled buffer; live pane: image in alt (2048+2048 px) → primary 0 → `vim` 0. A/B: revert `place`'s `bump_mutations()` and the shipped test goes red on its own assertion (`left: 1, right: 0`) |
| P1-2 | `a=d` inside `CSI ?2026h` bypassed the freeze | **FIXED** | `before=1 during=1 after=0`; rendered px `during=304 after=0`. A/B: revert to `self.grid.unplace_all()` → test red (`left: 0, right: 1`) |
| P1-3 | diff shipped no QA evidence | **FIXED (evidence half)** | this report + `evidence-manifest.json` + three exact metrics land in this diff; `make check-vt-qa` green. Council half: see P2-4 |
| P2-1 | `window`/`session snapshot` show no image | **ACCEPTED — declared staging** | re-measured 0 px; `docs/designs/inline-images.md` work item 5. `pane snapshot` and `pane glance` agree **byte for byte** |
| P2-2 | image overran its reserved rows under a non-default `appearance.font` | **FIXED** | 6 font faces, 7x14 → 16x17 and 12x23: no overrun in any. A/B: remove the conversion and the 7x14 cell overruns rows AND columns and eats the text |
| P2-3 | placements invisible to the revision/settle substrate | **FIXED** | `content_revision` 2 → 3 on arrival with zero cell writes; `wait-settled --quiet 3s` returns at 4.99 s with the image vs 2.99 s without |
| P3-1 | decode runs before the visibility test | **STILL PRESENT, amplified** | quantified below |
| P3-2 | an abandoned `m=1` transfer eats the next image | **STILL PRESENT, not reachable via `kitten icat`** | mechanism identified below |
| P3-3 | `X=` ignored though the committed fixture carries it | **STILL PRESENT** | `X` appears nowhere in `crates/shux-vt/src/graphics/kitty.rs` |
| P3-4 | the chunking test compares an `Observable` with no placements | **MATERIALLY RESOLVED** | `Observable.content_revision` now discriminates: `rev=2, placements=1` vs `rev=1, placements=0` |

## Acceptance criteria

| # | Criterion | Verdict | Evidence |
|---|---|---|---|
| 1 | default `kitten icat FILE` reaches `pane snapshot` | **PASS** (re-confirmed) | real `kitten icat` wire re-recorded on this run: 3 probes + `a=T,q=2,f=100,s=64,v=64`; `snap-f100-80x24.png`, 2048 px exact `#FF00FF` + 2048 px exact `#00C878`, 64x64 unclipped |
| 2 | chunked transfer arrives whole | **PASS** (re-confirmed) | `kitten icat noise360x266.png`, `f=24,o=z`, 94 continuation chunks; **95,760 px compared against the SOURCE png, 0 mismatched** |
| 3 | `f=24,o=z` and `f=100` both decode | **PASS** (re-confirmed) | rows 1 and 2; both metrics exact 0/0 |
| 4 | truecolor + indexed + basic survive beside an image | **PASS** (re-confirmed) | 80x24 / 120x40 / 200x60: `#FF6347` 43 px, indexed-93 `#8700FF` 17 px, bold basic red, yellow-on-navy after the image, image 4096 px exact — identical at all three breakpoints |
| 5 | rich TUIs do not regress | **PASS** | the leak is gone (`leak-C-vim-ghost.png` — `vim` clean, 0 px). Raw replay: 5 committed corpora x 3 chunkings (whole / 1 B / 7 B), rendered pixels hashed — **all 15 arms identical between `de1fadb` and `4238f6b`**. Live `btop` 120x40, `lazygit` 200x60, `nvim` 120x40 inspected at native resolution |
| 6 | synchronized output is not torn | **PASS** | place inside the window freezes (`during=0 after=1`); `a=d` inside the window now freezes too (`during=1 after=0`, 304 px held) |
| 7 | alt-screen recycling clears placements | **PASS** | `?1049`, `?1047` and `RIS` arms all `0`; double recycle `0`; live `vim` clean |
| 8 | tall image scrolls, and stops drawing when out | **PASS** | 36x190 image: 6840 → 4788 → 1368 → 0 px, `max_y` 189 → 132 → 37 → none. Repeated under the 7x14 cell box after the resize: 3920 → 2744 → 784 → 0, `max_y` 139 → 97 → 27 |
| 9 | hostile input neither hangs nor explodes | **PASS** (re-confirmed) | 256-placement cap holds (400 attempts → 256); a 360x266 `f=32` APC exceeds the APC cap and is dropped, not buffered; worst measured render 38.9 ms (see P3-1) |
| 10 | zero leaked daemons | **PASS** | 9 daemons, each pid read from `$XDG_RUNTIME_DIR/shux/shux.pid` **before** `shux daemon stop` and `/proc/<pid>` confirmed gone after. Final `/proc/*/exe` sweep over this checkout and every scratch build tree: **0**. No `pgrep -f` / `pkill -f` anywhere |
| 11 | pane / glance / window / session snapshots agree | **PASS for the shipped paths** | `pane snapshot` == `pane glance`: md5-identical at 4 cells, and 0/328320 through `pixel_verify.py`. `window`/`session snapshot` 0 px — **declared work item 5**, see P2-1 |
| 12 | config states | **PASS** | default · `config init` · feature-maxed (`appearance.font` x 6 faces) · malformed · hot-reload all render correctly and none overruns |

## Testing matrix

| Layer | Result |
|---|---|
| Unit + integration (`make test`) | **2244 passed, 2 skipped, exit 0** (`.shux/out/kitty-graphics-render/r2/make-test.log`) — 2241 at `8d4cc8f`, +3 from this fix |
| Lint (`make lint`) | exit 0 (`r2/make-lint.log`) |
| `make check-vt-qa` | guard self-test green (8 arms), evidence conforming |
| Regression tests seen failing first | both new tests red on a one-hunk revert, in **release** mode so the debug assertion is not what catches it; the pin test red on a drifted constant (`left: (9, 19), right: (9, 20)`) |
| Raw replay | 5 committed rich-TUI corpora x 3 chunkings, rendered-pixel hash: 15/15 identical `de1fadb` vs `4238f6b`; hash shown to discriminate a 10% truncation |
| Real `kitten icat` wire capture | re-recorded on this run via `pane record`; control blocks parsed independently |
| shux automation | 15 sessions across 2 isolated `XDG_RUNTIME_DIR`s, 80x24 / 120x40 / 200x60 |
| Visual inspection | 9 PNGs opened at native resolution |
| Pixel comparison | 3 exact metrics (0/0), all `"status": "pass"`; comparator shown to fail on a 1-px corruption (exit 1) and on a missing baseline (exit 2) |
| DootSabha design | **absent** — design record is prose (`docs/designs/inline-images.md`), not a council. See P2-4 |
| DootSabha implementation-diff | **absent** — substituted by two independent QA-gate runs. See P2-4 |

## Screenshot matrix

All under `.shux/out/kitty-graphics-render/r2/`. No screenshot is committed: no
metric compares against a baseline this repo tracks (`.shux/qa/README.md`,
*"Screenshots are conditional"*), so the three metric JSONs stand alone.

| Viewport | Command / app | Screenshot | Baseline | Metric | Status |
|---|---|---|---|---|---|
| 80x24 | `kitten icat quad64.png` | `snap-f100-80x24.png` | source PNG (untracked) | `pixel-icat-f100-80x24.json` | 0/4096 |
| 80x24 | `kitten icat noise360x266.png` (94 chunks) | `snap-chunk-80x24.png` | source PNG (untracked) | `pixel-icat-chunked-f24-oz-80x24.json` | 0/95760 |
| 80x24 | glance vs pane snapshot | `glance-f100-80x24.png` | `snap-f100-80x24.png` | `pixel-glance-vs-pane-snapshot-80x24.json` | 0/328320 |
| 80x24 | alt screen holding an image | `leak-A-alt-with-image.png` | — | 2048+2048 px | positive control |
| 80x24 | primary after the alt app exits | `leak-B-primary.png` | — | 0 px | clean |
| 80x24 | **`vim` on the recycled alt buffer** | `leak-C-vim-ghost.png` | — | 0 px | **FIXED** |
| 80x24 | `appearance.font` 9x19 / 9x17 / 9x14 / **7x14** / 12x23 / 16x17 | `font-{default,dejavu,freemono,cjk,loma,dejavuserifbold}-80x24.png` | — | no overrun in any | clean |
| 80x24 | same, conversion removed | `noconv-font-cjk-80x24.png` | — | overruns rows+cols, eats the text | A/B control |
| 120x40 / 200x60 | image + colour probe | `snap-probe-{120x40,200x60}.png` | — | 4096 px + exact colour counts | clean |
| 120x40 | `btop` | `tui-btop-120x40.png` | — | eyeball | clean |
| 200x60 | `lazygit` | `tui-lazygit-200x60.png` | — | eyeball | clean |
| 120x40 | `nvim` | `tui-nvim-120x40.png` | — | eyeball | clean |

## Findings

Nothing at P0 or P1.

### P2-1 — `window.snapshot` / `session.snapshot` still show no image (accepted staging)

Re-measured: `pane snapshot` 4096 px, `pane glance` 4096 px and byte-identical
to it, `window snapshot` 0 px, `session snapshot` 0 px. `shux-ui`'s composer
copies cells and not placements. `docs/designs/inline-images.md` stages this as
work item 5 and the PR's scope statement now names it, so this is a **stated
limitation of the shipped scope**, not a hidden gap. Recorded, not failed.

### P2-4 — no council record for this change, and it cannot be checked from here

`command -v dootsabha` still exits 1. The diff carries a design *document*
(`docs/designs/inline-images.md`, D1/D2/D11 plus item-4 decisions) but no council
output and no substitution artifact. CLAUDE.md's fallback puts the substitution —
parallel adversarial agents on disjoint surfaces — in the PR description, and
`gh` is not installed on this host (`gh: command not found`), so I cannot read
it. What I can say:

- The **implementation diff** has now been driven adversarially twice, by two
  independent gate runs against the real system, and both runs' findings were
  fixed with tests seen failing first. `council-substitution.md` records this.
- The **design** step has a written record but no adversarial pass on record.

A reviewer must confirm the PR names its substitution. If it does not, feature
protocol steps 1 and 7 are unmet — that is a process gap outside this gate's
reach, and it is why this is called out rather than left in residual risk.

### P3-1 — decode *and now resize* run before the visibility test

`composite_placements` decodes, then resizes, then tests `skip >= src.height()`.
A placement scrolled entirely above the viewport pays both. The P2-2 fix
amplified this because the resize is the expensive half. Measured, 24x80 pane,
256 placements (the cap) of a 90x95 image, all scrolled fully out:

| Cell box | render |
|---|---|
| bundled 9x19 (no resize) | 1.58 ms |
| CJK gothic 7x14 (resize fires) | **38.9 ms** |
| same font, no placements at all | 0.30 ms |

130x the empty-pane cost, per snapshot, for pictures nobody can see. Not a
correctness defect and not client-unbounded — `MAX_PLACEMENTS` 256 and
`MAX_IMAGE_BYTES` 32 MiB hold, and `font_size` is hard-coded 14.0 so the scale
factor is bounded by the installed font's metrics (measured 6x15 … 18x17 across
168 faces). Moving the `skip`/`top` visibility test above `decode_placement`
would remove it entirely. Note, not a gate failure.

### P3-2 — an abandoned `m=1` transfer still eats the next image, but `kitten icat` cannot reach it

Reproduced at library level with exact inputs → wrong output: one unterminated
`a=T,m=1`, then three complete `a=T` images at distinct columns. All three are
placed (`placements=3`), but the first renders **0 px** — its payload carries the
abandoned prefix and `RgbaImage::from_raw` rejects it — while the second and
third render 304 px each.

**It is not reachable through `kitten icat`.** Live repro with the real client:
an abandoned `m=1` chunk followed by `kitten icat quad64.png` renders the image
correctly (2048+2048 px, `p32-first.png`). The reason is mechanical: icat probes
`t=t` and `t=s` first, `graphics::kitty::parse` refuses those, and the refusal
path calls `assembler.abort()`. Any client that probes — which the default icat
always does — clears the stale transfer before its own image. A client that
streams images back-to-back without probing and abandons one would lose exactly
one picture, self-healing. Real code path, no user-facing trigger on the
transports this PR ships. Note.

### P3-3 — `X=` is still parsed by nobody

`crates/shux-raster/tests/fixtures/icat-32x32-png.bin` carries `X=2`; `X` occurs
nowhere in `graphics/kitty.rs`. Worth at most one cell of horizontal offset.
Declared out of scope. Unchanged.

### P3-4 — the chunking test now has a channel that can see a placement

`a_chunked_command_places_the_same_however_it_is_delivered` still compares an
`Observable` with no placements field, but `Observable.content_revision` is in
that comparison and the P1-1 fix made it move on a placement: measured
`rev=2, placements=1` with the picture against `rev=1, placements=0` without. A
lost or duplicated placement now changes the compared value. A *corrupted*
payload of the same count still would not. Materially resolved; the residue is
narrow.

### P3-5 — the commit message overstates `diff --since`

The commit says counting the placement makes an image "visible to … `diff
--since` and `lens gate`". Measured: after an image arrives with `C=1`,
`pane diff --since <rev>` reports `cells_changed: 0`, `cursor_moved: false`, an
empty bounding box and no regions — only `to_revision` moves. `wait-settled` and
`content_revision` genuinely do see it; the **cell-level** diff and any cell-tier
`lens gate` golden still cannot, because an image writes no cells. The code is
right; the sentence is wider than the measurement. Documentation note.

## Passed evidence

- **The alt-screen leak is gone, in a real pane.** The comparator is shown able
  to see the picture in that same pane and viewport (`leak-A`: 2048 magenta +
  2048 green at `(0,0)-(63,63)`) before it reports `0` for `vim` — so the zero is
  an absence, not a blind spot.
- **The recycling fast path the fix could have broken is intact.** White-box
  probe against `ScreenSwap` in a throwaway worktree: after a bare
  `?1049h`/`?1049l` the parked buffer still satisfies `is_blank_canvas` (so
  entering recycles without re-blanking, issue #106's optimisation), and after a
  toggle that *placed*, it does not. 100 bare toggles leave
  `content_revision` and `mutations` at 0, so the substrate gains no noise.
- **`is_actually_blank` now catches what the cheap check catches.** With the
  `place` bump reverted, the debug assertion in `screen.rs:76` fires — the
  debug-build tripwire the previous audit found inert is live again.
- **The declared-to-drawn conversion is load-bearing and correct in both
  directions.** Six font faces spanning 7x14 to 16x17 and 12x23: the image lands
  inside its reserved `ceil(w/9) x ceil(h/19)` box in every one, shrinking and
  growing alike. Removing the conversion and rebuilding reproduces the original
  defect exactly — 64x64 into a 7x14 cell overruns to `(63,63)` against a 28x56
  reservation and paints over `TRUECOLOR` and `FONT_42_MARK`.
- **Scroll clipping survives the resize.** A 36x190 image partially scrolled out
  clips at the right source row under both the 9x19 and the 7x14 cell box, and
  reaches exactly 0 px when fully out.
- **The pin test is real.** Drifting `shux_vt::DECLARED_CELL_PIXELS` to `(9, 20)`
  reddens it with the two values named.
- **Every comparator was falsified before being trusted.** `pixel_verify.py`:
  `status: fail` / exit 1 on a single corrupted pixel, exit 2 on a missing
  baseline. The colour counter: 2048 px where the image is, 0 where it is not,
  in the same pane. The replay hash: discriminates a 10% truncation. The overrun
  detector: `overruns_reserved_rows: true` on the no-conversion build,
  `false` on the shipped one.
- **Rich-TUI replay is byte-identical to base.** 15 arms, no divergence.
- **`pane capture` stays text.** No APC bytes, no payload.

## Residual risk

- `window`/`session snapshot` and attach still show nothing (items 5 and 6).
  A user who splits a pane sees the picture in `pane snapshot` and not in
  `window snapshot`; that asymmetry ships.
- Every pixel here is `shux-raster`'s. Nothing in this audit can see what an
  outer terminal would draw. That is correct for this change (it emits nothing
  new outward) and stops being correct at item 6.
- P3-1's cost is bounded but real for anyone who sets `appearance.font` and
  leaves a pane full of scrolled-away images.
- The council record (P2-4) is unverifiable from this host.

## Cleanup

Two isolated runtime dirs (`/tmp/kg2`, `/tmp/kgf`) plus the base worktree at
`/tmp/shux-base` and a throwaway head worktree at `/tmp/shux-head`. Nine daemons
started, nine stopped: each pid read from `$XDG_RUNTIME_DIR/shux/shux.pid`
**before** `shux daemon stop`, `/proc/<pid>` checked **after**. Neither runtime
dir holds a pidfile now. The final sweep walked `/proc/*/exe` for anything
resolving into `/home/user/shux/target/`, `/tmp/shux-head/target/`,
`/tmp/shux-base/target/` or the probe crates' target dirs: **0 processes**. No
`pgrep -f` / `pkill -f` was used. `/tmp/shux-head` was removed and its worktree
entry pruned; `/tmp/shux-base` is left in place for the next A/B. `git status
--short` is clean apart from this audit's own evidence.
