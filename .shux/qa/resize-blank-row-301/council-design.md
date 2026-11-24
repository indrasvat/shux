# #301 council

The design and the implementation were reviewed together, in one council
(codex + agy) that ran after the fix was written, against `a7ef8fa`. There
was no separate design round. The full record is `council-implementation.json`.

Verdict: FIX. Each finding came from reading the source. Each one was
reproduced at runtime with exact VT bytes before anything changed, and
each fix in `916bdcd` has a test that failed on `a7ef8fa`.

## Required

| Finding | Reproduced | Done |
|---|---|---|
| A deferred wrap survives an erase (`ABCDE CSI J X` at width 5) | yes: X on row 2, and the wrap came back after a 5->4 resize | ED 0, EL 0 and EL 2 clear the deferred wrap and the print intent. EL 0 follows xterm: it erases the last column and cancels the wrap. |
| A clear joins new output onto history (2x5, `ABCDEF CRLF`, clear, `XYZ`, widen) | yes: `ABCDEXYZ` for ED 0, EL 0 from column 0, EL 2, ED 2 and ED 1 | A full erase of the first visible row cuts history's link into it, and the history row keeps its cells. A suffix erase keeps the link, and so does a visible row's incoming link (readline retyping a continuation). |
| The erase marks the whole row dirty and counts the mutation twice | yes: the tally rose by 2 and the dirty range was the whole row | `Grid::erase_to_row_end` changes cells and the wrap flag in one operation: one bump, dirty `4..8` for an erase from column 4 of 8. |
| Tests too loose | n/a | Every cell's background is checked, then the default background after the fill. `%Ns` matches the fill width. Added: a mid-row suffix erase then a widen (`AB` and `FG` stay apart), and an interior ECH that keeps the wrap. |

## Reproduced and fixed (stale links are wrong results)

| Finding | Reproduced | Done |
|---|---|---|
| IL, DL and SD leave links pointing at unrelated rows | yes: IL kept `ABCDE` linked to the inserted blank; DL gave `ABCDEHI`; SD inside a region kept the link | Scrolls cut the links across a region's edges. Rows that move together keep theirs, and a full-screen scroll into history keeps every link. |
| `wrap_to_next_line` sets the link when the cursor cannot advance | yes: on the last row below a scroll region | The link is set only after the cursor has advanced or scrolled, on the row then above it. At a region's bottom margin the link still leads into the new row; a test guards this. |
| ED 1, ED 2 and DECALN leave a stale history link | yes, all three | They cut history's link into the page. |
| Prefix erases on the first visible row | checked | EL 1 and ED 1 on row 0 keep the link: the row still continues after the erased prefix, as it does with EL 1 elsewhere. |

## Policy, unchanged

- **DCH keeps the wrap.** A DCH can make a false join, but the wrap is kept on purpose for wide-glyph gaps, and three tests depend on that. Changing it means deciding the gap handling with it.
- **ED 3 clears the visible page and the scrollback.** kitty does the same; xterm, VTE and Alacritty keep the page. This is a compatibility choice outside #301.
- **ED 1, EL 1 and ECH leave the right edge's wrap.** They do not erase to the end of the row, so the row can still run on into the next; terminals differ here. An interior-ECH test pins it.
