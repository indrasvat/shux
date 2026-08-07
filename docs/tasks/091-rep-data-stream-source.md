# 091 — REP (`CSI b`) repeated the cell left of the cursor instead of the preceding character in the data stream

**Status:** Done
**Priority:** High (conformance; silently drops output a real application asked for)
**Milestone:** M3 polish
**Depends On:** 090 (`e856793`, DECALN) — DECALN homes the cursor, which is how the
column-0 case surfaced
**Touches:** `crates/shux-vt/src/parser.rs`, `crates/shux-vt/src/lib.rs`,
`crates/shux-vt/tests/rep.rs` (new), `crates/shux/tests/rep_pane_e2e.rs` (new),
`.shux/scripts/issue_122_evidence.sh` (new)

---

## Problem (issue #122)

ECMA-48 §8.3.103 defines REP as repeating **"the preceding character in the data
stream"**. shux derived it from the **screen** — `repeat_preceding_char` re-read the
cell to the left of the cursor — which agrees with the specification only while
nothing has moved the cursor since the character was printed.

```
X               print a graphic character
ESC[1;1H        home the cursor
ESC[3b          REP 3   ->  no-op, the screen still reads "X"

X ESC[3b        (no cursor move)  ->  "XXXX", works
```

At column 0 there is no cell to the left, `checked_sub(1)` returns `None`, and the
repeat is dropped without a trace. Reproduced at `e856793` (v0.46.7) through the
shipped binary, not just the parser:

```
$ shux session create demo -d -- sh -c "printf 'X\033[1;1H\033[3b'; sleep 60"
$ shux pane capture -s demo
X
```

Column 0 is the loud case. The quiet one is worse and far more common: **any** cursor
move between the character and the `CSI b` made REP repeat whatever happened to be
parked to the left of the new position — a blank, the continuation half of a wide
character, or an unrelated glyph from an earlier frame. Reproduced for each:

| Stream | shux before | Correct |
|---|---|---|
| `X ESC[1;1H ESC[3b` | `X` | `XXX` |
| `X ESC[1;10H ESC[3b` | `X` + three blanks | `X` + eight blanks + `XXX` |
| `ab CR LF ESC[3b` | second line empty | second line `bbb` |
| `QQQQ` on row 2, `X` on row 1, `ESC[2;5H ESC[3b` | `QQQQQQQ` | `QQQQXXX` |

### Why it matters

Address the line, print one character, repeat it across the width — that is how a
great many applications draw a rule, a progress bar or a box edge, and the cursor
move is the first thing they emit. On shux the bar came out as a single character.
The failure is silent: no error, no fallback, just missing output.

## The fix

The terminal remembers the character when it is printed, instead of trying to find it
again afterwards.

`LastGraphic` holds the exact scalar sequence that was printed — the base scalar plus
the rest of the grapheme cluster if the character grew one — recorded as PRINTED,
after character-set translation. REP replays those scalars through the ordinary
printing path:

```rust
for _ in 0..self.repeat_iterations(count, &source) {
    for ch in source.scalars() {
        self.write_char(ch);
    }
}
```

Replaying through the print path is not an implementation detail, it is the
specification. REP means "n more copies of that character arrived", so everything the
print path does to an arriving character — wrapping at the right margin, scrolling at
the bottom of the region, inserting under IRM, growing a grapheme cluster, taking the
current pen — happens to a repeat identically, and cannot drift out of agreement
later. It is also a total oracle: **REP(n) is byte-identical to the character sent n
more times**, which is what the differential and property tests assert.

Four smaller clauses fall out of that, each of which the old code got wrong:

1. **Only the character is remembered.** Colours, attributes and the hyperlink come
   from the pen current at the `CSI b`, because the pen belongs to the terminal. This
   was already true — the old code cloned the source *cell* but took only its
   character, width and grapheme payload, and wrote through `write_char`, which uses
   the current pen — and it now has regression tests, because the new record stores
   no style at all and so cannot drift back.
2. **Nothing to repeat is a no-op**, not "repeat whatever is next to the cursor". A
   fresh terminal, a stream of pure control sequences, and a terminal just after RIS
   all have no preceding character.
3. **RIS ends the stream.** A terminal that has just been switched on has no preceding
   character. Nothing else clears the record — not a cursor move, not an erase, not a
   resize, not an alternate-screen switch. The data stream is one stream.
4. **REP does not break the grapheme cluster under construction.** `csi_dispatch`
   clears the active grapheme cell for every sequence except SGR, on the grounds that
   a combining mark after a cursor move belongs to the new position. REP is not a
   break — it is more of the same character — so it joins exactly as the character
   arriving again would.

### Bounds (issue #102) held, and tightened

The clamp on REP's work survives the new source and gains a second bound. One
screenful of cells is still the iteration cap — a repeat legitimately wraps onto
following lines, so clamping to the current row would break it, and no real
application exceeds a screen. A multi-scalar cluster costs more per copy, so the
total number of scalars written is capped at two screenfuls as well. Together they
bound the work at two screenfuls however pathological the remembered character is,
where the old code's per-iteration cost was unbounded above by the cluster length.

### A defect the rewrite removed

`write_cell_from_source` placed a repeat using its **base scalar's** width and then
patched the grapheme payload in afterwards. For a cluster whose width comes from a
later scalar — `a` + ZWJ + an emoji is two columns wide though `a` is one — a repeat
landing in the last column produced a cell recording width 1 while holding a
two-column grapheme, with no continuation cell. Every consumer downstream (capture,
resize reflow, the rasterizer) then drew it wrong. Replaying through the print path
removes the second placement entirely.

## Testing matrix

| Level | Where | What |
|---|---|---|
| Unit (VT) | `crates/shux-vt/src/lib.rs` | source survives a cursor move; survives an erase; no-op with nothing to repeat; RIS clears it; repeats take the current pen |
| Integration (VT) | `crates/shux-vt/tests/rep.rs` | 57 cases across 10 groups — the data-stream source, nothing-to-repeat, the pen, character sets, grapheme clusters and wide characters, cursor/wrap/scroll/origin/insert, counts and bounds, the sequence space around `CSI b`, grid invariants (write tally, sync freeze, held clones, dirty regions) |
| Differential | `crates/shux-vt/tests/rep.rs` | the oracle over 8 source shapes × 7 prefixes × 4 counts, and over 23 intervening sequences × 3 sources |
| Property | `crates/shux-vt/tests/rep.rs::properties` | 512 random programs against the same oracle, plus 256 chunked at random byte boundaries |
| End-to-end | `crates/shux/tests/rep_pane_e2e.rs` | real daemon, real PTY, real shell, colour-probed: a rule drawn with REP, the issue's column-0 reproduction, a progress bar redrawn in place, a line-drawing box rule, and a flood that must stay bounded |
| Visual | `.shux/scripts/issue_122_evidence.sh` | five scenes shot through shux's own rasterizer, before and after, with per-scene assertions; the `pen` scene asserts on the canonical cell frame because text capture cannot see a colour |

## Acceptance criteria

- [x] REP repeats the preceding character in the data stream, across any number of
      intervening control sequences.
- [x] Column 0 is not special: a homed cursor repeats the same character any other
      position would.
- [x] With nothing printed yet, or after RIS, REP writes nothing.
- [x] The repeats take the pen (colour, attributes, hyperlink) current at the `CSI b`
      — unchanged behaviour, now pinned.
- [x] The remembered character is the one displayed, after character-set translation.
- [x] Grapheme clusters, wide characters and their continuation cells survive.
- [x] Wrapping, scroll regions, origin mode, insert mode and pending auto-wrap behave
      as they would for the character arriving again.
- [x] `CSI b` with an intermediate or a private marker is not REP.
- [x] The work a single REP can buy stays bounded (issue #102).
- [x] `make check` green; zero leaked daemons.

## Adversarial review — four agents on the real binary, three findings

DootSabha councils (feature protocol steps 1 and 6) are N/A in this environment and were
substituted, on the operator's instruction, with parallel agents that drive the shipped
binary rather than reasoning from source: rich-TUI compatibility, grapheme/wide-cell
invariants, resource bounds, and an A/B regression sweep against `e856793`.

**Fixed — a stray combining mark redefined what REP repeats.**
`remember_graphic_cluster` re-read the grown cluster out of the SCREEN cell that
`append_zero_width_scalar` had chosen, and that function falls back to the cell left of
the cursor when there is no active grapheme cell. The screen-derived reasoning this issue
exists to remove, surviving in the cluster path.

```
ABCZ  ESC[1;2H  U+0301  ESC[3b   ->  ÁÁÁÁ   (B, C and Z destroyed)
                                     expected ÁZZZ
```

A second route needs no cursor move: with auto-wrap off a wide character in the last
column is dropped, which clears the active cell and strands the variation selector
behind it on an earlier cell. Both reproduced, then fixed by extending the record only
when the scalar joined the cell the record already describes. That is xterm's rule —
**REP repeats the last character that occupied at least one column**, together with the
marks that joined it — and it is the one precondition on the oracle besides #124.
Regression tests for both routes, seen failing first.

**Fixed — a 10–24% throughput regression on cluster-heavy text.** Measured independently
by two reviewers. Re-reading the cell allocated a fresh `String` per cluster-growing
scalar, on the hot path for ALL terminal output rather than just REP. Extending the
record is now an O(1) push into a reused buffer. Best-of-7 over 24 MB streams, release:

| 24 MB of | `e856793` | this branch |
|---|---|---|
| ASCII | 32.5 MB/s | 31.9 MB/s |
| wide CJK | 50.0 MB/s | 49.7 MB/s |
| ZWJ emoji | 40.6 MB/s | 40.2 MB/s |
| combining marks | 23.8 MB/s | 22.8 MB/s |

What remains on combining marks (~4%) is the irreducible cost of tracking the cluster,
and it only appears on a stream where every second scalar is a mark.

**Fixed — two tests were green on the pre-fix commit.** Both parked the cursor at column
0, where the old code bailed for its own unrelated reason and looked correct. Both now
sit at a non-zero column and assert on the write tally, which is what sees the old code
cloning a blank and writing it.

**Corrected, not fixed — the pen was never wrong.** The first cut of this task claimed
the old code "cloned the source cell, colour included". It did clone the cell, but took
only its character, width and grapheme payload and wrote through `write_char`, which
uses the current pen. The evidence harness caught it: the `pen` scene passes on both
binaries and its two PNGs are byte-identical. The task file, the commit message and the
`pen` scene's own comment were corrected, and the two pen tests are now documented as
pins on unchanged behaviour rather than proof of the fix.

**Cleared — 15,120-input A/B sweep.** The whole `vt-corpus` rich-TUI and synthetic
fixture set at 5 geometries × 4 chunkings plus 9,500 generated streams, replayed through
both builds and diffed cell for cell. 1,422 differences, every one containing a
REP-shaped `CSI b`; the same inputs with REP stripped gave 0. All six rich TUIs (`vim`,
`nvim`, `htop`, `btop`, `lazygit`, `less`) render byte-identically, including over a
page the repeat command had just filled.

## Reference cross-check: Alacritty

Alacritty `1b2b36a6` builds on the same `vte` crate at the same version (0.15.0) that
shux pins, and since `cb7ad5b7` its ANSI layer *is* `vte::ansi` — so `crates/shux-vt/src/parser.rs`
is a hand-written replacement for the exact module Alacritty uses. The comparison is
direct. Every claim below was read from source and then re-verified by running Alacritty.

**The central question: shux has converged on the reference.** Alacritty has sourced REP
from remembered state (`ProcessorState::preceding_char`, written only in `Perform::print`)
and replayed it through `handler.input()` — the ordinary print path — since `2bfb3f70`
(2017). The screen has never been consulted. The approach this task removed has no
precedent.

Where the two differ substantively, shux is the stricter implementation:

| | Alacritty | shux |
|---|---|---|
| what is remembered | one scalar | the whole grapheme cluster |
| `e` U+0301 then `CSI 3 b` | three more acutes stacked on one `e` | `éééé` |
| RIS clears it | no — `preceding_char` lives in the `Processor`, `reset_state` is on the `Handler` | yes |
| a stray combining mark redefines it | yes | no (xterm's rule) |
| repeat bound | `u16::MAX` per sequence, no screen-relative clamp | one screenful of cells, two screenfuls of scalars |

Alacritty's bound is a deliberate security fix (`a2727d06`, "Fix DoS caused by excessive
CSI parameter values", whose changelog names `CSI Ps b`) but it narrows the counter rather
than clamping the work: measured on the real emulator, `一` followed by 1,000 repeats of
`ESC[65535b` — **8,003 input bytes — stalls it for 11.4 seconds**. shux's clamp bounds the
work per sequence and per scalar, and is strictly stronger.

**One real divergence, left as-is deliberately.** Alacritty stores the character BEFORE
character-set translation and re-maps it through whatever set is active at the `CSI b`;
shux stores it as printed. `ESC(0 q ESC(B ESC[3b` gives Alacritty `─qqq` and shux `────`.
Both readings are literal — Alacritty follows ECMA-48's "the preceding character in the
data stream", shux follows xterm's "the preceding graphic character" — and each is
self-consistent with its own oracle. They agree in every ordering a real application
emits, because the switch back to ASCII comes after the repeat, not before. Recorded here
so a future reader who checks Alacritty first does not mistake it for a defect.

**A caution about the reference's own tests.** Alacritty's only REP test,
`alacritty_terminal/tests/ref/csi_rep/`, is a real zsh recording whose expected grid
contains no output at all — the prompt redraw erases it before the recording ends. Making
`('b', [])` a no-op in a patched copy leaves the test passing. That is the likeliest
reason the cluster, RIS and stray-mark defects above have survived nine years, and it is
the argument for this task's differential oracle over a recorded-golden harness.

## Explicitly not changed

**The iteration clamp diverges from the oracle above one screenful, and stays.** `REP 100`
on a 3x10 grid produces one line of scrollback; 101 literal `X`s produce eight. Raising
the cap to include scrollback capacity would close it and would also let ten bytes write
`scrollback_capacity x cols` cells, which is the amplification issue #102 exists to
prevent. Pinned by `a_repeat_larger_than_one_screenful_scrolls_less_than_the_literal_stream`
so it is a documented choice rather than an untested edge, and stated as a precondition in
the test module's own documentation.

## Found in passing, filed separately

**An incrementally built grapheme cluster is torn in half at the right margin.**
`a` + ZWJ + emoji started in the last column leaves `a` + ZWJ in that column and puts
the emoji alone on the next line, where an atomic wide character wraps whole. A flag
pair splits the same way. This reproduces with no REP anywhere in the stream and
belongs to the grapheme printing path (task 069), not here. REP's side of it is
pinned by `rep_after_a_cluster_torn_by_the_right_margin_repeats_the_surviving_half`
so a fix there shows up as a test change rather than a silent behaviour change, and
the property test documents it as the one precondition on its oracle.

**DEL (0x7F) is stored as a printable cell (issue #127).** `unicode-width` returns `None`
only for C0, DEL and C1, so `write_char`'s `.unwrap_or(1)` is the control-character path.
C1 never reaches `print`, but DEL does: it takes a column, REP repeats it, and
`pane capture`'s text output writes the raw control byte to the operator's terminal
(`--format json` escapes it). Alacritty drops width-`None` scalars outright. No recorded
capture in `.shux/fixtures/vt-corpus/rich-tui/` contains one, so nothing is mis-rendering
today. A one-line change with a print-path blast radius of its own.

**A CSI sequence with too many parameters executes truncated (issue #126).** `vte` raises
an `ignore` flag on a sequence it could not represent; shux binds it to `_ignore` in all
three dispatch handlers and never reads it, so an overflowed sequence runs with whatever
parameters survived. Shared by every sequence, not just this one, and the diff here does
not touch `_ignore`.
