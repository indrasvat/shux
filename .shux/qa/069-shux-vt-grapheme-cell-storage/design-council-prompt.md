Review this shux task-069 design before implementation. Be concrete and critical.

Context:
- Repo: indrasvat/shux, Rust terminal multiplexer.
- Task: docs/tasks/069-shux-vt-grapheme-cell-storage.md.
- Design: .shux/qa/069-shux-vt-grapheme-cell-storage/DESIGN.md.
- Current failure: crates/shux-vt/src/parser.rs drops unicode-width zero scalars; Cell stores only one char; capture/raster/live render paths read only cell.ch.
- Current shape: Cell has ch, width, style, extended: Option<Arc<ExtendedAttrs>>. ExtendedAttrs currently stores hyperlink and underline attrs.
- Hard constraints:
  - Keep ASCII/common cells compact.
  - size_of::<Cell>() should not increase.
  - ASCII 80x24 + 5K scrollback RSS <= +15%.
  - ASCII capture_text throughput <= +10% slower.
  - No full HarfBuzz/shaping/color emoji/bidi in this task.
  - Existing simple glyph PNG goldens must stay pixel-exact.
  - SOLID QA will fail missing unit/integration/shux automation/visual/pixel/perf evidence.

Evaluate:
1. Is storing a rare grapheme payload inside ExtendedAttrs acceptable, or is a separate content payload worth the cell-size cost?
2. Does the parser plan correctly preserve combining marks, VS16, skin-tone modifiers, ZWJ sequences, and regional-indicator flag pairs without overclaiming full shaping?
3. What are the highest-risk failure modes for cursor movement, wrapping, wide continuations, insert/delete, REP, resize reflow, copy mode, live attach, and raster snapshots?
4. What exact tests/evidence should block the PR if missing?
5. Give a verdict: proceed, proceed with mandatory changes, or stop/re-scope.

Return prioritized findings and mandatory design changes only. Avoid generic Unicode background.
