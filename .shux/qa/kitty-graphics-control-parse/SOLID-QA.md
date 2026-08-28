VERDICT: PASS

# SOLID VT QA — kitty-graphics-control-parse

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| Commit audited | `51ae4c9` |
| Base | `origin/main` `7d1b716` |
| Change | inline-images work item 3 — APC scan, kitty control-block parse, refusals, bounds |
| Design | `docs/designs/inline-images.md` |

The tree was verified clean and at `51ae4c9` at the start of this audit and again
at the end, including after the mutation matrix mutated and restored it.

This is the second QA pass. The first returned `VERDICT: FAIL` at `bc75fc8` on
evidence, never on behaviour. Everything re-verifiable was re-run at `51ae4c9`;
items carried forward are labelled with the SHA at which they were established.

## Stated DoD matrix

Criteria taken from the PR description's verification matrix and from
`docs/designs/inline-images.md` "Decisions this change implements".

| # | Stated criterion | Verdict | Evidence |
|---|---|---|---|
| D1 | native emulation, no passthrough escape | PASS | no passthrough path in diff; `dispatch_graphics` writes only to `GraphicsSink` |
| D11 | refuse `t=f`/`t=t`/`t=s` before the payload is read | PASS | `every_file_backed_transport_is_refused`; mutation row 1 caught |
| D11b | the reply half deferred, deliberately | PASS | `nothing_is_answered`; audit probe: 17 forms → 0 bytes |
| — | nothing emitted on the wire, nothing stored | PASS | `qa_no_graphics_form_is_ever_answered`, `qa_da1_survives_a_graphics_query_at_every_split` |
| — | every render path touched | PASS | capture · glance · pane/window/session snapshot, 0 failures |
| — | config states default · init · maxed · malformed | PASS | 4×5 matrix, 0 failures, PNG content asserted |
| — | cross-path consistency test | PASS | capture == glance in all four states |
| — | `make check` (lint + tests) | PASS | 2234 pass / 2 skipped; clippy + fmt clean |
| — | visual proof, no committed screenshots | PASS | 11 pixel pairs 0/0; no baseline tracked, so no PNG owed |
| — | dootsabha councils (design + implementation) | PASS | `council-substitution.md`; `dootsabha` genuinely absent |
| — | no screenshots committed unless durable baselines | PASS | `screenshots: []`; metrics name scratch-only baselines |

## Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit | 637 pass in `shux-vt` @`51ae4c9` | `qa2-vt.log` |
| Integration | 2234 pass / 2 skipped workspace @`51ae4c9` | `qa2-make-test.log` |
| Raw byte / replay | 5 committed rich-TUI fixtures replayed at recorded 120x36 | `qa2-abrender.log` |
| Shux automation | 80x24 · 120x40 · 200x60, truecolor + indexed + basic on every capture | `qa2-abrender.log`, `qa2-ab200.log` |
| Visual inspection | full-resolution PNGs opened and read | see Screenshot matrix |
| Pixel comparison | 11 pairs, all `status=pass`, exact 0/0 | `metrics2/` |
| Comparator sensitivity | proven able to fail in this pass | see below |
| Mutation matrix | 9/9 production mutations caught by the named test; control green | `qa2-mutmatrix.log` |
| DootSabha design | substituted, recorded, spot-verified | `council-substitution.md` |
| DootSabha diff review | substituted, recorded, spot-verified | `council-substitution.md` |

### Comparator proven able to fail, in this pass

| Probe | Expected | Observed |
|---|---|---|
| different content, same size | fail | `status=fail`, 12625 px, exit 1 |
| size mismatch | fail | `status=fail`, exit 2 |
| HEAD vs byte-stripping mutant build, `apc-80x24` | fail | `status=fail`, 2834 px, exit 1 |
| HEAD vs byte-stripping mutant build, `apc-120x40` | fail | `status=fail`, 2834 px, exit 1 |

The stripping mutant loses `before-apc`, `after-apc-truecolor` and
`abort-esc-then-red` from the grid — the exact defect class this change must not
have — and the APC scenes catch it. The colour and rich-TUI scenes do not catch
it, which the PR states in advance rather than hiding: measured independently
here, all five rich-TUI fixtures contain **zero** complete APC sequences
(1902 / 1010 / 760 / 747 / 87 `ESC` bytes but no `ESC _ … ESC \`), so no cut ever
occurs in those scenes and they can only prove the scanner's *presence* is inert.

## Screenshot matrix

No baseline PNG is tracked in this repo for this change, so per
`.shux/qa/README.md` the pixel-metric JSON stands alone and no `*-actual.png` is
owed. Baselines are renders from the `origin/main` binary, produced in this
audit's scratch.

| Viewport | Workload | Actual | Baseline | Status |
|---|---|---|---|---|
| 80x24 | colour probe (truecolor+indexed+basic, combining, ZWJ, DEC) | `ab/head2/colour-80x24.png` | `ab/main/colour-80x24.png` | pass 0/0 |
| 120x40 | colour probe | `ab/head2/colour-120x40.png` | `ab/main/colour-120x40.png` | pass 0/0 |
| 200x60 | colour probe | `ab/head200/colour-200x60.png` | `ab/main200/colour-200x60.png` | pass 0/0 |
| 80x24 | APC stream (chunked transmit, ESC-abort, unterminated) | `ab/head2/apc-80x24.png` | `ab/main/apc-80x24.png` | pass 0/0 |
| 120x40 | APC stream | `ab/head2/apc-120x40.png` | `ab/main/apc-120x40.png` | pass 0/0 |
| 200x60 | APC stream | `ab/head200/apc-200x60.png` | `ab/main200/apc-200x60.png` | pass 0/0 |
| 120x36 | btop replay | `ab/head2/tui-btop.png` | `ab/main/tui-btop.png` | pass 0/0 |
| 120x36 | lazygit replay | `ab/head2/tui-lazygit.png` | `ab/main/tui-lazygit.png` | pass 0/0 |
| 120x36 | nvim replay | `ab/head2/tui-nvim.png` | `ab/main/tui-nvim.png` | pass 0/0 |
| 120x36 | vicaya replay | `ab/head2/tui-vicaya.png` | `ab/main/tui-vicaya.png` | pass 0/0 |
| 120x36 | vivecaka replay | `ab/head2/tui-vivecaka.png` | `ab/main/tui-vivecaka.png` | pass 0/0 |

Inspected as images, not merely counted. `colour-80x24`: truecolor fg/bg,
indexed 208/27, basic red and green-bg, bold/italic/underline all distinct; the
decomposed combining mark places over `e`; DEC line-drawing renders as box glyphs,
**not** as `lqqqk` letters. `apc-80x24`: exactly the four expected lines, correct
per-line colour, no payload text (`QUJD`/`REVG`/`AAAA`/`broken-unterminated`) on
the grid, no ghost or stale wide cells, cursor block correct. `tui-btop` and
`tui-nvim` render full colour chrome, gauges and box borders with no tofu,
clipping or colour bleed.

## Findings

**P0 — none. P1 — none. P2 — none.**

**P3-1 — the PR description carries a citation the code has already corrected.**
The PR body still says the kitty permission callback is at `graphics.c:628`. The
rustdoc was corrected in `3870b19` to `graphics.c:701-711`, and the correction
matters: the real guard is `transmission_type != 's'`, so `t=s` gets no
permission check at all (`graphics.c:698`). The PR body also predates this pass
and says "9 cases" / "2,235 tests" where the current numbers are 11 and 2,234.
Not a code defect and not gated evidence; fix the description at push so the
durable record matches the code.

**P3-2 — CJK renders as tofu in the colour probe.** `世界` shows replacement
boxes: the bundled raster font has no CJK. Pixel-identical to `origin/main`,
pre-existing, out of this change's scope, and already flagged in the harness
comment. That arm proves wide-cell *accounting*, not glyph rendering.

## Passed evidence

- **Guard question answered: omitting `head` is correct.** `scripts/check-vt-qa.sh`
  requires exactly five top-level manifest keys — `solid_qa_report`,
  `dootsabha_design`, `dootsabha_implementation`, `screenshots`, `pixel_metrics`.
  It explicitly does not require `task` and compares no field against the folder
  name. Extra keys are ignored; a missing `head` is not an error. Adding the SHA
  in the landing commit is right, and guessing it would have been worse.

- **P2-1 fix confirmed, and proven able to fail for the right reason.**
  `nothing_is_answered` is back to `is_empty()` for five bare graphics forms and
  now asserts the combined query+DA1 read as an exact byte equality. Both halves
  were driven red in a private worktree at `51ae4c9`, with the unchanged test:
  emitting a reply at the dispatch seam reds the loop naming the offending
  command; an off-by-one over-consumption (`at = cut.end + 1`) that eats the DA1's
  `ESC` is invisible to the loop and reds only the new equality, `left: []` vs the
  DA1 bytes. An intermediate mutation that appended a reply only when `responses`
  was already non-empty **survived** — correctly, since the APC cut is dispatched
  before the trailing slice runs, so it never fires. That was a bad probe, not a
  gap in the test.

- **The ten-form property re-measured independently, wider.** An audit-written
  probe drove 17 graphics command forms (query, all three file transports, `i`+`I`,
  unknown action, all three animation actions, delete, put, both hostile integer
  shapes, empty control block, bare `G`, doubled comma, unknown transport) through
  a real `VirtualTerminal`: every one produced exactly zero response bytes. A DA1
  sharing the read with a graphics query was answered in full and alone at **all
  54 split points**. Positive control: a bare DA1 *is* answered, so the driver can
  see a reply.

- **All three corrected kitty citations verified against the vendored sources.**
  `graphics.c:698` is `safe_shm_open` (no check); `graphics.c:701` is
  `if (global_state.boss && transmission_type != 's') {` and the block closes at
  `711`; `graphics.c:2569` is `if (g->id && g->image_number)`, ahead of
  `unsigned char action = g->action;` at `2574`, so "before the action switch and
  before any transport work" is accurate; `graphics-protocol.rst:839` is the
  "Specifying both `i` and `I`" note and `:840` is the EINVAL-reply requirement
  shux deliberately declines. All three previously wrong citations are now right.

- **The council record is honest and sufficient.** `dootsabha` is genuinely absent
  from this environment, which is the precondition CLAUDE.md's fallback requires.
  The record names each step, the disjoint surfaces, and what each agent found.
  Three of its claims were spot-checked against the tree rather than taken on
  trust, and all three hold: (a) the refuted 8-bit-ST finding is pinned by
  `only_the_7bit_st_terminates_an_apc_and_vte_agrees`, which carries a real
  positive control and asserts that `AFTER` does *not* resume after `0x9C`/BEL —
  proving vte is still inside the string, so shux and vte genuinely agree and
  "fixing" it would have created the divergence; (b) the ESC counts 1902 / 1010 /
  760 / 747 / 87 match the fixtures byte-for-byte; (c) the "defanged regression
  row" was real — `q=` falls in `parse`'s unknown-key skip arm, so the old
  `q=nope` row was not "also malformed" at all, while the `I=abc` that replaced it
  is. Mutation row 2 now catches, which it could not have before.

- **Mutation matrix: 9 caught, 0 not caught, control green**, each mutation
  required to red the specific test naming it. Run against the shared checkout;
  the tree was verified byte-identical to `51ae4c9` afterwards.

- **Config × render-path matrix at `51ae4c9`:** four config states (default,
  `config init`, feature-maxed, malformed TOML) across capture, glance, and
  pane/window/session snapshot. Content asserted, never exit status: marker before
  the APC, truecolor marker after it, ESC-abort marker, and **zero** APC payload
  bytes on the grid in every cell. PNGs carry real content (498 distinct colours
  per pane render, 1196–1329 per composed frame), and pane renders are identical
  across all four states.

### Carried forward from pass 1 (established at `bc75fc8`, not re-run here)

- ~39.7M compared observable dumps, 5,466 randomised corpus runs, exhaustive 1-
  and 2-way splits, and a differential oracle against a build with no scanner.
- A real `kitten icat` PTY recording replayed through a live pane.

Everything else in this report was measured at `51ae4c9` in this pass.

## Residual risk

- **The refusal guards a consumer that does not exist yet.** Nothing reads a
  payload today, so `t=f` cannot leak a file whatever the parser does. The value
  is that the decision is made and tested before the image store lands. The next
  change must not reintroduce a permissive default; `every_file_backed_transport_is_refused`
  is the tripwire.
- **Seven of eleven pixel cases cannot fail by construction**, because no rich-TUI
  fixture contains a complete APC. They prove the scanner's presence is inert, not
  that its cutting is correct. The APC scenes and the unit-level
  `every_cut_ends_exactly_one_byte_past_the_st` carry that half.
- **A pre-existing `vte` C1 chunk-sensitivity defect is pinned, not fixed**, by
  `c1_controls_are_chunk_sensitive_in_vte`. Reproduced against a build with no
  graphics code, so it is not caused by this change; the test fails the day it is
  fixed and names what to delete.
- **shux cannot audit its own emit path here.** This change emits nothing to an
  outer terminal, so that limit is not exercised — but it becomes live for work
  item 6 (attach re-transmit), where `make test-gui-terminal` is the only witness.
- **Live `lazygit`, `vicaya` and `vivecaka` are not installed** in this
  environment. Only their committed raw replay fixtures were exercised, which is
  what CLAUDE.md requires unless recordings are being refreshed. Refreshing those
  recordings will need a host that has them.

## Cleanup

Zero leaked shux daemons: a `/proc/*/exe` scan matching `*shux*` returned 0
processes after every daemon-backed run. The A/B harness additionally proves this
per-run against its own unique socket path. The audit's private git worktree was
removed and pruned. The mutation matrix restored all three mutated files;
`git status --porcelain` is empty and `git diff HEAD` is empty at `51ae4c9`.
