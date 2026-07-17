# Task 078: lens gate — capture schema + frozen contract suite

**Status:** Not Started
**Priority:** High
**Milestone:** M2
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
   `size{rows,cols}`, `cursor{row,col,visible,shape,blinking,color}`, `alt_screen`,
   `defaults{fg,bg,cursor}` (OSC 10/11/12), `palette` (OSC 4 — see decision below),
   and `rows`.
2. **Freeze exactly ONE canonical row-RLE JSON shape** (council #2 residual): a row
   is `{row, runs}` where each run is `[col, text, style?]`; runs are **sorted,
   non-overlapping**, never straddle a wide-glyph boundary; default runs omitted;
   full row count always present; `style` omits default fields (skip-if-default).
   Color is a **semantic variant** — `{"idx":u8}` | `{"rgb":[r,g,b]}` | omitted =
   Default — never hex. Width derived from `unicode-width` (version pinned).
3. **Mask sentinel** (council #2 residual): masked/redacted regions serialize as an
   explicit structural sentinel run (e.g. `[col, "▮"*n, {"mask":true}]`), **not**
   omission, so geometry stays stable and first-run goldens never commit
   timestamps/random IDs.
4. **OSC 4 palette decision** (the one architecture footnote): shux-vt currently
   treats OSC 4 palette redefinition as a Class-B limitation and answers palette
   queries from standard xterm. This task DECIDES and red-tests one of:
   (a) capture real palette state in the envelope and compare/hash it, or
   (b) when an OSC 4 override is active during capture, treat indexed-color captures
   as non-portable and emit `palette_unportable` **as a diagnostic reason on a
   `fail` frame** (council #3 — NOT a distinct verdict status; the status set below
   is frozen and closed). The decision is made in dootsabha design review and
   recorded here before coding.
5. **The full frozen red contract suite** — types + signatures so tests
   **compile-but-fail**, covering the **complete, closed** gate status set (council
   #3): `pass/fail/xfail/xpass/missing_golden/xfail_expired/stale_golden/`
   `child_error/settle_never_stable/scenario_error/infra_error/update_refused`
   (`palette_unportable` is a `fail` *reason*, not a status); the exit map
   (0/1/2/3/4/5/6, with `stale_golden`→1); xfail metadata
   (reason/owner/issue/expiry/fingerprint) parsing; mask sentinel;
   redaction-before-serialize ordering; report.json schema.
   **Red-suite harness (council #3 BLOCKER — `make check` must stay green):** the
   frozen contract tests live in a **quarantined lane** (a dedicated nextest
   profile / `make test-lens-gate-contract` target) that is **excluded from `make
   check`** while red. A recorded failing transcript is committed. Each frozen case
   is annotated with the task # (079–083) that will turn it green, and later tasks
   flip cases green **without editing them** (the freeze guard forbids weakening) —
   a per-task retirement plan.
6. **Extend `check-lens-frozen.sh`** to freeze `.shux/fixtures/lens-gate/**` and
   `crates/shux/tests/lens_gate_*` under a `GATE-TEST-CHANGE:` trailer (or reuse the
   `LENS-TEST-CHANGE` regime), so the contract cannot be silently weakened.
7. **`CanonicalCell` / `CellRef` ownership is by-value / owned** (council #3 —
   remove the "or materialized cache" alternative): the view yields owned cell
   values, never borrows RLE-decode temporaries.

## Non-Goals

- No capture emission (`pane.glance --cells`) — task 080.
- No comparator implementation — task 079.
- No scenario runner, CLI verb, verdict computation, or bless flow.
- No PNG/pixel work.

## Design Review Decisions

DootSabha design review (codex + agy, config-file only) MUST run on this task's
schema + contract before coding, and record: the OSC 4 decision (4a vs 4b, with
`palette_unportable` as a `fail` reason not a status), the final canonical row-RLE
shape, the closed status set + exit map, and confirmation that `CellRef` is
literally by-value/owned.

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
| L2 quarantine | `make check` is **green** with the red contract lane excluded; `make test-lens-gate-contract` shows the expected failures; each frozen case names its retiring task (079–083). |
| L2 governance | `check-lens-frozen.sh` rejects a diff touching `.shux/fixtures/lens-gate/**` or `lens_gate_*` without the trailer; accepts with it. |
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
