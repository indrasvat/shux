# Task 069 Design: Grapheme-Aware Cell Storage

## Goal

Preserve multi-codepoint terminal cell content in `shux-vt` so later text
capture, live attach rendering, and PNG snapshot rendering are not forced to
guess after data has already been discarded.

This task is a storage and propagation improvement. It is not a full Unicode
shaping engine.

## Non-Goals

- No HarfBuzz, swash, CoreText, Pango, or full shaping integration.
- No color emoji.
- No bidi or RTL layout.
- No claim that every composed emoji renders as one perfect glyph in PNG.

## Current Failure

`Cell` stores only `ch: char`. `VtHandler::write_char` drops all scalar values
whose `unicode-width` is zero. That destroys common sequences before the
rasterizer or capture path can make an intentional decision:

- `e` + U+0301 combining acute accent
- U+FE0F variation selector 16
- Fitzpatrick skin-tone modifiers
- ZWJ emoji sequences
- regional-indicator flag pairs

`capture_text()` currently pushes `cell.ch`, copy mode reads `cell.ch`, live
attach prints `RenderCell.ch`, and `shux-raster` draws one `char`.

## Proposed Representation

Keep the common cell layout unchanged:

- `Cell.ch` remains the scalar fast path for ASCII and simple Unicode.
- `Cell.width` remains the terminal column width.
- `Cell.extended` remains `None` for normal cells.
- Add `grapheme: Option<String>` to `ExtendedAttrs`, because `extended` is
  already the rare heap-allocated escape hatch. This avoids adding another word
  to every `Cell`.

Add small APIs on `Cell`:

- `display_text(&self) -> Cow<'_, str>` returns the grapheme payload if present,
  otherwise the scalar `ch`.
- `set_grapheme_payload(String)` stores a payload only when it differs from the
  scalar fast path.
- `append_grapheme_scalar(char)` promotes a cell to extended storage and
  appends a combining/variation/joiner scalar.
- `has_grapheme_payload()` for tests and render-path decisions.

This deliberately treats a complex grapheme as cell content, not style. The
implementation uses `ExtendedAttrs` only to preserve compact common cells.
Grapheme payloads are cell-local:

- `cursor.extended` never stores a grapheme payload.
- Writing a new cell clones cursor hyperlink/underline attrs with
  `grapheme = None`.
- Appending a grapheme uses `Arc::make_mut` on the target cell so cells sharing
  the same hyperlink/style `Arc` do not inherit or mutate each other's text
  payload.

## Parser Rules

The parser receives Unicode scalar values one at a time from `vte::Parser`.
It will not attempt full grapheme segmentation. It will preserve the practical
terminal cases that are currently lost:

1. Normal width-1 or width-2 scalar: write a normal cell, preserving the current
   style and extended attributes.
2. Zero-width scalar after a printable cell: append it to the preceding cell's
   grapheme payload.
3. Zero-width scalar at the start of a row with no previous printable cell:
   ignore it, matching terminal behavior where a combining mark with no base
   cannot occupy a cell.
4. ZWJ sequences: since U+200D is zero-width, it appends to the previous cell;
   the next emoji scalar should also append to that same in-progress grapheme
   instead of creating a new cell.
5. Regional-indicator flags: if a regional indicator arrives immediately after
   a previous regional indicator cell, merge it into that previous cell and set
   the cell width to 2 with a continuation cell when space allows.

The parser needs a narrow "active grapheme cell" cursor anchor so a zero-width
joiner can cause the next scalar to append to the same cell. The anchor is
cleared by cursor movement, line movement, erase, insert/delete, SGR-only
changes that do not write text, and any non-join sequence printable write.

Required anchor contract:

- A zero-width scalar after a final-column base attaches to that final cell and
  preserves pending wrap for the next printable scalar.
- A zero-width scalar after a width-2 head attaches to the wide head, never the
  continuation cell.
- Cursor movement, BS, CR, LF, TAB, CUP/HVP/CHA/VPA, erase, insert/delete, and
  scroll operations clear the active anchor before they mutate position/grid
  state.
- SGR does not by itself clear the active anchor; styling a subsequent combining
  mark should remain possible.
- REP repeats the full cell display payload, not only `Cell.ch`.
- Regional-indicator pairs become a width-2 cell with a continuation when the
  pair can occupy the row; final-column/no-wrap edge cases are conservative and
  must keep wide-cell invariants.

## Render Path Propagation

All user-visible text paths must prefer `Cell::display_text()`:

- `VirtualTerminal::capture_text`
- copy-mode row extraction and selection extraction
- copy-mode overlay writes
- live attach `RenderCell` output
- `shux-raster`
- status/debug helpers that serialize visible cells

For `shux-ui::RenderCell`, add a rare extended text payload without changing
the existing simple constructor behavior. The live renderer can print the full
string for non-continuation cells. Terminals that support shaping/emoji can then
do better than the PNG rasterizer.

For `shux-raster`, draw composed payloads conservatively:

- Combining-mark sequences: render each scalar at the same cell origin where
  possible so accents are at least preserved visually.
- ZWJ, VS16, skin-tone, and flag-pair payloads: attempt the full string only if
  a future renderer supports it; with `fontdue`, render the first visible base
  scalar plus documented fallback. Capture remains lossless.
- The current `fontdue` fallback renders scalar glyphs inside the same cell box;
  it must not spill into adjacent cells or panic on unsupported scalars.
- Existing simple glyph PNG goldens must remain exact.

## Performance And Memory Plan

Baseline must be taken before implementation changes:

- 80x24 viewport with 5K scrollback, ASCII-only.
- capture throughput over repeated `capture_text(None)`.
- process resident memory after filling the grid and scrollback.
- `std::mem::size_of::<Cell>()`.

Budgets:

- `size_of::<Cell>()` must remain unchanged.
- ASCII 5K-scrollback RSS may increase by at most 15%.
- ASCII `capture_text()` throughput may slow by at most 10%.

If a proposed implementation increases `Cell` size, it fails the design before
coding.

## Test Plan

- Unit: `e\u{0301}` captured exactly as `e\u{0301}`.
- Unit: VS16, skin tone, ZWJ sequence, and flag pair are preserved in cell
  payloads.
- Unit: ASCII cells keep `extended == None`; `size_of::<Cell>()` unchanged.
- Unit: task 068 wide-cell invariant helper covers grapheme heads and
  continuations.
- Integration: `pane.capture` returns the full Unicode stress content.
- Raster: existing simple PNG goldens stay exact.
- Shux automation: stress pane at 80x24, 120x40, 200x60 with text capture and
  PNG snapshots.
- Pixel: compare unchanged simple cases at zero tolerance; compare approved
  grapheme stress baselines only after design approval.
- QA: `shux-vt-solid-qa` must return `VERDICT: PASS`.

## Open Questions For Council

1. Is reusing `ExtendedAttrs` acceptable for rare grapheme storage, or should
   this task pay the memory cost for a separate content payload field?
2. Is the proposed ZWJ active-anchor enough for practical TUIs, or should task
   069 explicitly defer ZWJ emoji cluster merging and preserve only the joiner
   itself?
3. Should regional-indicator flag-pair merging be width-2 and overwrite the
   next cell, or should it remain two width-1 cells with payload preservation?
4. Which render-path limitations must be documented as accepted degradation
   for `fontdue` before SOLID QA can pass?
