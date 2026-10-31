# DootSabha implementation-diff councils — issue #192

Transcribed by the VT QA gate from the councils' own JSON output (raw JSON stays
in gitignored scratch, `.shux/out/192/council{2,3,4,5}.json`, because it carries
local checkout paths). Every round: `dootsabha council`, providers agy + codex
both `ok`, chair claude; all before push.

| Round | Reviewed | Duration | Reviewer verdicts | Synthesis |
|---|---|---|---|---|
| 2 | `6b21101..3f4a10c` | 971 s | FIX-FIRST (agy), FIX-FIRST (codex) | FIX-FIRST: zoom auto-focus broke a rollback test; X10/collapsed-pane release gaps; zoom/copy-mode defect |
| 3 | `3f4a10c..2540e85` | 445 s | SHIP (agy), SHIP (codex) | no confirmed defect in gesture anchor or zoom/focus; one concern: a release of an unheld button released every held button |
| 4 | `2540e85..b76e793` | 290 s | SHIP (agy), FIX-FIRST (codex) | FIX-FIRST: legacy releases carry no button identity; presses inside one gesture lost when it ends |
| 5 | `2540e85..8016fde` | 213 s | SHIP (agy), FIX-FIRST (codex) | not a clean ship: see below |

Together the rounds cover every commit in `6b21101..8016fde`.

## Round 5, stated plainly

The closing synthesis does **not** say "no defect". Both reviewers agree on one
concrete behavior: with the shown slice at grid rows 274–300 and a legacy (X10)
host encoding, a secondary press inside a gesture already underway is pinned to
row 223, the last row X10 can encode — a cell the client is not shown. Codex
calls that FIX-FIRST; agy calls it intended; the chair calls it a contract
decision (every press names a shown cell, versus a gesture never strands a
button) that no coordinate can satisfy on both sides once the slice lies past
row 223.

That is a mouse-encoding contract question for the `shux-tui-qa` gate and the
PR description, not a rendering defect: it changes no cell, cursor or pixel.
The VT gate records it so it is not lost, and does not adjudicate it.
