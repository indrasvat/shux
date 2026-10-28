# Council record: OSC 7501 status chrome (P2a)

This records what the VT gate checked for DootSabha compliance. Pointers are to tracked files and commits on
`feat/osc7501-status-chrome` at `a4530be`.

## Design council (`dootsabha_design`)

`docs/designs/program-status/PLAN.md` §6.1, "Council outcome (2026-10-27; agy, then codex confirming or refuting each
claim against the code)". All five P2a decisions were accepted with refinements: seam, budget, colours, glyphs, exited
panes, status bar, config, the matrix additions and goldens. This gate checked the refinements that are VT scope:
- one overlay drawn by both renderers;
- the VT-identical grapheme layout;
- theme border colours in snapshots;
- badge glyphs in the primary font;
- pane goldens unchanged.

## Implementation-diff council (`dootsabha_implementation`)

- **What the coordinator reports.** A codex `dootsabha consult` reviewed `1e0fcf3` before any push.
- **Dispositions.** The four findings are fixed in `37fae50`, whose commit message records them:
  - ZWJ sequences and flags split, and graphemes kept past the VT's 32-scalar cap;
  - the status bar measured by width but painted one cell per scalar;
  - `compose()` copying status-bar cells as width 1 without graphemes;
  - config reread across awaits.
- **Raw output.** The raw consult output was not provided to this gate. The disposition is the commit message.
- **Later fix commits.** `9ee66ee`, `bf22e26`, `a6280cc` and `a4530be` came after that review. This gate's four
  independent runs stand in for them: it raised F1, F1b, F4 and F7, and each was fixed with a test seen red first.
  Per CLAUDE.md step 7, the PR should state that.
