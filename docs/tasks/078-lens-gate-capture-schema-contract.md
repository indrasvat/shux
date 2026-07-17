# Task 078: lens gate — capture schema + frozen contract suite

**Status:** In Progress
**Priority:** High
**Milestone:** M3
**Depends On:** 077
**Quality Gate:** shux-vt-solid-qa
**Touches:** `crates/shux-vt/src/` (new capture types), `crates/shux/tests/lens_gate_*`, `.shux/fixtures/lens-gate/`, `scripts/check-lens-frozen.sh`, `Makefile`, `docs/`

> Part of the `shux lens gate` initiative (verification-as-CI). Design proposal +
> two brutal dootsabha councils (v2, verdict CONVERGED) are recorded in
> `.local/lens-ci-gate-proposal.md` and `.local/council-r{1,2}-*.md`. This is the
> **new golden-gate layer on top of the completed lens (task 077)** — it does not
> change 077's shipped `run/settle/glance/diff` behavior.

## Problem

The gate needs a **stable, versioned, self-describing captured-frame schema** and a
**frozen red contract** before any capture/diff/runner code exists — otherwise
implementation shapes the tests (council #1 BLOCKER). Today shux serializes only
plain text + PNG; the styled cell data in `shux_vt::Cell` (glyph/fg/bg/attrs/width)
is never persisted, and there is no notion of a gate verdict.

## Scope

1. **`FrameEnvelope` + `CanonicalCell` serde types in `shux-vt`.** Carry full
   rendering semantics so a `cell` diff is theme-correct: `schema` version,
   `size{rows,cols}`, `cursor{row,col,visible,shape}` (**R4: no `blinking` — the VT does
   not track it; no `color` — OSC 12 lives in `defaults.cursor`**), `alt_screen`,
   `defaults{fg,bg,cursor}` (OSC 10/11/12), `palette_overridden` (OSC 4 — **R1**), and
   `rows`.
2. **Freeze exactly ONE canonical row-RLE JSON shape** (council #2 residual): a row
   is `{row, runs}` where each run is `[col, content, style?]`; runs are **sorted,
   non-overlapping**, never straddle a wide-glyph boundary; **blank** default cells
   omitted (**but visible default-styled content IS serialized, with no style
   object**); full row count always present; `style` omits default fields
   (skip-if-default). Color is a **semantic variant** — `{"idx":u8}` |
   `{"rgb":[r,g,b]}` | omitted = Default — never hex.
   **R2:** `content` is a string iff every cell is simple (one scalar, width 1), else a
   per-column array. **R3:** a wide continuation is an explicit `""` entry that escapes
   the run's style and decodes to canonical `Cell::wide_continuation()`.
   **R5:** width is NOT re-derived from a pinned `unicode-width` — the geometry is
   self-describing (R3) and the **resolved** `unicode-width` is recorded in **task 080's
   fingerprint sidecar** (not in this envelope), with `stale_golden` catching drift.
3. **Mask sentinel** (council #2 residual): masked/redacted regions serialize as an
   explicit **structural** sentinel run — **R8:** `[col, null, {"mask":true,"cells":n}]`,
   **never glyph text** (a `▮` sentinel could collide with real content containing `▮`) —
   **not** omission, so geometry stays stable and first-run goldens never commit
   timestamps/random IDs.
4. **OSC 4 palette decision** — **DECIDED: option (b)** (R1 above). Option (a) was
   ruled impossible within this task's non-goals: no palette state exists in the VT (the
   SET path discards the colour, `parser.rs:1494`), so (a) means a new 256-entry table +
   re-pointed query path + OSC 104 + re-adjudicating a Class-B limitation — a VT feature
   task, not a schema task. Implement (b) as **one sticky `palette_overridden: bool`**
   on the VT (set in the existing valid-SET arm, exposed via an accessor, must not
   disturb `content_revision`). An active override + indexed colour in a capture →
   `fail` with reason `palette_unportable` — **a diagnostic reason, NOT a distinct
   verdict status** (council #3; the status set below is frozen and closed).
5. **The full frozen red contract suite** — types + signatures so tests
   **compile-but-fail**, covering the **complete, closed** gate status set (council
   #3): `pass/fail/xfail/xpass/missing_golden/xfail_expired/stale_golden/`
   `child_error/settle_never_stable/scenario_error/infra_error/update_refused`
   (`palette_unportable` is a `fail` *reason*, not a status); the exit map
   (0/1/2/3/4/5/6, with `stale_golden`→1); xfail metadata
   (reason/owner/issue/expiry/fingerprint) parsing; mask sentinel;
   redaction-before-serialize ordering; report.json schema.
   **Red-suite harness (council #3 BLOCKER — `make check` must stay green):** the
   frozen contract tests live in a **quarantined lane**, `make
   test-lens-gate-contract`, **excluded from `make check`** while red. A recorded
   failing transcript is committed. Each frozen case is annotated with the task #
   (079–083) that will turn it green, and later tasks flip cases green **without
   editing them** (the freeze guard forbids weakening) — a per-task retirement plan.
   **Mechanism = `#[ignore]` (surveyed decision — a nextest profile does NOT work).**
   There are five entry points and they do not share one filter: `make test` →
   `run-cargo-test.sh` → **`cargo test`** (`Makefile:110`, ignores nextest config);
   `make check` → `test`; pre-push → `make test`; CI → **`cargo nextest run
   --workspace`** (`ci.yml:79,149` — bypasses the Makefile entirely); CI coverage →
   `cargo llvm-cov nextest` (`:178`). So a **Makefile filter leaves CI red** and a
   **nextest `default-filter` leaves `make check` red**. `#[ignore]` is honoured by
   *both* runners with zero config and zero CI edits, and — crucially — ignored tests
   are still **compiled**, which is exactly this task's "compile-but-fail" contract.
   Rejected: a `#[cfg(feature)]` gate (safe from `--all-features`, which appears
   nowhere, but it would exclude the contract from **compilation** and gut the freeze).
   Lane runs via `cargo test -p shux --test lens_gate_contract -- --ignored` (nextest:
   `--run-ignored all`; 0.9.133 supports it — verified). Document why this `#[ignore]`
   is legitimate where `lens_common/mod.rs:729` forbids it: that rule targets T-tier
   skips that would **hide a real gap**; this lane is the inverse — tests that MUST
   fail until a named task retires them, with a committed failing transcript.
6. **Extend `check-lens-frozen.sh`** to freeze `.shux/fixtures/lens-gate/**` and
   `crates/shux/tests/lens_gate_*` under a `GATE-TEST-CHANGE:` trailer.
   **This is NOT additive — there is a prefix collision (surveyed).**
   `FROZEN_RE='^(\.shux/fixtures/lens/|crates/shux/tests/lens_)'`
   (`check-lens-frozen.sh:29`) **already prefix-matches `lens_gate_*`**, so a commit
   touching only gate tests with only a `GATE-TEST-CHANGE:` trailer would **fail the
   lens check** (which demands `LENS-TEST-CHANGE:`). The gate arm must be tested
   **first** and the lens arm **tightened to exclude** `lens_gate_` (POSIX ERE has no
   lookahead → an ordered (key, regex) pair-table, or an explicit `lens_gate_`
   early-continue). `.shux/fixtures/lens/` does not collide with `lens-gate/` (the
   trailing slash saves it) — only the test-path arm does. Preserve `--no-renames`
   everywhere (a `git mv` out of a guarded prefix decomposes to delete+add, so the
   delete is still caught — PR #86 bot review).
   **Companion (surveyed gap):** the freeze is **not enforced in CI at all** —
   `grep -c lens .github/workflows/ci.yml` → **0**. It runs only as a local
   `commit-msg` hook, which `git commit --no-verify` bypasses. The range mode +
   `LENS_FROZEN_BASE` exist but no workflow calls them. For a task whose whole point is
   freezing a contract, wire the range mode into CI (needs `fetch-depth: 0`).
7. **`CanonicalCell` / `CellRef` ownership is by-value / owned** (council #3 —
   remove the "or materialized cache" alternative): the view yields owned cell
   values, never borrows RLE-decode temporaries.

## Non-Goals

- No capture emission (`pane.glance --cells`) — task 080.
- No comparator implementation — task 079.
- No scenario runner, CLI verb, verdict computation, or bless flow.
- No PNG/pixel work.

## Design Review Decisions

**DONE — 2026-07-17.** DootSabha design review (codex + agy) + a tie-break council on the
two split blockers. Records: `.local/078-design-{codex,agy}.md`,
`.local/078-tiebreak-{codex,agy}.md`, `.local/078-grounding-findings.md`,
`.local/078-q6-decisive-finding.md`. Rulings below are **frozen**.

**R1 — OSC 4 → option (b), sticky flag** (unanimous). Option (a) is impossible without new
VT work: the OSC 4 SET path parses the colour and **discards** it
(`parser.rs:1494-1496`), queries answer from the hardcoded `xterm_256_palette`
(`parser.rs:1487`), no palette state exists, OSC 104 is unimplemented, and the whole area
is an adjudicated Class-B limitation (`grid.rs:487`, test `osc_4_palette_no_bump`
`lib.rs:2901`). (a) would mean a 256-entry table + re-pointed query path + OSC 104 +
re-adjudication — a VT feature task, violating 078's non-goals.
→ Add ONE **sticky** `palette_overridden: bool` to the VT, set in the existing
`else if parse_osc_color(..).is_ok()` arm, exposed via an accessor. Additive; must NOT
disturb `content_revision` (the no-bump test must keep passing). Sticky because an override
persists for the VT's life. Serialized as `palette_overridden` in the envelope. A capture
with `palette_overridden == true` **and** indexed colour present → frame is `fail` with
reason `palette_unportable` (a **reason**, never a status — the status set stays closed).

**R2 — run content → hybrid** (tie-break, unanimous). Content is a JSON **string** iff every
cell in the run is simple (exactly one Unicode scalar of display text, `width == 1`);
otherwise a JSON **array**, one entry per grid column in the run.
Rationale: the string form is restricted to all-simple cells, so decoding is `.chars()` —
one char = one cell. **No grapheme segmenter, no `unicode-segmentation` dependency, no
ambiguity.** The round-trip hazard exists only for multi-scalar graphemes and wide glyphs,
which route to the array form. Keeps an 80-column prose line readable instead of 80 quoted
strings — goldens are reviewed by humans *and agents*.
**Canonical rule (validator):** reject array form when every entry is a simple cell; reject
string form for any wide, continuation, or multi-scalar-grapheme case. Exactly one legal
encoding per run.

**R3 — wide continuation → explicit `""`** (tie-break, unanimous).
`[6, ["漢", ""], style]`. Cell count = array length; cell *i* is wide iff entry *i+1* is
`""` — geometry is self-describing and needs no width table at decode.
**The style hazard is real and is closed by rule, not by luck** (see
`.local/078-q6-decisive-finding.md`): every write site assigns the continuation via
`Cell::wide_continuation()` verbatim (`parser.rs:365,441,484,593`; `grid.rs:1003,1117`),
which hardcodes `style: CellStyle::default()`. So a continuation NEVER shares its head's
style. Therefore:
- **Decoder:** `""` **escapes the run's style** and decodes to canonical
  `Cell::wide_continuation()`. It never inherits the run style.
- **Encoder:** a wide head and its continuation stay in the **same run** despite the style
  mismatch (special case), so run-splitting is not style-dependent.
- `""` cannot collide: no real cell has empty display text — `display_text()`
  (`cell.rs:165`) returns `grapheme` (only ever real content: `lib.rs:1419/1452/1459`) else
  `ch.to_string()` (a `char` is never empty). The continuation's own display text is `" "`
  (a space), not `""`.

**R4 — `cursor.blinking` → DROPPED** (unanimous). `Cursor` (`cursor.rs:29`) has no blinking
field; shux-vt does not track blink. An envelope must not promise state the VT lacks — a
golden always recording `blinking:false` is a lie, not a contract. `cursor.color` is **not**
duplicated under `cursor`: OSC 12 state lives in `defaults.cursor`
(`TerminalDefaultColors::cursor`), which is its actual home.
→ `cursor { row, col, visible, shape }` only. **This amends §1 of this task.**

**R5 — `unicode_width_ver` → record resolved, do NOT hard-pin** (unanimous). `unicode-width
= "0.2"` is a caret range in three crates. Hard-pinning `=0.2.x` fights cargo; instead task
080's fingerprint records the **resolved** version from `Cargo.lock` and `stale_golden`
refuses the compare on drift. **This amends §2's "version pinned" wording.**
→ The version lives in **080's fingerprint sidecar, NOT in this envelope** (see R8).

**R6 — style flags → named booleans, skip-if-false** (unanimous). `{"bold":true}`, not a raw
`u8`. A raw `CellFlags(u8)` would freeze private bit positions into the wire format and make
every golden an unreadable magic number.
→ `fg, bg, bold, dim, italic, underline, blink, inverse, hidden, strikethrough`.

**R7 — extended attrs MUST be in run style** (unanimous; caught a real bug in the first draft
— they were omitted entirely, which would have silently dropped them on round-trip).
→ style also carries `hyperlink` (string), `underline_color` (colour), `underline_style`
(enum: `none|single|double|curly|dotted|dashed`). `ExtendedAttrs::grapheme` is NOT a style
field — it is the cell's display text, carried by R2's content.

**R8 — mask → structural, NO glyph text** (design review, unanimous; the tie-break regressed
this to `"▮▮▮"` because it was not asked about masks — **regression rejected**).
→ `[col, null, {"mask": true, "cells": n}]`, `n > 0`, no other style fields. A glyph
sentinel (`▮`) could be confused with real content that legitimately contains that glyph.
Also **rejected**: the tie-break's `"fingerprint"` field inside the envelope — §1 lists no
such field and the sidecar is **task 080's** item 3. 078 must not annex it.

**R9 — schema evolution** (unanimous). `"schema": 1` + `deny_unknown_fields` (fail closed) +
a **typed** unsupported-schema error path now. No migration hook in 078: fail-closed +
`stale_golden` + re-bless is sufficient; a schema-2 migrator can land when schema 2 does.

**R10 — `CellRef` by-value/owned** (council #4). `Grid`/`Row::get` yields `&Cell`
(`grid.rs:258`); `Cell: Clone` but not `Copy` (the `Arc<ExtendedAttrs>` field), so "owned"
= a clone at the view boundary — cheap on the common path (`extended: None`).

**Default-run disambiguation** (codex, design review): a **blank** cell (space, default
style) is **omitted**; **visible** content with default style **is** serialized, as a run
with **no style object** (e.g. `[10, "OK"]`). These are different things and the first draft
conflated them.

### The frozen canonical envelope

```json
{
  "schema": 1,
  "size": { "rows": 3, "cols": 12 },
  "alt_screen": false,
  "defaults": { "fg": [238,238,238], "bg": [12,12,12], "cursor": [255,255,255] },
  "cursor": { "row": 1, "col": 4, "visible": true, "shape": "block" },
  "palette_overridden": false,
  "rows": [
    { "row": 0, "runs": [
      [0, "hello", { "fg": {"idx":2}, "bold": true, "underline": true,
                     "hyperlink": "https://example.invalid/hello",
                     "underline_color": {"rgb":[80,160,255]},
                     "underline_style": "curly" }],
      [6, ["漢", ""], { "fg": {"rgb":[255,210,120]}, "italic": true }],
      [8, ["é"], { "fg": {"idx":4} }],
      [10, "OK"]
    ]},
    { "row": 1, "runs": [ [2, null, { "mask": true, "cells": 3 }] ] },
    { "row": 2, "runs": [] }
  ]
}
```

Colour is `{"idx":u8}` | `{"rgb":[r,g,b]}` | omitted = `Default` — **never hex**. `rows`
always has exactly `size.rows` entries (empty `runs` allowed).

### Validator assertions (frozen)

1. Runs sorted by `col`, non-overlapping, in bounds: `col + Σ(widths) <= size.cols`.
2. No run starts inside a prior wide cell's continuation column.
3. `""` is reserved **exclusively** for a wide continuation: array-form only, never first,
   never after another `""`, only immediately after an entry that decodes to width 2.
4. Real cell display text is non-empty; no entry has display width 0 (except `""`) or > 2.
5. Wide head + its continuation are adjacent **within one run**.
6. String form iff all cells simple; array form otherwise (exactly one legal encoding).
7. Mask runs: `content == null`, `cells > 0`, `mask: true`, no other style field.
8. Unknown fields → fail closed. Unsupported `schema` → typed error, not a panic.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 serde | `FrameEnvelope`/`CanonicalCell` round-trip is lossless and byte-stable; pretty output is deterministic. |
| L1 canonical | Row-RLE validator rejects overlapping runs, unsorted runs, wide-boundary straddles, wrong row count, and non-canonical default encoding. |
| L1 schema | Schema-version compat: an older `schema` value is rejected or migrated deterministically; unknown fields fail closed. |
| L1 semantics | Color variant preserved (`idx` vs `rgb` vs Default never collapse); OSC defaults + cursor semantics + palette round-trip. |
| L1 mask | Masked region serializes as sentinel with stable geometry; a masked timestamp cell never appears in the golden. |
| L1 contract (RED) | The full **closed** status set (incl. `stale_golden`) + exit map + xfail/report contract compiles and **fails** in the quarantined lane — the frozen red suite; a failing transcript is committed. |
| L1 status set | `palette_unportable` is representable only as a `fail` reason, not a verdict status; the status enum is closed (no open-ended variants). |
| L1 CellRef ownership (council #4) | The contract layer proves cells are yielded **owned/by-value**: a decoded cell outlives the RLE-decode temporary it came from (a borrowing signature would fail to compile — asserted via a compile-fail/trybuild case or an outlives test), and no `CellRef` API returns a reference tied to a decode buffer. |
| L2 quarantine | `make check` is **green** with the red contract lane excluded; `make test-lens-gate-contract` shows the expected failures; each frozen case names its retiring task (079–083). |
| L2 governance | `check-lens-frozen.sh` rejects a diff touching `.shux/fixtures/lens-gate/**` or `lens_gate_*` without the trailer; accepts with it. |
| L2 governance — collision (surveyed) | A commit touching **only** `lens_gate_*` with **only** a `GATE-TEST-CHANGE:` trailer **passes** (proves the lens arm was tightened to exclude `lens_gate_` and the gate arm is tested first); a commit touching **only** `lens_*` (non-gate) with only `GATE-TEST-CHANGE:` still **fails**; a commit touching **both** needs **both** trailers. A `git mv` of a frozen file out of a guarded prefix is still caught (`--no-renames` preserved). |
| L2 freeze has CI teeth | The frozen guard's range mode runs in CI (`fetch-depth: 0`) and fails a pushed branch that weakens a frozen path without the trailer — proving the freeze survives `git commit --no-verify`. |
| L1 R1 palette flag | A valid OSC 4 SET sets sticky `palette_overridden`; it stays set after a later non-OSC-4 write; it does **not** advance `content_revision` (the adjudicated `osc_4_palette_no_bump` invariant still holds); a capture serializes the flag. |
| L1 R3 continuation | A **styled** wide glyph round-trips exactly: `""` decodes to canonical `Cell::wide_continuation()` with **default** style (NOT the run's style) — the regression this rule exists to prevent. Head+continuation stay in one run. |
| L1 R2 canonicality | String form iff all cells simple; the validator **rejects** array form where the string form is legal, and **rejects** string form for wide/continuation/multi-scalar cases — exactly one legal encoding per run. |
| L1 R7 extended attrs | `hyperlink`, `underline_color`, `underline_style` survive round-trip (the first-draft schema dropped them silently). |
| L1 R8 mask structural | A mask run is `[col, null, {"mask":true,"cells":n}]`; a golden containing a literal `▮` in **real content** is never confused with a mask. |
| L1 fuzz | Property test: arbitrary grids serialize→deserialize→equal; malformed captures fail closed; Unicode normalization stable. |

## Acceptance Criteria

- [ ] `FrameEnvelope`/`CanonicalCell` exist in `shux-vt` with a pinned `schema` version and full rendering-semantics fields.
- [ ] Exactly one canonical row-RLE shape is frozen with a validator and red tests.
- [ ] Mask regions serialize as a stable structural sentinel.
- [ ] The OSC 4 palette decision is recorded and red-tested.
- [ ] The complete gate status/exit/xfail/report contract is captured as a frozen, compiling-but-failing red suite.
- [ ] The frozen-path guard covers the gate fixtures + test paths.
- [ ] No capture emission, comparator, runner, or verdict code is added.

## Definition of Done

- [ ] DootSabha design review findings incorporated before coding (OSC 4, canonical shape, `CellRef` rule).
- [ ] Red contract suite committed first and demonstrably failing.
- [ ] L1/L2 tests pass (except the intentionally-red contract suite, which is frozen).
- [ ] `make check` and `make check-lens-frozen` pass.
- [ ] `shux-vt-solid-qa` gate reports `VERDICT: PASS` (schema/serialization touches VT); evidence under `.shux/qa/078-*/`.
- [ ] Implementation-diff DootSabha convergence review is clean or all findings addressed.
- [ ] `docs/PROGRESS.md` and this task updated; learnings appended to `docs/agents/learnings.md`.
