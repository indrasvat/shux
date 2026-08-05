# 089 — DEC 2026 synchronized output clones the whole grid, scrollback included, on every freeze

**Status:** Done
**Priority:** High (security — pane-driven DoS of the daemon-wide pane-IO lock; issue #115)
**Milestone:** M3 polish
**Depends On:** 106 fix (`daab8a4`, alternate-screen buffer reuse) — same lock, same shape
**Touches:** `crates/shux-vt/src/sync.rs` (new), `crates/shux-vt/src/parser.rs`,
`crates/shux-vt/src/lib.rs`, `crates/shux-vt/src/grid.rs`, `crates/shux/src/main.rs`,
`crates/shux-vt/tests/sync_output_bounds.rs` (new),
`crates/shux-vt/tests/sync_output_differential.rs` (new),
`crates/shux-vt/examples/alt_screen_churn.rs`,
`.shux/scripts/sync_output_dos_check.sh` (new)

---

## Problem (issue #115)

`CSI ?2026h` — "synchronized output", the sequence `vim`, `nvim`, `lazygit` and `btop`
wrap their redraws in — froze the presented frame by copying the live grid:

```rust
// crates/shux-vt/src/parser.rs, private_mode(2026, true)
let mut presented_grid = self.grid.clone();
```

`Grid::clone` copied `raw` in full: visible rows **and scrollback**. The cost of one
freeze scaled with retained history, not with screen size, and it was paid inside the
daemon-wide `PaneIoState` mutex — so a pane that emits sixteen bytes bills every other
pane in every other session.

### Measured at `6236f25` (0.46.6), `cargo run --release --example alt_screen_churn -p shux-vt`

| sequence | bytes | alloc bytes / toggle | amplification | toggles/sec |
|---|---|---|---|---|
| `ESC[?1049h ESC[?1049l` (after #106) | 16 | 212 | 13x | 2,384,950 |
| `ESC[?2026h ESC[?2026l` | 16 | **28,967,604** | **1,810,475x** | **35** |
| one printable char (baseline) | 1 | 18 | 18x | 9,429,777 |

35 toggles/sec is ~29 ms of lock per 16 bytes of pane output — **78x more allocator
traffic per toggle than the bug #106 fixed**.

## Why #106's fix does not transfer

A retired alternate-screen buffer can be recycled: it is screen-sized, has no scrollback,
and is blank on reuse. None of that holds here. Three approaches were measured or
analysed in the issue and rejected:

1. **Recycle the presentation buffer.** Kills the allocation but not the `memcpy`, and
   retains a full grid-plus-scrollback copy per pane for ever.
2. **Clear the recycled buffer per `process()` batch.** Cuts 512 clones per 8 KB read to
   1, but 1 clone is still 29 ms of lock per 8 KB.
3. **Snapshot only the visible viewport.** Cheap and correct for the presented frame,
   but `VirtualTerminal::grid()` returns the frozen grid and copy mode reads
   `total_lines()` / `scrollback_row()` through it — a pane holding `?2026h` open would
   show no scrollback.

## The fix

Two independent halves, because the attack has two shapes.

### 1. Freeze lazily, and make the hook impossible to forget

`?2026h` records that a freeze is wanted. The copy is taken by the first write that would
change the presented frame; `?2026l` on a frame that was never written discards a freeze
that was never taken. Readers cannot observe mid-batch state (`process()` holds
`&mut self`), so this is identical for every observer.

The dangerous version of this is a hand-maintained list of "places that mutate the
presented frame": miss one and a synchronized redraw tears in every rich TUI, silently.
So it is not a list. Each component of the presented frame — grid, cursor, title, dynamic
default colours — is handed to the parser wrapped in `sync::Presented`, which hands out a
shared reference for free and takes the snapshot on the way to handing out a mutable one.
`self.grid.rows()` still borrows immutably and still costs nothing; `self.grid.set_cell()`
still borrows mutably and now freezes first. **A future mutation path cannot forget to
freeze, because there is no way to reach the mutable state except through the freeze.**

It is precise in the other direction too, which the issue called out as the trap: a
sequence that parses but changes nothing presented — `ESC[6n`, a mouse-mode toggle, an
unhandled private mode — never reaches `DerefMut` and cannot be used to re-arm the copy.

Each component freezes independently, and that is still coherent: a component is
snapshotted on its FIRST change after `?2026h` and did not change before that, so every
slot holds its `?2026h` value whether it was filled at `?2026h` or long after.

The alternate-screen flag is the one component not behind a `Presented`, because it lives
inside `TerminalModes` next to a dozen fields that are not presented state — including
synchronized output itself, so wrapping the struct would freeze on the sequence that arms
the freeze. It is frozen by `VtHandler::set_alternate_screen`, its only writer.

### 2. Freeze the viewport, not the pane

A frozen frame that holds the whole grid is expensive twice over, and adversarial
review measured both halves:

- **Taking it** costs one pointer per retained line — 5,000 of them on a used pane —
  for every window, and a pane opens them as fast as it can write sixteen bytes.
  Measured: 87 KB per `ESC[?2026h a ESC[?2026l`, and a **51x** victim-latency
  regression end to end.
- **Holding it** costs more. Every line the frozen frame references is a line the live
  grid can no longer recycle as it scrolls, so it must allocate a replacement. A pane
  that scrolled its whole history inside one window paid **29 MB** for 416 bytes —
  the original defect's magnitude, restored at a slightly higher price.

Neither applies to the viewport: it is a fixed number of rows, and it is the only part
of the grid the mode actually promises to hold still. So `Grid::clone_presented_viewport`
copies the visible rows and nothing else, and history — which is not part of the
presented FRAME — is read live through `VirtualTerminal::presented_row`, which shifts
its indices by whatever has been evicted since the freeze (`Grid::evicted`).

Copy mode's coordinate space moves to `presented_total_lines()` / `presented_row()`
accordingly; that is the whole reader change, because every history read already
funnelled through `Grid::row(abs)` + `total_lines()`.

### 3. Copy-on-write rows

`Row.cells` becomes `Arc<Vec<Cell>>`, unshared on write through `Row::cells_mut`
(`Arc::make_mut`). Copying a viewport is then a walk of row pointers, and a row that is
never written after a copy is never copied at all. The uniqueness check on the write
path is two relaxed atomic loads when the row is unshared, which is the hot path.

This is also what bounds the "hold" cost above: only the rows the window actually
rewrites are copied, at most one screen per window.

### 4. A window cannot be held open for ever

None of the above helps a pane that opens a window and never closes it: an application
killed mid-redraw froze its pane permanently and pinned a copy of that frame in daemon
memory. Two bounds, on independent axes:

- `SYNC_UPDATE_TIMEOUT_MS = 150` — the value the ecosystem settled on (Alacritty's
  `SYNC_UPDATE_TIMEOUT`). Measured against `btop`, the one installed application that
  actually drives mode 2026: it holds a window for **0–6.3 ms**, so the deadline is
  24–1500x a real full-screen redraw.
- `SYNC_UPDATE_MAX_BYTES = 2 MiB` of pane output absorbed by one window (Alacritty's
  `SYNC_BUFFER_SIZE`), so the clock is not the only thing trusted.

The VT enforces both on the way into every batch, which covers every pane still
producing output — including every abusive one, since abuse means output. The daemon's
existing 1 s timeout task sweeps `release_expired_sync()` for the pane that has gone
silent, publishing a revision when a frame is revealed.

### 5. A resize releases the window

Found by adversarial review, and it removes a pre-existing defect rather than adding a
rule. The frozen frame was reflowed on resize, which is wrong twice: it holds no history
to rewrap against, so a widening resize cannot pull soft-wrapped lines back the way the
live grid does; and on the **alternate screen** the live grid is canvas-resized, never
reflowed, so a pane resized while `vim` or `lazygit` held a window open presented
rewrapped content the application had never drawn. That second one is present at
`6236f25` too.

The frame an application asked shux to hold still was drawn for a geometry that no
longer exists. Releasing costs one torn frame where a repaint is already on its way
(`SIGWINCH`), and it is not an amplification route: resizes come from the daemon and no
pane can emit one. A resize to the size the pane already has is not a resize and leaves
the window alone.

### Also fixed on the way

- A window title set inside a window leaked into the frozen frame when the pane had no
  title at freeze time — `title()` fell through to the live value on `None`.
- `#[derive(Default)]` on `Row` produced a zero-column row that no `Grid` invariant
  permits. Removed.

## Measured result

`cargo run --release --example alt_screen_churn -p shux-vt`, 240x64 pane, 5000 lines:

| sequence | before | after |
|---|---|---|
| `ESC[?2026h ESC[?2026l` | 28,967,604 B/toggle, 35/sec | **212 B/toggle, 3,010,842/sec** |
| `ESC[?2026h a ESC[?2026l` | 28,976,894 B, 61/sec | **8,636 B, 456,055/sec** |
| `ESC[?2026h ESC[2J ESC[?2026l` | 28,967,710 B, 35/sec | **374,142 B, 13,221/sec** |
| `ESC[?2026h` + 80x`ESC[64S` + `ESC[?2026l` | 29,340,980 B, 59/sec | **382,516 B, 132/sec** |
| the same scroll, no window | 8,692 B, 234/sec | 8,692 B, 170/sec |
| one printable char (baseline) | 18 B, 8.8M/sec | 18 B, 8.8M/sec |

212 bytes is what parsing the two escape sequences costs, and it is now identical at
24x80 and 240x64 and identical with an empty scrollback and a full one.

End to end (`.shux/scripts/sync_output_dos_check.sh`, six attacker panes, 240x64, 5000
lines of scrollback each, victim `pane capture` latency in another session):

| attack | before | after |
|---|---|---|
| `ESC[?2026h ESC[?2026l` | **15 of 15 captures never returned** (8 s ceiling), 710x median | **1.0x**, none timed out |
| `ESC[?2026h a ESC[?2026l` | 6 of 15 never returned, 830x median | **1.8x**, none timed out |

## Testing matrix

| # | Property | Where |
|---|---|---|
| 1 | A window that changes nothing costs no more than parsing it | `sync_output_bounds.rs::sync_toggling_costs_no_more_than_parsing_it` |
| 2 | Redundant `?2026h`/`?2026l` cost no more than parsing | `sync_output_bounds.rs::redundant_sync_mode_changes_...` |
| 3 | Inert traffic inside a window takes no snapshot (hook precision) | `sync_output_bounds.rs::inert_traffic_inside_a_sync_window_...` |
| 4 | `ESC[?2026h ESC c` takes no snapshot | `sync_output_bounds.rs::sync_then_full_reset_takes_no_snapshot` |
| 5 | Cost independent of scrollback depth | `sync_output_bounds.rs::sync_toggle_cost_is_independent_of_scrollback_depth` |
| 6 | Cost independent of pane size | `sync_output_bounds.rs::sync_toggle_cost_is_independent_of_pane_size` |
| 7 | A written window does not scale with pane width | `sync_output_bounds.rs::a_written_sync_window_does_not_scale_with_pane_width` |
| 7b | A snapshot that IS taken does not scale with history | `sync_output_bounds.rs::a_taken_snapshot_does_not_scale_with_retained_history` |
| 7c | Scrolling all history inside a window adds at most one screen | `sync_output_bounds.rs::a_window_that_scrolls_all_history_costs_little_more_than_the_scroll_alone` |
| 7d | A full-screen clear inside a window copies one screen, not one grid | `sync_output_bounds.rs::a_full_screen_clear_inside_a_window_copies_one_screen_not_one_grid` |
| 8 | A retained line costs a pointer, not its cells | `sync_output_bounds.rs::a_retained_line_costs_a_pointer_not_its_cells` |
| 9 | COW copies only the rows written | `sync_output_bounds.rs::a_written_sync_window_copies_only_the_rows_it_writes` |
| 10 | Nothing is retained across windows | `sync_output_bounds.rs::repeated_sync_windows_do_not_retain` |
| 11 | Guards: written window visible to counters; empty input free; control not free | `sync_output_bounds.rs` (3 tests) |
| 12 | Deferral is unobservable (differential proptest, 400 cases) | `sync_output_differential.rs::deferring_the_sync_freeze_is_unobservable` |
| 13 | Same, on a pane with real scrollback | `sync_output_differential.rs::..._with_scrollback` |
| 14 | Guard: the two arms take different allocation paths | `sync_output_differential.rs::the_two_arms_take_different_allocation_paths` |
| 15 | Presented frame frozen with and without a copy taken | `lib.rs::sync_window_that_writes_nothing_still_presents_the_same_frame` |
| 16 | Every write in a window is hidden, not just the first | `lib.rs::every_write_in_a_sync_window_is_hidden_not_just_the_first` |
| 17 | Escape split across PTY reads | `lib.rs::sync_mode_survives_an_escape_split_across_reads` |
| 18 | Combined `?1049;2026h` / `?2026;1l` | `lib.rs::sync_mode_arms_and_releases_from_a_combined_parameter_list` |
| 19 | Alt-screen switch inside a window stays hidden | `lib.rs::an_alt_screen_switch_inside_a_sync_window_stays_hidden` |
| 20 | Title set/replaced inside a window does not leak | `lib.rs` (2 tests) |
| 21 | RIS inside a window releases it | `lib.rs::full_reset_inside_a_sync_window_releases_it` |
| 22 | Deadline releases a stuck window | `lib.rs::a_sync_window_is_released_once_it_outlives_its_deadline` |
| 23 | Silent pane swept by the daemon path | `lib.rs::a_silent_pane_holding_a_sync_window_is_swept` |
| 24 | Byte cap releases independently of the clock | `lib.rs::a_sync_window_is_released_once_it_absorbs_too_much_output` |
| 25 | Deadline is per window, not per pane | `lib.rs::the_deadline_resets_with_every_window` |
| 26 | End-to-end: victim RPC latency, quiet vs under attack | `.shux/scripts/sync_output_dos_check.sh` |
| 27 | Rich TUIs render correctly inside a window | dogfood: `vim`, `btop` (real mode-2026 windows), `htop` |
| 28 | No write path reaches a row's cells without unsharing | `cow_aliasing_adversarial.rs` (22 tests; proven able to fail by reintroducing the defect in a sandbox) |
| 29 | A resize releases the window and presents what an unsynced terminal would | `cow_aliasing_adversarial.rs::a_resize_releases_the_window_and_presents_the_live_frame`, `..::alt_screen_resize_releases_...`, `lib.rs::synchronized_output_resize_*` |
| 30 | A same-size resize does NOT release the window | `cow_aliasing_adversarial.rs::a_same_size_resize_leaves_the_window_open` |
| 31 | History stays reachable while a window is open | `lib.rs::synchronized_output_keeps_presented_scrollback_reachable` |

## Acceptance criteria

- [x] Lazy freeze, hook exhaustive by construction rather than by enumeration
- [x] `sync_toggle_2026` in `alt_screen_churn` down to the parse floor
- [x] Cost independent of pane geometry AND retained history
- [x] Differential-proptest oracle proves deferral unobservable
- [x] Bounds tests seen RED at `6236f25` before the fix
- [x] Rich-TUI evidence unchanged: `vim`, `btop`, `htop`, no tearing inside a window
- [x] End-to-end DoS check: no victim latency delta under attack (1.0x / 1.8x)
- [x] `make check` green (lint + full workspace suite)
- [x] Adversarial review: four agents on disjoint surfaces; every finding reproduced and fixed with a regression test
