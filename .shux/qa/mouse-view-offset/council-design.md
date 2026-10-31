# DootSabha design council — issue #192

Transcribed by the VT QA gate from the council's own JSON output (the raw JSON
carries local checkout paths, so it stays in gitignored scratch, `.shux/out/192/`).

## Design source

The approach (per-pane honest rendering, a cursor-following row offset shared by
content, cursor and mouse) comes from the Sep 2026 attach-resize research verdict,
which named #192 as the mouse half of that plan and was reviewed by codex and grok.

## Round 1 — design challenge (`council.json`)

| | |
|---|---|
| Strategy | `dootsabha council`, config from `~/.config/dootsabha/config.yaml` |
| Providers | agy: ok, codex: ok; chair claude (fallback codex) |
| Duration | 778 s |
| Target | `95346b5`, `0725d9f` on base `6b21101`, before push |
| Verdict | **FIX-FIRST** |

Decisions put to the council:

- **D1** — map input through the live VT's offset, or through the view the
  compositor last shipped to that client.
- **D2** — pin an in-flight drag/release that leaves the grid onto the nearest
  shown cell, rather than dropping it.
- **D3** — test harness grid seed changed from 28 to 27 rows to match its rect.
- **D4** — a fuzz target pulling `shux-ui` + `shux-core` into the fuzz crate.

Outcome that shaped the code under audit: D1 was ruled a correctness defect —
input must map through the frame the client was shown. That is why
`RenderCompositor` now records a `PaneView` per pane per frame (`pane_views()`),
the change this VT audit covers. X10 releases past row 223 and a collapsed
pane's lost release were also raised and fixed in later commits.

## Timing note

This round ran against the first implementation commits (the council started
within a minute of `95346b5`), not before any code existed. The earlier design
source is the research verdict above.
