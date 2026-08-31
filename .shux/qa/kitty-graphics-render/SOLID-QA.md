VERDICT: FAIL

# SOLID VT QA — kitty-graphics-render

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| Commit audited | `8d4cc8f` — *feat(vt): render kitty-graphics images into pane snapshots* |
| Base | `de1fadb` |
| Design | `docs/designs/inline-images.md`, work item 4 |
| Scratch | `.shux/out/kitty-graphics-render/` (gitignored) |

The branch tip advanced to `10d9cbf` during this audit. `git diff --stat
8d4cc8f..10d9cbf -- crates/ Cargo.lock Cargo.toml` is empty — `10d9cbf` is
docs-only — so every measurement below still describes `8d4cc8f`'s code.

Three findings force the verdict: a **cross-application image leak through the
recycled alternate-screen buffer**, reproduced in a real pane with `vim`; a
**synchronized-output tear on `a=d`** that contradicts this change's own stated
design decision; and **no QA evidence or council record in the diff at all**
(`make check-vt-qa` exits 2 on it).

The headline deliverable itself works, and works exactly: real `kitten icat`
with no `--transfer-mode` flag renders pixel-identically to the bytes it put on
the wire.

## Acceptance criteria

| # | Criterion | Verdict | Evidence |
|---|---|---|---|
| 1 | default `kitten icat FILE` reaches `pane snapshot` | **PASS** | `snap-icat-default-80x24.png`; 2048 px exact `#FF00FF` + 2048 px exact `#00C878`, bbox `(324,19)-(387,82)` = col 36 row 1, 64x64 unclipped. A/B: base `de1fadb` prints *"This terminal does not support the graphics protocol"* and draws nothing (`base-snap-icat-80x24.png`). |
| 2 | chunked transfer arrives whole | **PASS** | Recorded PTY: 355 APCs, first `a=T,q=2,f=24,o=z,m=1,s=720,v=540`, then 350 x `a=T,q=2,m=1` (4096 b64 each), final `a=T,q=2` (1872). 1,439,660 b64 bytes. shux's PNG == the wire bytes inflated independently in Python: **328,320 px compared, 0 mismatched**. |
| 3 | `f=24,o=z` and `f=100` both decode | **PASS** | `f=24,o=z` per row 2. `f=100`: recorded wire for the 64x64 file is `a=T,q=2,f=100,s=64,v=64` (236 b64), pixel-exact vs the source PNG (`pixel-icat-f100-80x24.json`, 4096/4096). |
| 4 | truecolor + indexed + basic survive beside an image | **PASS** | `snap-colorprobe-{80x24,120x40,200x60}.png`: `#FF6347` 165 px, indexed-93 `#8700FF` 116 px, bold basic red, yellow-on-navy *after* the image, image magenta 2048 px exact — all three breakpoints identical. |
| 5 | rich TUIs do not regress | **FAIL** | Clean-path rendering is fine (see below) and replay is byte-identical to base. But criterion 7's leak puts a **previous application's picture over `vim`'s buffer** — `leak-4-vim-ghost.png`. |
| 6 | synchronized output is not torn | **FAIL** | Placement inside `CSI ?2026h` **does** freeze correctly (before=0 during=0 after=1). `a=d` inside the same window does **not** (before=1 **during=0** after=0). See P1-2. |
| 7 | alt-screen recycling clears placements | **FAIL** | Library: `in_alt=1 back_primary=0 **recycled=1**`. Real pane: `vim`'s alternate screen carries 4096 px of a dead application's image at `(0,0)-(63,63)`. See P1-1. |
| 8 | tall image scrolls, and stops drawing when out | **PASS** | Library: 36x190 px image, lit px 7220 → 4712 after 3 rows → **0** when fully scrolled out. Real pane: bbox moves to `(0,190)-(63,253)`, then 0 px after 40 more lines. |
| 9 | hostile input neither hangs nor explodes | **PASS** | 8 probes, each ran and the pane still printed afterwards. Worst case 1.57 ms. Caps hold: 1000 `a=T` → 256 placements; 400 MiB zlib bomb → refused, render 438 µs. |
| 10 | zero leaked daemons | **PASS** | Pids read from `$XDG_RUNTIME_DIR/shux/shux.pid` *before* `shux daemon stop`, `/proc/<pid>` confirmed gone after: 12616, 1034, 7422, 17304, 28645. Final `/proc/*/exe` sweep for this checkout's binary: 0. Base worktree removed. |
| 11 | pane / glance / window / session snapshots agree | **FAIL** | `pane snapshot` 2048 px, `pane glance` 2048 px, `window snapshot` **0 px**, `session snapshot` **0 px**. `pane capture` correctly unaffected (43 bytes of text, no APC leakage). See P2-1 for the staging argument. |
| 12 | config states | **PARTIAL FAIL** | default / `config init` / malformed / hot-reload all render the image correctly. **feature-maxed fails**: see P2-2. |

## Testing matrix

| Layer | Result |
|---|---|
| Unit + integration (`shux-vt`, `shux-raster`) | 669 passed / 0 failed |
| Workspace (`make test`, clean run) | 2241 passed, 2 skipped, exit 0 — `.shux/out/kitty-graphics-render/make-test-clean.log` |
| Lint (`make lint`) | exit 0 |
| `make check-vt-qa` on this diff | **exit 2** — VT paths touched, no `.shux/qa/<scope>/` evidence in the diff |
| Raw replay | 5 committed rich-TUI corpora (`btop`, `lazygit`, `nvim`, `vicaya`, `vivecaka`) at 3 chunkings (whole / 1 B / 7 B), grid text + rendered pixels hashed: **head == base, all 15 arms** |
| Real `kitten icat` wire capture | 4 invocations recorded via `shux pane record`, control blocks parsed independently |
| shux automation | 12 sessions across 3 isolated `XDG_RUNTIME_DIR`s, 80x24 / 120x40 / 200x60 |
| Visual inspection | 11 PNGs opened at native resolution |
| Pixel comparison | 2 exact metrics (0/0), both `"status": "pass"` |
| DootSabha design | **absent** — see `council-substitution.md` |
| DootSabha implementation-diff | **absent** — see `council-substitution.md` |

## Screenshot matrix

All paths are under `.shux/out/kitty-graphics-render/`. Nothing is committed as
a screenshot: no baseline this repo tracks exists to diff any of them against
(`.shux/qa/README.md`, *"Screenshots are conditional"*), so the two pixel-metric
JSONs stand alone.

| Viewport | Command / app | Screenshot | Baseline | Metric | Status |
|---|---|---|---|---|---|
| 80x24 | `kitten icat quad64.png` (default) | `snap-icat-default-80x24.png` | source PNG, cropped | `pixel-icat-f100-80x24.json` | 0/4096 |
| 80x24 | `kitten icat noise1200.png` (352 chunks) | `snap-kg3-chunked-80x24.png` | wire bytes, inflated in Python | `pixel-icat-chunked-f24-oz-80x24.json` | 0/328320 |
| 80x24 | `kitten icat bands1200.png` | `snap-icat-bands-80x24.png` | — | eyeball + colour count | clean |
| 80x24 / 120x40 / 200x60 | colour probe + image | `snap-colorprobe-*.png` | — | exact-RGB counts | clean |
| 80x24 | `kitten icat --clear` before/after | `clear-{before,after}.png` | — | 2048 → 0 px | clean |
| 80x24 | scroll partial / fully out | `scroll-{partial,off}.png` | — | bbox → 0 px | clean |
| 80x24 | **`vim` after a dead app's image** | `leak-4-vim-ghost.png` | — | 4096 px ghost | **DEFECT** |
| 80x24 | `window` / `session snapshot` | `rp-window.png`, `rp-session.png` | — | 0 px | **DEFECT** |
| 80x24 | `appearance.font` = CJK gothic | `cfg-hotreload-cjk-80x24.png` | — | image overruns its rows | **DEFECT** |
| 80x24 | `vim` clean | `tui-vim-80x24.png` | — | eyeball | clean |
| 120x40 | `nvim` clean | `tui-nvim-120x40.png` | — | eyeball | clean |
| 120x40 | `btop` clean | `tui-btop-120x40.png` | — | eyeball | clean |
| 200x60 | `lazygit` clean | `tui-lazygit-200x60.png` | — | eyeball | clean |

## Findings

### P1-1 — a placement survives alternate-screen recycling and lands in the next application

`Grid::place` (`crates/shux-vt/src/grid.rs`) pushes to `placements` and adjusts
`placed_bytes`. It never touches `mutations`. `Grid::is_blank_canvas` decides
reuse from `mutations == 0` plus geometry, and `Screen::enter`
(`crates/shux-vt/src/screen.rs:64-88`) takes a fast path on that verdict which
**skips `reset_blank` entirely** — the function this diff added
`self.placements.clear()` to. The guard is bypassed in exactly the case it was
written for. `debug_assert!(spare.is_actually_blank(cols))` inspects cells only,
so a debug build does not catch it either.

Library probe (`/tmp/kgprobe`, `shux-vt` + `shux-raster` as path deps):

```
enter alt -> place -> leave alt -> enter alt
in_alt=1  back_primary=0  recycled=1
```

Real pane, `shux session create` + `kitten icat`-shaped APC, then `vim`:

```
stage A: printf '\033[?1049h'; cat <a=T APC>; printf '\033[?1049l'
         primary snapshot: 0 magenta px            (correct)
stage B: vim -u NONE /etc/os-release
         vim's alternate screen: 4096 magenta px at (0,0)-(63,63)
```

`leak-4-vim-ghost.png` shows the block painted over `PRETTY_NAME=` and
`NAME="Ubuntu"`. `docs/designs/inline-images.md` puts *"alt-screen placements
anchored to an absolute line"* in scope, so this is in-scope behaviour, and it
is a cross-application content leak, not a cosmetic one.

### P1-2 — `a=d` inside `CSI ?2026h` bypasses the freeze

`crates/shux-vt/src/lib.rs`, `dispatch_graphics`:

```rust
Action::Delete => {
    self.grid.unplace_all();
    self.graphics.assembler.abort();
}
```

`self.grid`, not `sync::Presented`. `crates/shux-vt/src/sync.rs`'s module doc
states the invariant plainly — *"there is no way to reach the mutable state
except through the freeze"* — and this diff adds one. `place_image` two
functions below does route correctly and carries a comment explaining why; the
delete arm was missed.

```
place -> ?2026h -> a=d,d=A -> ?2026l
before=1  during=0  after=0        (expected 1 / 1 / 0)
```

The image vanishes from a frame the terminal has promised to hold still — the
#115 class, in the one path this change's own design doc calls out: *"Placements
mutate the grid, but only through `sync::Presented`."* `kitten icat --clear`
emits `a=d`, and any image-drawing TUI that clears-then-redraws inside a
synchronized frame reaches it. Reproduced deterministically at library level;
the 150 ms `SYNC_UPDATE_TIMEOUT_MS` makes an RPC-paced pane repro racy, so I did
not claim one.

The rustdoc above `dispatch_graphics` is also now false: it still says *"Takes
no `&mut self`"* while the signature is `fn dispatch_graphics(&mut self, ...)`.

### P1-3 — the diff ships no QA evidence and no council record

`make check-vt-qa` on this diff:

```
VT QA CHECK FAILED: ... touches VT rendering paths ... but adds or updates
no .shux/qa/<scope>/ evidence
make: *** [Makefile:749: check-vt-qa] Error 2
```

`git show --stat 8d4cc8f` contains no `.shux/qa/` path, no dootsabha JSON, and
no substitution note. `dootsabha` is not installed (`command -v dootsabha` exits
1), which per CLAUDE.md means the step is *substituted*, not skipped — and the
substitution is unrecorded. See `council-substitution.md`.

### P2-1 — `window.snapshot` and `session.snapshot` do not show the image

`shux-ui/src/composed.rs::compose` builds a fresh `Grid` and copies **cells**
from each pane; `placements` are not carried, so the multi-pane composers render
the text and drop the picture.

```
pane snapshot   2048 px   pane glance     2048 px
window snapshot    0 px   session snapshot   0 px
```

`docs/designs/inline-images.md` stages this as work item 5 (*"multi-pane
composers — `window` / `session snapshot` — next"*), so it is a **declared**
staging rather than a hidden gap — but it is absent from the scope statement
this audit was given, whose OUT list names animation, `z`, `a=p`, `U=`,
`t=f`/`t=t`/`t=s`, `X=`/`Y=` and `c=`/`r=` and stops there. Criterion 11 as
stated is not met; a reader of the PR would not know why.

### P2-2 — image geometry is pinned to a constant that nothing pins

`crates/shux-vt/src/lib.rs:44` introduces `DECLARED_CELL_PIXELS: (u32, u32) =
(9, 19)` and claims *"`shux-pty` declares it; `crates/shux` pins the two
together."* It does not:

```
$ grep -rn DECLARED_CELL_PIXELS crates/ --include=*.rs
crates/shux-pty/src/handle.rs:378   pub const DECLARED_CELL_PIXELS: (u16, u16) = (9, 19);
crates/shux-pty/tests/integration.rs:609
crates/shux/src/snapshot.rs:496     shux_pty::DECLARED_CELL_PIXELS,
crates/shux-vt/src/lib.rs:44        pub const DECLARED_CELL_PIXELS: (u32, u32) = (9, 19);
crates/shux-vt/src/lib.rs:572
```

Two independent constants, different types, no test relating them. Nothing
would fail if one drifted.

Worse, the *rasterizer's* cell box is derived from the configured font, not from
either constant, while `place_image` reserves rows and columns from the `(9,19)`
literal. Measured across supported `appearance.font` values, 80x24 pane:

| `appearance.font` | snapshot | cell | image behaviour |
|---|---|---|---|
| default (bundled JetBrains Mono) | 720x456 | 9x19 | correct |
| DejaVu Sans Mono | 720x408 | 9x17 | gap below the image |
| FreeMono | 720x336 | 9x14 | overruns |
| Japanese Gothic | 560x336 | 7x14 | **overruns and paints over the next line of text** |

`cfg-hotreload-cjk-80x24.png` shows `AFTER-IMAGE-TRUECOLOR` half-covered.
`appearance.font` is documented and hot-reloads, so this is a supported config
state, and it is the feature-maxed cell of criterion 12.

### P2-3 — placements are invisible to the lens / settle / revision substrate

With `C=1` so nothing else moves, a placement changes the presented frame and
advances neither counter; a delete is the same:

```
placement-only:  content_revision 1 -> 1,  grid.mutations 0 -> 0,  placements=1
delete-only:     content_revision 1 -> 1,                          placements=0
```

`pane wait-settled`, `pane checkpoint` / `pane diff --since` and `lens gate`
therefore cannot see an image appear or disappear: a scenario whose only change
is an image reads as "nothing changed". This is also the mechanical root cause
of P1-1.

### P3-1 — decode happens before the visibility test

`composite_placements` calls `decode_placement` — zlib inflate and/or PNG decode
— and only afterwards tests `skip >= src.height()`. A placement scrolled fully
above the viewport is decoded in full on every render before being discarded.
Bounded by `MAX_IMAGE_BYTES` (32 MiB total) and `MAX_PLACEMENTS` (256), and
measured harmless at 438–610 µs in my floods, but the ordering is backwards and
there is no cache: every snapshot re-decodes every image.

### P3-2 — an abandoned chunked transfer eats the next image

`Assembler::feed` continues an open transfer into whatever arrives next, so one
unterminated `m=1` followed by three complete `a=T` transfers yields three
placements of which the first carries 100 bytes of the abandoned payload as a
prefix; `RgbaImage::from_raw` then rejects it and that picture never draws.
Self-healing and bounded, but silent.

### P3-3 — `X=` is declared out of scope and real `kitten icat` emits it

The change's own committed fixture,
`crates/shux-raster/tests/fixtures/icat-32x32-png.bin`, has control block
`a=T,q=2,f=100,s=32,v=32,X=2`. Ignoring `X=`/`Y=` misplaces a picture by up to
one cell. Cosmetic; not a break.

### P3-4 — the chunking test cannot see what it claims to compare

`a_chunked_command_places_the_same_however_it_is_delivered` compares
`Observable`, which has `frame`, `scrollback`, `cursor`, `title`,
`scroll_region`, `content_revision`, `responses` — and no placements. It detects
a *lost* placement only indirectly, through the cursor advance, and cannot
detect a corrupted or differently-assembled payload at all. Given P2-3,
`content_revision` cannot help it.

## Passed evidence

- **Real `kitten icat`, default invocation, pixel-exact.** The two committed
  metrics are 0/4096 and 0/328320 at `--max-pixel-diff-ratio 0
  --max-mean-channel-delta 0`. The chunked baseline is not shux's own output:
  it is the 352 recorded APC payloads, base64-decoded and zlib-inflated by
  Python, cropped at the 57 px scroll offset shux ended on.
- **Every comparator was run against a reintroduced defect and against empty
  input before being trusted.** The wire comparator: 6000 px on a corrupted PNG
  (exit 1), `AttributeError` + exit 1 on an empty recording. `pixel_verify.py`:
  `status: fail` / exit 1 on the corrupted PNG, exit 2 on a missing baseline.
  The replay hash: discriminates a 10% truncation and a single flipped letter,
  and discriminates an empty terminal.
- **Rich-TUI replay is byte-identical to base.** 5 committed corpora x 3
  chunkings, grid text and rendered pixels folded into one hash: no arm differs
  between `de1fadb` and `8d4cc8f`.
- **Live rich TUIs render correctly** when no kitty client has been near the
  pane: `vim` 80x24, `nvim` 120x40, `btop` 120x40, `lazygit` 200x60, all
  inspected at native resolution.
- **Hostile input is bounded.** `v=4294967295` (50 µs, clamped to the pane),
  `s=0` (rejected), a PNG declaring no size (rejected), non-base64 garbage
  (rejected), a truncated PNG (stored, refused at decode), a 400 MiB zlib bomb
  compressed to a few KB (rejected by the exact-size inflate), an unterminated
  transfer followed by more images, and 1000 placements capped to 256.
- **`kitten icat --clear` works**: 2048 px → 0 px in a real pane.
- **`pane capture` is unpolluted**: 43 bytes of text, no APC bytes, no payload.

## What could not be verified, and why

- **`a=d` tearing in a live pane.** Proven deterministically in-process. Not
  reproduced through the RPC surface because `SYNC_UPDATE_TIMEOUT_MS = 150`
  releases the window faster than a `pane snapshot` round trip can reliably
  land inside it.
- **`htop`.** Not installed: `command -v htop` exits 1. `btop` was used in its
  place, per the corpus list. `btop` additionally needed
  `env LANG=C.UTF-8 LC_ALL=C.UTF-8 btop --utf-force`; without it btop refuses to
  start with *"ERROR: No UTF-8 locale detected!"*, which is the container's
  locale and not this change.
- **The outer-terminal emit path.** Every pixel in this report was produced by
  `shux-raster`. CLAUDE.md is explicit that shux cannot audit its own emit path
  by rendering it itself, and `make test-gui-terminal` was not run. This change
  emits nothing new to an outer terminal — attach draws no images, work item 6
  is where that lands — so there is no emit path here to photograph, but the
  evidence is subject to the caveat and is recorded as such.

## Cleanup

Three isolated `XDG_RUNTIME_DIR`s were used (`/tmp/kgqa`, `/tmp/kgcfg`,
`/tmp/kgqa2`) plus `/tmp/kgbase` for the base-build A/B. Every daemon pid was
read from `$XDG_RUNTIME_DIR/shux/shux.pid` **before** `shux daemon stop` and
`/proc/<pid>` checked **after**. No `pgrep -f` / `pkill -f` was used anywhere.
Final sweep — every `/proc/*/exe` resolving into `/home/user/shux/target/` or
`/tmp/kg-base/target/` — returned nothing. The base worktree was removed and
`git status --short` is empty apart from this audit's own files.
