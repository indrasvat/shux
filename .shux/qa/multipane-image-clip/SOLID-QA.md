VERDICT: FAIL

# SOLID VT QA — multipane-image-clip

## 1. Change under audit

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| HEAD | `3d46d74` |
| Base | `origin/main` = `0e97d24` (v0.49.0) |
| Merge base | `0e97d2419cdfa86d5457bcee19f109d155cf5271` |
| Diff | `crates/shux-raster/src/lib.rs`, `crates/shux-ui/src/composed.rs`, `crates/shux-vt/src/{grid,lib}.rs`, `crates/shux/src/snapshot.rs`, `crates/shux/tests/window_snapshot_images.rs` (517+/60-) |
| Issue/PR | none named; scope derived from the diff and from the four claims the parent stated |
| Scratch | `.shux/out/multipane-image-clip/` (gitignored) |

No `dootsabha` on this machine; see `council-substitution.md`.

## 2. Verdict

FAIL, on one P1. The **behaviour** the change claims is correct — I could not
falsify any of the four claims, at three viewport breakpoints, five config
states, a hot reload, five committed rich-TUI replays and a zoomed pane, with 25
exact `0`/`0` pixel metrics and two independent negative controls proving the
comparators can fail. What fails is the **regression protection**: the single
line that connects all of it to `window.snapshot` and `session.snapshot` is
pinned by nothing, and I removed it and watched all 896 tests in the `shux`
crate pass.

## 3. Stated DoD matrix

The four claims the parent put up to be falsified.

| # | Claim | Result | Evidence |
|---|---|---|---|
| 1 | A picture in one pane of a split appears in `window`/`session snapshot`, same height and same slice as `pane snapshot` | **PASS** | 18 exact 0/0 metrics: `pixel-crosspath-c{80x24,120x40,200x60}-{small,big}.json` (crop of the window PNG at the pane rect ≡ that pane's own `pane snapshot`, byte-for-byte), plus `pixel-session-vs-window-c120x40-big.json`. Negative control: the same comparison against the `origin/main` binary FAILS with 373293 changed pixels (`metrics/NEGCTL-crosspath-base.json`) — the bug is real and this change is what fixes it. |
| 2 | Never paints outside its pane — not a neighbour, not a border, not the status bar | **PASS** | `pixel-outside-pane-*.json`: mask the image pane out and the window is pixel-identical to an image-free control — borders, neighbour and status bar all 0 changed pixels. Independently, magenta-pixel bbox equals the pane rect exactly at every size/font (`.shux/out/.../assess.py` output). Negative control: a binary built from this HEAD with the clip removed leaves 29205 pixels outside the pane and visibly erases the status bar (`metrics/NEGCTL-outside-pane-noclip.json`, `shots/nc120x40-big-window.png`). |
| 3 | Additionally bounded by what the SOURCE GRID owns, not just the pane rect | **PASS (unit only)** | `an_image_larger_than_its_pane_is_drawn_and_stays_inside_it` puts a 37×23 grid in a 49×27 rect; mutation `M4_clip_ignores_src_dims` (drop both `.min(src_grid…)`) is caught by it. I could not construct the resize-lag window live, so this claim rests on the unit test plus the mutation, not on a daemon-backed capture. Stated, not hidden. |
| 4 | Rich TUIs unaffected when no image is present | **PASS** | `origin/main` vs HEAD, whole composed window, exact 0/0, for five committed raw replays — `pixel-richtui-ab-{btop,lazygit,nvim,vicaya,vivecaka}.json` — and for a coloured shell split (`pixel-noimage-ab-120x40.json`). Each replay pane carries 2041–6102 distinct colours. |
| — | Mutation matrix: 7 reintroduced defects caught, correct tree caught by none | **PARTIAL — see P1-1** | I ran 12 of my own mutations, not theirs. Correct tree: 3/3 green. 11/12 caught. The survivor is the production call site. |

## 4. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit (touched crates) | PASS | `cargo nextest run -p shux-raster -p shux-vt -p shux-ui` → 911 passed (raster 28, vt 647, ui 236). Log `.shux/out/multipane-image-clip/unit-touched-crates.log`. |
| Integration (workspace) | PASS | `make test` → **2251 passed, 2 skipped**. Log `.shux/out/multipane-image-clip/make-test.log`. |
| New tests, re-run | PASS | `cargo nextest run -p shux --test window_snapshot_images` → 3/3. |
| Mutation / can-it-fail | **FAIL** | 12 mutations in a private worktree. 11 caught; `M8_snapshot_no_call` survives. Table: `.shux/out/multipane-image-clip/mutation-matrix.txt`, full-crate run `mut-M8-full-shux-crate.log`. |
| Raw byte / replay | PASS | All five `.shux/fixtures/vt-corpus/rich-tui/*.raw` replayed through real panes under both binaries. |
| Shux automation | PASS | 40 daemon-backed runs, isolated `XDG_RUNTIME_DIR` per run, real `kitten icat`, real `ls --color`, real replays. Driver `.shux/out/multipane-image-clip/drive.sh`. |
| Colour probes | PASS | truecolor `38;2;0;200;90`, indexed `38;5;208`, basic `34` in every driven pane. Read back from the window PNG: both panes render `(0,200,90)`, `(255,135,0)`, `(36,114,200)` identically (43/39/… px each). |
| Visual inspection | PASS | Full-resolution PNGs opened as images, listed in §5. No clipping, tofu, ghost cells, colour bleed, cursor artifacts or layout drift. |
| Pixel comparison | PASS | 25 committed metrics, every one `status: pass` with exact `0`/`0`. |
| Comparator falsifiability | PASS | `pixel_verify.py` FAILS on the clip-removed binary (29205 px), FAILS on the base binary (373293 px), and exits non-zero on empty and on missing input (`metrics/NEGCTL-*`). |
| DootSabha design | PASS (substituted) | `council-substitution.md` §1 — two adversarial design reviews with measured findings, three of them independently re-derived by my mutation matrix. |
| DootSabha implementation diff | **FAIL — not complete** | `council-substitution.md` §7. See P2-1. |
| Lint | PASS | `make lint` (clippy `-D warnings` + fmt-check). Log `lint.log`. |
| Process hygiene | PASS | 0 daemons and 0 leftover runtime dirs at close; each run's pid read from `$XDG_RUNTIME_DIR/shux/shux.pid` before `daemon stop` and `/proc/<pid>` polled to disappearance. `daemon-hygiene.txt`. |

## 5. Screenshot matrix

No committed baseline exists for any metric, so per `.shux/qa/README.md` no PNG
is committed; every frame below lives in gitignored scratch and was opened at
native resolution during this audit.

| Viewport | Scene | Frame (scratch) | Baseline | Metric | Status |
|---|---|---|---|---|---|
| 120×40 | 180×95 image + probes ‖ `ls --color` | `shots/c120x40-small-window.png` | none | `pixel-crosspath-c120x40-small.json` | PASS — image inside pane, both panes' probes legible, borders + status bar clean |
| 243×39 | 600×900 image ‖ **btop** raw replay | `shots/img-btop-window.png` | none | `pixel-crosspath-img-btop.json`, `pixel-outside-pane-img-btop.json` | PASS — btop pane byte-identical to the image-free control |
| 120×40 | 600×900 image, `appearance.font` = japanese-gothic (**7×14** cell), ascii borders | `shots/cfg-maxed-cjk-window.png` | none | `pixel-crosspath-cfg-maxed-cjk.json` | PASS — clip exact at a non-default cell box |
| 120×40 | **clip removed** (negative control, built from this HEAD) | `shots/nc120x40-big-window.png` | none | `metrics/NEGCTL-outside-pane-noclip.json` | FAIL as designed — paints the top border, the pane title, the bottom border and the whole status bar |
| 120×40 | `origin/main` binary, same scene | `shots/base120x40-big-window.png` | none | `metrics/NEGCTL-crosspath-base.json` | FAIL as designed — 0 image pixels in the window snapshot, 373293 in the pane snapshot |
| 80×24, 200×60 | small + big image | `shots/c{80x24,200x60}-{small,big}-window.png` | none | 8 metrics | PASS |
| 120×40 | zoomed pane, oversize grid (130×50 into 120×39) | `shots/zoom-window.png` | none | inline assertion | PASS — 414600 image px, 0 in the status-bar rows |

## 6. Findings

### P1-1 — the production call site is pinned by nothing; the feature can be unwired with the suite green

`crates/shux/src/snapshot.rs:298` is the only production caller of
`Rasterizer::composite_composed`:

```
grep -rn "composite_composed" crates/
  crates/shux/src/snapshot.rs:298                 <- the only product caller
  crates/shux/tests/window_snapshot_images.rs:128 <- the tests' own pipeline
```

The three new tests rebuild `compose` → `render` → `composite_composed`
themselves and never call `snapshot_window`. Replace that line with
`let _ = &composed.placements;` and:

- `cargo nextest run -p shux --test window_snapshot_images` → **3 passed**
- `cargo nextest run -p shux` (the whole crate, daemon-backed tests included)
  → **896 passed, 2 skipped**
  (`.shux/out/multipane-image-clip/mut-M8-full-shux-crate.log`)

No shell check under `.shux/scripts/` places an image in a window snapshot
either, so nothing in the repository notices. Every other mutation I reintroduced
died — 11 of 12 — which is what makes this one legible: the matrix stops one
function call short of the two RPCs the change exists for.

This is the same shape as the defect being fixed. `window.snapshot` shipped
without pictures for a release because nothing exercised the composed snapshot
path end-to-end; the fix does not close that hole, it reproduces it one layer up.
CLAUDE.md: *"Every fix ships with a test seen failing first"*, and *"applies
hardest to defects in verification machinery."*

Fix shape: one daemon-backed test that drives `window.snapshot`/`session.snapshot`
through the RPC and asserts image pixels land inside the pane rect. My driver at
`.shux/out/multipane-image-clip/drive.sh` plus `assess.py` is that test in shell
form and takes ~12s per case; it caught M1 (clip removed) on a real binary.

### P2-1 — implementation-diff council (protocol step 7) not complete at the audited HEAD

No implementation-diff council record exists at `3d46d74`, and `dootsabha` is not
installed. The parent reported two adversarial agents running in their own
worktrees during this audit; their output did not exist when the audit closed, so
it was not read, not reproduced and not judged. Per CLAUDE.md the step is *"not
optional, and not scaled down for small diffs"*, and per this gate's contract
missing evidence is failure, not residual risk. Recorded in
`council-substitution.md` §7 rather than asserted as done.

### P3-1 — `shux daemon stop` can return 0 with the daemon still running (PRE-EXISTING, not this change)

`crates/shux/src/daemon_boot.rs:308-338` polls `kill(pid, 0)` 40×50 ms and, on
timeout, prints a warning and still exits 0. Observed twice in this audit
(lazygit and nvim replay sessions): `/proc/<pid>` was still present immediately
after `daemon stop` returned and went away 100–200 ms later. Reproduces on the
`origin/main` binary as well, so it is not caused by this diff — but a cleanup
trap that trusts `daemon stop`'s exit status can report success on a live daemon.
My harness polls to disappearance instead of trusting it.

### P3-2 — live attach still draws no pictures; the divergence from snapshot widened (SCOPED OUT, verified)

`crates/shux-ui/src/compositor.rs` contains no reference to placements
(`grep -n placement` → no match), so `shux attach` renders a split with text and
no images while `pane`/`window`/`session snapshot` now all render them. The
parent scoped `compositor.rs` out as later work; recorded because a user who
attaches and then snapshots the same window now sees two different pictures, and
that is worth one line in the PR description.

### P3-3 — two of the five rich-TUI replay arms are only conditionally deterministic

`nvim.raw` and `lazygit.raw` contain terminal queries (XTVERSION/DA1); shux's
replies arrive as PTY *input* and the shell echoes them at a variable point, so a
naive replay is capture-nondeterministic — head-vs-head differed by exactly the
same 13867 pixels as base-vs-head (`metrics/pixel-det-nvim.json` vs
`pixel-richtui-ab2-nvim.json`), which is how I know the difference was the scene
and not the binary. `stty -echo` before the replay makes both arms exactly
reproducible, and the committed `pixel-richtui-ab-{nvim,lazygit}.json` are from
those runs. Noted so nobody re-derives the false regression.

## 7. Passed evidence

- 25 committed pixel metrics, all `status: pass`, all exact `0`/`0`.
- Cross-path identity (window-snapshot crop ≡ pane snapshot) holds at 80×24,
  120×40, 200×60; for an image that fits and one that overflows both axes; under
  `default`, `config init`, DejaVu (9×17), FreeMono (9×14), japanese-gothic
  (7×14) and a malformed config; across a live hot reload that moved the cell box
  from 9×19 to 9×17 under a running daemon with a live picture.
- Image extent equals the pane rect exactly at every cell box:
  531×703 = 373293 px (9×19), 531×629 = 333999 (9×17), 531×518 = 275058 (9×14),
  413×518 = 213934 (7×14). Cell-granular clipping in a pixel blit would not land
  on these numbers.
- `session.snapshot` ≡ `window.snapshot` byte-for-byte in all 9 size cases.
- Zoomed pane with an oversize grid: 0 image pixels in the status-bar rows.
- Single-pane path behaviour-preserved: 28/28 `shux-raster` tests, and the
  refactor of `composite_placements` into `blit` degenerates to the old
  arithmetic when the clip is the whole canvas.
- `make lint` and `make test` (2251) green on the audited tree.

## 8. Residual risk

- Claim 3 (source-grid bound) is proven by unit test + mutation only; I did not
  reproduce a live resize lag through the daemon.
- `composite_composed` is `pub` with an unstated precondition — it has no left
  clip and relies on every producer deriving `ox` and `clip.col` from the same
  pane origin. Sound for the one caller today; the `debug_assert` that guarded it
  was deliberately removed in `3d46d74`. The live-attach slice that lands next is
  where this bites.
- Deep scrollback was not exercised: every capture here has short history, so
  `viewport_row`'s `evicted() + scrollback_len()` term was small.
- Emit-path blindness: everything above was drawn by `shux-raster`. Nothing here
  photographs what an outer GUI terminal draws (`make test-gui-terminal` not run;
  this change emits nothing new to an outer terminal).

## 9. Cleanup

Zero leaked daemons and zero leftover runtime dirs at close
(`ps -eo pid,args | grep -c "[s]hux __daemon"` → 0; `ls -d /tmp/sxq.*` → none).
Every one of the 40 runs read its pid from `$XDG_RUNTIME_DIR/shux/shux.pid`
BEFORE `daemon stop` and then polled `/proc/<pid>` to disappearance, failing loud
past 20 s; per-run log in `.shux/out/multipane-image-clip/daemon-hygiene.txt`.
No `pgrep -f` / `pkill -f` was used. The mutation worktree
(`scratchpad/mut`) is restored to a clean `3d46d74`; no product source in
`/home/user/shux` was modified by this audit.

Disk note: `/` was at 100% (56 MB free) when this audit started and no build
could run. I deleted `target/aarch64-apple-darwin` (a Linux-unusable
cross-compile tree) and `target/debug/incremental` (regenerable cache) to
proceed. No source, no tracked file and no non-regenerable artifact was touched.
