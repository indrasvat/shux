# Council substitution — `dootsabha` unavailable

`dootsabha` is not installed in this environment. CLAUDE.md *Tooling fallbacks*
requires the step not be skipped:

> Spawn context-appropriate **parallel adversarial agents on disjoint surfaces**
> that drive the real system. Reproduce every finding before believing it; fix
> with a test seen failing first.

Both council steps ran that way. This file is the record the manifest names for
`dootsabha_design` and `dootsabha_implementation`.

## Step 1 — design council, before coding

Two agents on disjoint surfaces, run before any code was written, on the design
for inline-image placement storage (work items 4+5 of
`docs/designs/inline-images.md`). This commit range is the first thing that came
out of them: the eviction-base defect was a design-council finding about where
placements can be anchored, not a defect anyone was looking for.

**Agent A — store and bounds.** Asked where placements live given that
`clone_visible` runs on every snapshot, how absolute-line anchoring arithmetic
works, and which of §5's bounds can be enforced where the code will sit. Found
that `Grid` already carries `evicted`, a monotonic count of lines that have
fallen off the front — so `absolute index = evicted + index in raw` is an anchor
coordinate the grid already maintains, and Zellij's five explicit reflow hooks
are not needed for the scroll case. **Also found that the two viewport-only
clone paths copy `evicted` unchanged**, which is what `f01c954` fixes.

**Agent B — rendering and scaling.** Asked who owns the cell pixel size, which
render paths need carry-over, and whether one PR is right. Findings are recorded
against the render work, not this range; the one that bears on it is that
`Rasterizer::render` takes a `Grid` and nothing else, so anything a placement
needs must survive `clone_visible`.

Reproduced before believing, per CLAUDE.md. The clone-path claim was
re-derived from the source independently of the agent's report, and the
rendering council's headline finding (a placement spilling out of its pane in
`window snapshot`) was re-run through the REAL `shux_ui::compose`,
`pane_viewport` and `compute_rects` rather than the agent's transcription of
them — 13680 stray pixels, 6840 of them on the status bar, matching its figures.

## Step 7 — implementation-diff council, before pushing

Two agents on disjoint surfaces against `9793961..6172c24`. Not scaled down for
a small diff, per CLAUDE.md.

**Agent A — the arithmetic, in every grid state.** Verdict: sound. Could not
break `evicted + scrollback_len()`. Established that `clone_visible`'s
`self.scrollback_len()` and `clone_presented_viewport`'s pre-loop `sb` cannot
diverge (no interior mutability anywhere in `Grid`'s field graph), swept 17
hand-built states + 400 randomised operation sequences + 256 degenerate-geometry
combinations, and argued `saturating_add` is the correct failure mode: at the
ceiling the base under-reports, so an anchor resolves past the end and is
dropped, where wrapping would alias onto a live row.

It also closed a hole in the author's own TDD: reverting BOTH production lines
cannot show both are covered, because the `clone_visible` assertion panics
before the second is reached. Reverting only `clone_presented_viewport`'s line
produced a separate, distinct red. Both lines are independently pinned.

**Finding acted on — P2, `reset_blank` breaks the invariant this diff wrote.**
`reset_blank` discards every line the grid held and left the base untouched: the
same defect class as the clone paths, in the one producer of a `Grid` that is
not a struct literal and so cannot be caught by the compiler. Driven through the
real `ScreenSwap`, an absolute index taken in one alternate-screen session
resolved onto a LIVE row of the next (`Some("Y")`). Fixed in `1ff6e72` with
three tests seen failing first.

Not declinable under *Correctness is never a scope question*, and in scope
because `f01c954` is what made the invariant explicit rather than merely
unstated.

**Finding NOT taken, with reason.** The agent also proposed making
`is_blank_canvas` compare the base, so the recycling branch could not re-open
the defect. Checked for reachability first: `VirtualTerminal` drops the spare
outright when dimensions change (`lib.rs`, `dims_changed`), so a spare can never
be resized, and every other path that moves the base also advances the write
tally. No grid with a moved base reaches that check. Guarding it there would be
unreachable code; the implication is pinned by
`a_grid_that_looks_blank_to_the_recycling_check_counts_from_zero` instead, so a
future change that breaks it goes red.

**Carried forward, not fixed here.** Two pre-existing observations that bear on
the image work rather than on this range: `FrameEnvelope` has no field for the
eviction base, so serialized placements would not round-trip their absolute
anchors; and absolute line identity is not preservable across a reflow in any
scheme, so the anchor-invalidation contract needs stating when placements land.

**Agent B — the consumer surface.** Verdict: no consumer affected. No P1, no P2.

Enumerated the readers by machine rather than by grep: renamed `Grid::evicted()`
and let `cargo check --workspace --all-targets` find every call. Exactly three,
all inside `shux-vt` — `sync.rs:99` and `lib.rs:694`, both reading the LIVE grid,
plus the diff's own test. The field is private to the `grid` module and the
accessor is `pub(crate)`, so nothing outside the crate can reach it by
construction. The author's argument was falsified rather than merely
unchallenged.

No wire or compatibility surface: `Grid` derives only `Debug`, and the capture
schema (`FrameEnvelope`, schema 1) carries `alt_screen, cursor, defaults,
palette_overridden, rows, schema, size` and nothing else — checked against the
frozen fixture. (The RPC wire's `evicted_revision` is the lens checkpoint FIFO,
an unrelated concept.)

Copy mode driven for real: a 31-key script through `handle_key_with_vt` over 17
stages including freeze with history partially and fully evicted, alt-screen
entered and left inside the freeze, and a recycled alt buffer — every cell
fingerprinted with its full `CellStyle`. 1177 lines, byte-identical base vs fix.
Daemon-backed A/B over 6200+ coloured lines (past the 5000 cap, so `evicted > 0`)
across all five capture/snapshot paths: 24/24 artifact pairs identical, 12/12
PNGs opened and pixel-compared at `pixel_diff_ratio` 0.000000.

**The probe was proven able to fail**, which is the part that makes the null
result worth anything: wiring the clone's field into `presented_history_len`
moved 687 diff lines while leaving base identical, and stripping colour while
keeping text identical moved 1342. Sensitive, and not monochrome-blind.

**Finding acted on — P3, the stated reason for leaving `is_blank_canvas` alone
was inexact.** `clear_scrollback` moves the base and bumps no write tally, so
"only moves on a path that also advances the tally, or on a resize" was wrong.
The conclusion survives for a different reason: `clear_scrollback` cannot move a
base FIRST, because a non-empty scrollback implies prior scrolling, which does
bump. Commit message corrected and the state added to the pinning test.

**P3 — a settled capture cannot see a freeze.** `SYNC_UPDATE_TIMEOUT_MS = 150`
is swept by the daemon tick, so any `pane capture` after a `wait-settled` is past
it. The only real-daemon route to a live freeze is a pane re-opening windows
faster than 150 ms; that scene was added and showed 0 torn frames on both trees,
with the torn detector proven to fire on a synthetic half-frame. Recorded so
nobody later "proves" frozen behaviour with a settled capture.

## Follow-up still open at the time of writing

`15fd835` (the `reset_blank` fix) was authored after Agent B started and is not
in its worktree. Unlike the clone fix it changes OBSERVABLE behaviour on the
alternate-screen recycling path, and Agent A measured the reuse/no-reuse bases
diverging (`[0,0,4]` vs `[0,0,0]`) before it. Agent B has been sent back to run
its harness against that commit, including a real vim/nvim workload rather than
synthetic alt-screen sequences.
