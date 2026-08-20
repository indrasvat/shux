# Adversarial review — protocol semantics and diagnosis

Second cold-context agent, disjoint surface (see `adversarial-review-apc.md` for
why this substitutes for `dootsabha`).

## Charter
Re-derive the diagnosis independently, and attack the kitty-graphics protocol
semantics, image store, placements and query/response contract.

## Verdict delivered
**BLOCKED on the graphics core** — five P0s. It also refuted a headline claim of
the diagnosis, which is what this scope acts on.

## Findings that changed this diff

**The diagnosis was incomplete, and the missing half is the reported symptom.**
shux leaked the outer emulator's identity into every pane. Reproduced as an A/B
against a build of the base commit, same command, only the shux binary differing:

| pane env | before | after |
|---|---|---|
| sterile | `This terminal cannot show images…`, exit 1 | unchanged |
| `KITTY_WINDOW_ID` in shux's own environment | **nothing at all — no message, no exit** | `This terminal cannot show images…`, exit 1 |

terminal-browser maps an unanswered graphics probe to `"supported"` whenever it
*recognises* the terminal, so a leaked `KITTY_WINDOW_ID` skips the gate entirely
and streams images into a pane that discards them. Fixed in the preceding commit.

**A claim in the plan was simply wrong.** The plan asserted terminal-browser
renders "1920×1280 regardless of pane size". False: the fallback is a *cell*
size, `(16, 32)` at `engine/crates/pixel-core/src/engine/mod.rs:347`, and the
canvas is `cols*cw x rows*ch`. The recorded pane was 120x40 → 120·16 = 1920,
40·32 = 1280. The frame was always pane-derived. Verified by experiment:
answering `CSI 16t` with a 10x20 cell produced exactly 1200x800.

## Findings recorded for the graphics core (not in this diff)
Carried forward, not lost: placements must sit behind `sync::Presented` or every
frame tears under `?2026h`; a zip-bomb guard derived from the declared `s`/`v` is
validated against attacker input; terminal-browser targets
`display.displayFrequency` (default 60), not the 0.4 fps of a static page, and
decode would land under the daemon-wide pane-IO lock; `q>=1` suppresses query OKs
(kitty `graphics.c:866-871`) so "a query is always answered" is false; answering
`OK` to `a=q,t=f` would make terminal-browser send filenames instead of pixels
and go permanently blank; `ESC c`, `CSI 2J` and entering the alt screen must
clear images.

**Sequencing consequence adopted now:** answering the graphics query with `OK`
before shux can actually draw would turn today's honest error back into a silent
blank screen. The query answer and the renderer must land together, so this diff
answers nothing.
