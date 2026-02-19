# Task 060: Rich CLI Output — Beautiful List Commands

**Status:** Planned
**Priority:** High (UX polish, brand identity)
**Milestone:** M1 (alongside pane/rendering work)
**Depends on:** 011 (CLI foundation), 015 (pane operations)
**Touches:** `crates/shux/src/style.rs`, all `handle_*_list` functions in `cli.rs`

---

## Problem

The current CLI list output (`shux ls`, `window list`, `pane list`) looks like tmux circa 1995:

```
alpha: 1 window (created 10s ago) [bfdb89fb-dbc5-49cc-b1fc-613a0ca00f66]
beta: 1 window (created 7s ago) [6e68ad4a-75d5-462f-a409-a5b39002ca89]
```

```
0 : 1 (1 pane)
1 : editor (1 pane)
3*: logs (1 pane)
```

```
  [39287dc5-fa35-4d04-b0a0-c3a7fca02d32]
* [ef06172a-430f-4521-abaf-938d3d394df1]
```

**Issues:**
1. **No visual container** — output bleeds into surrounding terminal noise
2. **No column alignment** — data is hard to scan vertically
3. **UUID overwhelm** — full 36-char UUIDs dominate every line; 8-char short IDs suffice
4. **No summary footer** — no at-a-glance totals
5. **No hierarchy context** — `pane list` doesn't show which session/window
6. **Active marker is invisible** — a lonely `*` is easy to miss
7. **No empty-state design** — "no sessions" is just dimmed text
8. **No icons/glyphs** — missed opportunity for visual anchors
9. **Confirmation messages are OK** — one-liners like `Created session 'alpha'` are fine, but could get a subtle checkmark

The PRD says: *"Beautiful defaults. Zero config for 90% of use cases."* and *"What tmux would be if designed today."* The CLI output is the first thing users see — it IS the brand.

---

## Design Principles

1. **Framed, not floating** — Unicode box-drawing (`╭─╮│╰─╯`) gives visual containment
2. **Aligned columns** — Names, counts, ages, IDs in clean columns for scannability
3. **Short IDs** — 8-char UUID prefix (like git short SHA). Full UUID available via `--format json`
4. **Context headers** — Every list shows what scope you're looking at
5. **Summary footers** — Totals at bottom-right of frame
6. **Active indicator** — Colored `◆` (filled diamond) vs `◇` (open diamond), not `*`
7. **Graceful fallback** — Plain ASCII when piped, `NO_COLOR`, or dumb terminal
8. **Don't break scripts** — `--format json` is unchanged; `--format plain` for simple parseable text

---

## Proposed Designs

### A. Session List — `shux ls`

**Current:**
```
alpha: 1 window (created 10s ago) [bfdb89fb-dbc5-49cc-b1fc-613a0ca00f66]
beta: 1 window (created 7s ago) [6e68ad4a-75d5-462f-a409-a5b39002ca89]
gamma: 1 window (created 4s ago) [d3b6fb44-c0b6-485f-91fd-49118f10d3e7]
session-3: 1 window (created 2s ago) [4a3b8c8a-d46e-4a5e-95ba-79f296434d27]
```

**Proposed:**
```
╭─ Sessions ────────────────────────────────────────────────╮
│                                                           │
│  ◆ alpha           2 windows    10s ago         bfdb89fb  │
│  ◇ beta            1 window      7s ago         6e68ad4a  │
│  ◇ gamma           1 window      4s ago         d3b6fb44  │
│  ◇ session-3       1 window      2s ago         4a3b8c8a  │
│                                                           │
╰──────────────────────────── 4 sessions · 5 windows total ─╯
```

- `◆` (cyan, bold) = attached/active session; `◇` (dim) = detached
- Session name is bold white, columns are aligned
- Short 8-char ID is dim/muted (right-aligned)
- Summary footer shows aggregate counts
- The box width adapts to the longest session name (min 56 cols)

**Empty state:**
```
╭─ Sessions ────────────────────────────────────╮
│                                               │
│  (no sessions)                                │
│                                               │
│  Create one: shux new -s my-project           │
│                                               │
╰───────────────────────────────────────────────╯
```

### B. Window List — `shux window list -s alpha`

**Current:**
```
0 : 1 (1 pane)
1 : editor (1 pane)
2 : server (1 pane)
3*: logs (1 pane)
```

**Proposed:**
```
╭─ Windows ── session: alpha ───────────────────────────────╮
│                                                           │
│   #   NAME              PANES                             │
│   0   1                 1                                 │
│   1   editor            2                                 │
│   2   server            1                                 │
│   3   logs              1        ◀ active                 │
│                                                           │
╰────────────────────────── 4 windows · 5 panes ── alpha ──╯
```

- Context header shows parent session name (cyan accent)
- Column headers are dim/muted
- `◀ active` marker is cyan, much more visible than `*`
- Index column is right-aligned
- Footer repeats session name for context

### C. Pane List — `shux pane list -s alpha`

**Current:**
```
  [39287dc5-fa35-4d04-b0a0-c3a7fca02d32]
  [81d026be-2a82-43a5-ba55-62b3b8ccb3db]
* [ef06172a-430f-4521-abaf-938d3d394df1]
```

**Proposed:**
```
╭─ Panes ── window: editor ── session: alpha ───────────────╮
│                                                           │
│   ID         CWD                  CMD                     │
│   39287dc5   ~/code/project       zsh                     │
│   81d026be   ~/code/project       vim main.rs             │
│   ef06172a   ~/code/project       zsh          ◀ focus    │
│                                                           │
╰──────────────────────────────── 3 panes ── editor:alpha ──╯
```

- Context header shows full hierarchy (window + session)
- Short 8-char pane IDs instead of full UUIDs
- CWD and command columns (when available from pane metadata)
- `◀ focus` marker for active pane
- If a pane is zoomed: show `◀ focus [zoomed]` in yellow

### D. Confirmation Messages — `shux new`, `kill`, etc.

**Current:**
```
Created session 'alpha' [bfdb89fb-dbc5-49cc-b1fc-613a0ca00f66]
```

**Proposed:**
```
✓ Created session 'alpha'  bfdb89fb
```

- Leading `✓` (green) for success, `✗` (red) for errors
- Short ID instead of full UUID
- One-liner, no box needed — these are transient confirmations

### E. Error Messages

**Current:**
```
error: session name 'alpha' already exists
Error: session name 'alpha' already exists
```

**Proposed:**
```
✗ session name 'alpha' already exists
```

- Single line, no redundant "error:" prefix + "Error:" duplication
- `✗` in red is the error indicator

### F. Tree View — `shux ls --tree` (future, stretch)

```
╭─ Sessions ────────────────────────────────────────────────╮
│                                                           │
│  ◆ alpha                                10s ago  bfdb89fb │
│  ├─ editor (2 panes)                                     │
│  │  ├─ zsh ~/project                             39287dc5│
│  │  └─ vim main.rs                               81d026be│
│  └─ server (1 pane)                                      │
│     └─ node server.js                            ef06172a│
│                                                           │
│  ◇ beta                                  7s ago  6e68ad4a │
│  └─ 1 (1 pane)                                           │
│     └─ zsh ~/                                    a1b2c3d4│
│                                                           │
╰──────────────────── 2 sessions · 3 windows · 5 panes ────╯
```

---

## Fallback Strategy

| Condition | Behavior |
|-----------|----------|
| `--format json` | Machine-readable JSON (unchanged) |
| `--format plain` | Tab-separated, no box, no color (NEW — for `grep`/`awk`) |
| `--format text` (default) | Rich output described above |
| Piped (`!is_terminal`) | Auto-switches to `plain` (no box, no color) |
| `NO_COLOR` env | Box drawing preserved, all color/bold stripped |
| Narrow terminal (<40 cols) | Omit ID column, shrink box |
| Dumb terminal (`TERM=dumb`) | Same as `plain` |

The `plain` format is designed for scripting:
```
alpha	2	10s	bfdb89fb
beta	1	7s	6e68ad4a
```

---

## Implementation Plan

### Step 1: Add `TerminalContext` to `style.rs`

A small struct that captures output decision-making:
- `is_tty: bool` — stdout is terminal
- `colors: bool` — NO_COLOR + is_tty
- `unicode: bool` — TERM != dumb, locale includes UTF-8
- `width: u16` — terminal width (fallback 80)
- `format: OutputFormat` — from `--format` flag

All rendering functions take `&TerminalContext` instead of calling `colors_enabled()` repeatedly.

### Step 2: Add `BoxRenderer` to `style.rs`

A helper that draws Unicode box frames with dynamic width:

```rust
struct BoxRenderer {
    ctx: TerminalContext,
    min_width: u16,
    title: Option<String>,
    footer: Option<String>,
}

impl BoxRenderer {
    fn header(&self) -> String;          // ╭─ Title ──...──╮
    fn row(&self, content: &str) -> String;  // │ content...   │
    fn separator(&self) -> String;       // ├──────...──────┤
    fn footer(&self) -> String;          // ╰──── footer ───╯
    fn empty_row(&self) -> String;       // │               │
}
```

When `!ctx.unicode`, falls back to ASCII: `+-|` instead of `╭─╮│╰─╯`.
When `!ctx.is_tty`, emits nothing (no box).

### Step 3: Add `ColumnLayout` to `style.rs`

A mini column-alignment engine:

```rust
struct ColumnLayout {
    columns: Vec<Column>,
}

struct Column {
    header: String,
    align: Align,     // Left, Right
    min_width: usize,
    color: Option<Color>,
}

impl ColumnLayout {
    fn add_row(&mut self, cells: Vec<String>);
    fn render(&self, ctx: &TerminalContext) -> Vec<String>;
}
```

Calculates max width per column, pads cells, returns formatted lines.

### Step 4: Rewrite `print_session_entry` → `render_session_list`

Replace individual `print_*_entry` calls with batch `render_*_list` functions that:
1. Collect all items
2. Calculate column widths
3. Render header + rows + footer as a box

### Step 5: Rewrite `print_window_entry` → `render_window_list`

Same pattern, with session context in header.

### Step 6: Rewrite `print_pane_entry` → `render_pane_list`

Same pattern, with window+session context in header.

### Step 7: Add `--format plain` support

Update `OutputFormat` enum to include `Plain`, wire it into handlers.

### Step 8: Polish confirmation messages

Add `✓`/`✗` prefix, switch to short IDs.

### Step 9: Update integration tests

Update string matching in `m0_integration.rs` and `cli_integration.rs` to match new output:
- Box-drawing characters in response validation
- Short IDs instead of full UUIDs in text output
- `◆`/`◇`/`◀` markers instead of `*`
- Summary footer assertions
- `--format json` tests remain unchanged (JSON is not affected)
- `--format plain` tests added for stable parseable output

### Step 10: L4 Visual Tests — `test_060_rich_cli_output.py`

Extensive iterm2-driver visual test suite. See full spec below.

### Step 11: Update existing L4 visual tests

Update test_013, test_014, test_015 assertions to match new output format:
- Box-drawing characters in content checks
- `◆`/`◇` instead of `*` for active markers
- Short 8-char IDs in content matching
- Summary footer text in content checks

---

## Dependencies & Crate Evaluation

**No new dependencies recommended.** The box-drawing and column alignment are simple enough to hand-roll (50-100 lines each). Adding `tabled` or `comfy-table` would be overkill for 3 list commands and would add compile time.

The existing `crossterm::style` + `crossterm::terminal::size()` provides everything needed:
- `terminal::size()` → terminal width for box sizing
- `style::Stylize` → color and attributes
- Already in the dependency tree

---

## L4 Visual Test Specification

**File:** `.claude/automations/test_060_rich_cli_output.py`

This is the most visually-oriented task in the project — the test suite must capture **every output variant** as a screenshot and verify structural properties of the rich output.

### Part A — Setup & Build (Tests 1–2)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 1 | Build | `make build` | returncode == 0 | — |
| 2 | Create test sessions | `shux new -s alpha -d`, `shux new -s beta -d`, `shux new -s gamma -d` | All created | `060_setup` |

### Part B — Session List: Rich Output (Tests 3–8)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 3 | Box frame present | `shux ls` | Output contains `╭` and `╰` box corners | `060_session_list` |
| 4 | Header text | (from test 3 output) | Contains `Sessions` in header line | — |
| 5 | Column alignment | (from test 3 output) | All session names left-aligned, counts aligned, IDs right-aligned | — |
| 6 | Active marker | (from test 3 output) | Active session line contains `◆` (not `*`) | — |
| 7 | Detached markers | (from test 3 output) | Non-active sessions have `◇` | — |
| 8 | Summary footer | (from test 3 output) | Footer contains `3 sessions` and `windows` | `060_session_list_footer` |

### Part C — Session List: Short IDs (Tests 9–10)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 9 | Short IDs in text | `shux ls` | Lines contain 8-char hex IDs (not 36-char UUIDs) | `060_short_ids` |
| 10 | Full IDs in JSON | `shux --format json ls` | JSON output still contains full 36-char UUIDs | `060_json_full_ids` |

### Part D — Session List: Empty State (Test 11)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 11 | Empty state | Kill all sessions, then `shux ls` | Box frame with "(no sessions)" and hint text | `060_empty_sessions` |

### Part E — Window List: Rich Output (Tests 12–17)

Recreate sessions first (alpha with 3 windows: editor, server, logs).

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 12 | Box frame present | `shux window list -s alpha` | Output contains `╭` and `╰` | `060_window_list` |
| 13 | Context header | (from test 12 output) | Header contains `session: alpha` | — |
| 14 | Column headers | (from test 12 output) | Contains `#`, `NAME`, `PANES` column labels (dim) | — |
| 15 | Active marker | (from test 12 output) | Active window line contains `◀ active` (not `*`) | `060_window_active_marker` |
| 16 | Index alignment | (from test 12 output) | Index numbers right-aligned in `#` column | — |
| 17 | Summary footer | (from test 12 output) | Footer contains window count and pane count | `060_window_list_footer` |

### Part F — Pane List: Rich Output (Tests 18–23)

Split panes in alpha's editor window first.

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 18 | Box frame present | `shux pane list -s alpha` | Output contains `╭` and `╰` | `060_pane_list` |
| 19 | Context header | (from test 18 output) | Header contains `session: alpha` and window name | — |
| 20 | Column headers | (from test 18 output) | Contains `ID`, `CWD`, `CMD` column labels | — |
| 21 | Short pane IDs | (from test 18 output) | Pane IDs are 8-char, not full UUIDs | — |
| 22 | Focus marker | (from test 18 output) | Active pane line contains `◀ focus` | `060_pane_focus_marker` |
| 23 | Summary footer | (from test 18 output) | Footer contains pane count | `060_pane_list_footer` |

### Part G — Pane List: Zoom State (Tests 24–25)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 24 | Zoom a pane | `shux pane zoom -s alpha` | Success confirmation | — |
| 25 | Zoom visible in list | `shux pane list -s alpha` | Zoomed pane shows `[zoomed]` (yellow) in its row | `060_pane_zoomed` |

### Part H — Confirmation Messages (Tests 26–30)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 26 | Create confirmation | `shux new -s confirm-test -d` | Output starts with `✓` and contains short ID | `060_confirm_create` |
| 27 | Kill confirmation | `shux kill -s confirm-test` | Output starts with `✓` and says `Killed` | `060_confirm_kill` |
| 28 | Window create | `shux window new -s alpha -n test-win` | Output starts with `✓` | `060_confirm_window_create` |
| 29 | Pane split | `shux pane split -s alpha` | Output starts with `✓` | `060_confirm_pane_split` |
| 30 | Rename | `shux window rename -s alpha -w test-win -n renamed` | Output starts with `✓` | `060_confirm_rename` |

### Part I — Error Messages (Tests 31–33)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 31 | Duplicate session | `shux new -s alpha -d` | Output starts with `✗`, no redundant "error: Error:" duplication | `060_error_duplicate` |
| 32 | Kill nonexistent | `shux kill -s nonexistent` | Output starts with `✗` | `060_error_not_found` |
| 33 | Kill last pane | (kill down to 1 pane, try kill) | Output starts with `✗` | `060_error_last_pane` |

### Part J — Plain Format / Piped Output (Tests 34–37)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 34 | Plain session list | `shux --format plain ls` | No box chars (`╭╰│`), tab-separated columns | `060_plain_sessions` |
| 35 | Plain window list | `shux --format plain window list -s alpha` | No box chars, tab-separated | `060_plain_windows` |
| 36 | Plain pane list | `shux --format plain pane list -s alpha` | No box chars, tab-separated | `060_plain_panes` |
| 37 | Piped auto-detect | `shux ls \| cat` (via subprocess) | No ANSI escapes, no box chars in captured output | — |

### Part K — NO_COLOR Compatibility (Tests 38–39)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 38 | NO_COLOR session list | `NO_COLOR=1 shux ls` | Box drawing preserved, no ANSI color codes | `060_nocolor_sessions` |
| 39 | NO_COLOR confirmation | `NO_COLOR=1 shux new -s nocolor-test -d` | `✓` present but no color escapes | `060_nocolor_confirm` |

### Part L — Multi-Session Stress (Tests 40–42)

Create 8+ sessions to verify box scaling and alignment.

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 40 | Many sessions | Create `s1` through `s8`, then `shux ls` | Box accommodates all, columns still aligned | `060_many_sessions` |
| 41 | Long session name | Create session `my-extremely-long-project-name-that-tests-width`, then `shux ls` | Box widens, no truncation, no overflow | `060_long_name` |
| 42 | Summary accuracy | (from test 40 output) | Footer count matches actual session count | — |

### Part M — JSON Format Cross-Check (Tests 43–44)

| # | Test | Command | Verify | Screenshot |
|---|------|---------|--------|------------|
| 43 | JSON session list | `shux --format json ls` | Valid JSON array, full UUIDs, all fields present | `060_json_sessions` |
| 44 | JSON pane list | `shux --format json pane list -s alpha` | Valid JSON array, `window_id` field, full UUIDs | `060_json_panes` |

### Screenshot Verification Strategy

Every screenshot captured by the test suite must be **visually inspected** after the run. The test script prints a summary table mapping test names to screenshot files. During implementation:

1. **First pass:** Run full suite, capture all ~30 screenshots
2. **Visual audit:** Open each screenshot and verify:
   - Box corners render correctly (no mojibake)
   - Column alignment is visually clean
   - Colors match the palette (cyan accent, green success, red error, dim muted)
   - Active markers (`◆`, `◀`) render at correct Unicode width
   - No trailing whitespace or misaligned right borders
   - Summary footer text is right-aligned within the box
3. **Regression baseline:** Save verified screenshots as reference (gitignored)
4. **Re-run after changes:** Any style.rs change must re-run the full suite

### Test Infrastructure Notes

- Kill stale daemon before each run (belt-and-suspenders, same as test_014/015)
- Each Part that needs fresh state should clean up previous test sessions
- `read_screen()` captures full terminal buffer for text assertions
- `take_screenshot()` captures iTerm2 window for visual verification
- Tests that check "no ANSI escapes" should use subprocess capture (not iTerm2 screen) to get raw bytes

---

## Acceptance Criteria

1. All three list commands (`ls`, `window list`, `pane list`) render with box frames, aligned columns, context headers, and summary footers
2. Active/focused items use `◆`/`◇` diamonds and `◀ active`/`◀ focus` marker (cyan)
3. UUIDs are displayed as 8-char short prefixes (except in `--format json`)
4. Piped output auto-detects and switches to `plain` (no box, no color, tab-separated)
5. `NO_COLOR` strips color but preserves box drawing and Unicode
6. `--format plain` provides stable, parseable tab-separated output
7. Confirmation messages use `✓`/`✗` prefix with short IDs
8. All existing L4 visual tests (013, 014, 015) updated and passing with new output format
9. New L4 visual test suite (`test_060_rich_cli_output.py`) with 44 tests and ~30 screenshots, all passing
10. Every screenshot visually audited for rendering correctness
11. Zero new crate dependencies

---

## Non-Goals (for this spike)

- Tree view (`--tree`) — defer to a follow-up task
- Color themes for CLI output — defer to task 024 (theme engine)
- Interactive/filterable lists — defer to task 032 (command palette)
- Progress bars or spinners — not needed for list commands
- Nerd Font icon support — nice-to-have but not required; standard Unicode suffices
