# 095 — `pane list` named no pane, and its argv column lost the argument boundaries

**Status:** Done
**Priority:** Medium (cosmetic on its face; two of the defects are wrong execution)
**Milestone:** M3 polish
**Depends On:** 094
**Quality Gate:** shux-tui-qa — CLI listing surface, no VT/raster change
**Touches:** `crates/shux/src/style.rs`, `crates/shux/src/cli.rs`,
`crates/shux/src/pane_command.rs`, `crates/shux/src/main.rs`,
`crates/shux-pty/src/command.rs`, `crates/shux-pty/src/lib.rs`,
`crates/shux/tests/pane_list_columns.rs` (new),
`.shux/scripts/issue_135_evidence.sh` (new),
`.shux/scripts/issue_135_mutation_check.sh` (new),
`.config/nextest.toml`, `scripts/check-test-groups.sh`

---

## Problem (issue #135)

The issue reports two gaps in `pane list`'s human output. Reproducing them found
the first one is worse than reported, and reading the argv-join path found two
more defects of the same class on the same surface.

### 1. The human format named no pane — and said nothing else either

The issue says there is no title column. In `--format text` — the format a human
actually gets — there is no *anything* column. Captured inside a real pane, on
`e7fe0fa`:

```
╭─ Panes ── window: 1 ── session: r135t ───╮
│                                          │
│ ID                                       │
│ fef99978   ◀ focus                       │
│ 216245e8                                 │
│                                          │
╰────────────────────── 2 panes ── 1:r135t ╯
```

`style::render_pane_list`'s Text arm built a two-column layout — an id and a
focus marker — and never read `PaneInfo::cwd` or `PaneInfo::command`, both of
which `cli.rs` computed and handed it. `title` was not on `PaneInfo` at all,
though `pane.list` has always returned it.

Task 060 §C, which designed this box, specified **ID / CWD / CMD**. Only ID ever
shipped. So this is not a missing feature; it is a listing that never
implemented its own design, and #135 adds the title the pane's border draws.

### 2. argv joined with a bare space

`["/bin/sh", "-c", "sleep 900", "one two three"]` and a seven-element argv
printed identically. Since #125 made `$SHELL -c <script>` the normal shape of a
`--cmd` pane, the ambiguous case is now the common one.

### 3. `pane.run_command`'s `args` — the sixth spawner (found while reading the join)

`shell_escape_args` is the repo's other "argv → one string" function, and
`CommandEngine::start_command` uses it to build a line it **types into the
pane's live shell**. It quoted on a **denylist** of seven characters — space,
`"`, `'`, `$`, `\`, `` ` ``, `!`. Everything else went through raw. Reproduced
against `e7fe0fa`:

```
args: ["a;id", "", "b"]   →  echo a;id  b
a
uid=0(root) gid=0(root) groups=0(root)
```

The `;` started a second command; the empty argument vanished. Also reproduced:
`{a,b}` split into two words, a newline truncated the line, and `(` produced a
syntax error that swallowed the completion marker so the call sat out its whole
timeout and returned `timed_out`.

**Severity, stated honestly.** This is *not* privilege escalation: the sibling
`command` parameter is deliberately raw shell text, so a caller who can send
`args` can already run anything. It is the #125 defect — input accepted, wrong
thing done, no error — on the one spawning RPC #125 did not reach. The same read
found `args` parsed with `filter_map(|v| v.as_str())`, so `["a", null, "b"]` ran
`a b` and reported success: the exact silent drop #125 removed from the other
five.

### 4. Nothing bounded the box's width

`TerminalContext::width` has been captured since task 060 and read by nothing
(hence the `#[allow(dead_code)]`), and task 060's own "narrow terminal → shrink
the box" fallback was never implemented. That was survivable while the widest
cell was a session name. It is not survivable once a `--cmd` string is a column,
so the fix could not add the columns without also adding the bound.

## The fix

### One shell-quoting implementation, shared by both consumers

`shux_pty::shell_quote_arg` replaces the denylist with an ASCII **allowlist** —
a denylist is always one metacharacter short of the shell it guards. Empty
string → `''` (a denylist structurally cannot catch it: it contains nothing).
Leading `=` is always quoted because zsh's `EQUALS` option is on by default, and
`=nosuchprog` is a *fatal* error that aborts the line, taking the completion
marker with it. `~` is deliberately off the allowlist, and must stay off: bash
expands a tilde after an unquoted `=` or `:`.

`pane list` renders argv with the same function. Two dialects of "faithful
rendering of an argv" would be free to disagree, and drift on the one that
injects is how the denylist survived this long.

### The pane list gets its columns

| arm | before | after |
|---|---|---|
| `--format plain` | `id \t cwd \t command` | `id \t cwd \t command \t title` |
| `--format text` | `ID`, marker | `ID`, `TITLE`, `CWD`, `COMMAND`, marker |
| `--format json` | unchanged | unchanged |

`title` goes **last** in the plain arm so the three fields scripts already parse
keep their positions — the rule issue #120 followed when `window list` grew an
id column. The `command` field's *content* changes: it is now shell-quoted. That
strictly increases information (the boundaries become recoverable where they
were lost), and `--format json` remains the byte-exact machine contract.

### The box is bounded by the terminal

`ColumnLayout::budget` is opt-in per listing, so `session list` and `window list`
render byte-identically to before (pinned by a test). Within the budget:

- Columns are chosen by priority — ID, then TITLE, then COMMAND, then CWD — and
  one that cannot reach a useful width is **dropped**, not rendered as a bare
  ellipsis. This is task 060's own narrow-terminal fallback, finally implemented.
- The remaining width is shared **max-min fair**: smallest ask first, each
  getting its whole ask or an equal share of what is left. A proportional split
  clipped a six-cell `TITLE` to `prin…` to buy four columns for a `COMMAND` that
  was going to be truncated anyway.
- The header and footer are trimmed too. The box is not bounded by its columns
  alone: the header quotes the window and session names and the footer quotes
  them again.

### `args` is validated for the sink it actually has

`parse_run_args` rejects non-string elements (naming the index), and
`reject_untypeable` rejects C0 and DEL. Every other command in `pane_command.rs`
ends at `execve`, where NUL is the only impossible byte; `args` ends at a **tty
line discipline**, where NUL is the byte that is harmlessly discarded and
`0x03`/`0x15`/`0x1a`/`0x7f` are the ones that destroy the line. See the
regression below for why that is not a tidiness argument.

## Adversarial review — three agents, disjoint surfaces, driving the real binary

Every finding was reproduced independently before it was believed.

| # | Surface | Finding |
|---|---|---|
| 1 | layout | **A regression this task introduced.** `fit_width` summed **per-character** widths while everything else in the module measures **whole strings**, and `UnicodeWidthStr::width` is a property of the string: `☀️` (U+2600 U+FE0F) is 1 summed and **2** as a string; a ZWJ family is 6 summed and **2**. Fitted cells came back up to twice their allocation, `pad_right` then added no padding, the row under-reported its own length, and the box printed lines wider than the terminal with a ragged frame — at **100 of 177 widths tested**, up to 240 columns wide in an 80-column terminal. Fixed by measuring the accumulated string; the same fix removes an ellipsis appended when nothing was dropped, and stops ZWJ sequences truncating six times too early |
| 2 | layout | The stated minimum boxable width was **wrong by seven**. It was written down as 24 from "an id and a `◀ focus` marker are 18 columns", forgetting that a *zoomed* pane's marker is `◀ focus [zoomed]` — 16 columns, not 7. Now derived from `pane_marker(true, true)` rather than restated, and the width sweep includes a zoomed pane. (The overflow below the minimum is pre-existing and worse on `e7fe0fa`; what was wrong here was the claim) |
| 3 | RPC | **A regression this task introduced.** A control byte in `args` **permanently wedged the pane.** Correct quoting puts the byte *inside* the single quotes; the tty line discipline then consumes it (`0x03` INTR, `0x15` KILL, `0x1a` SUSP, `0x7f` ERASE) and truncates the line mid-quote, so bash drops to its `>` continuation prompt and stays there, swallowing every later command sent to that pane. A/B, fresh pane per case: on `e7fe0fa` the same input failed once and **recovered**; on this branch the pane was dead. Making the quoting correct turned a transient error into a permanent one, and the missing validation is what allowed it. Fixed by `reject_untypeable` |
| 4 | quoting | The doc on `render_argv` claimed the display "cannot drift into promising a round-trip it does not deliver". **It does drift** — `safe_label` (the issue #104 egress guard) runs *after* the quoting and must, so a control byte becomes the visible text `\u{9}` inside the quotes and the printed line no longer reproduces the original bytes. Measured: argument **boundaries** survived 2,637 of 2,637 end-to-end cases; **content** did not, in 73% of cases containing a control character. The claim is corrected in the code; the boundaries are what #135 asked for and what the acceptance criteria say |
| 5 | listing | A pane that is **zoomed but not focused** showed no marker at all in either human format while `--format json` reported `is_zoomed: true`. `pane.zoom` takes a pane id, so that state is ordinary — and it is the state an operator most needs told about, since a zoomed pane is why the others are off screen. Pre-existing; fixed here because the marker is this task's surface |
| 6 | quoting | `pane run --help` documented `--command` as "Command to run" and never said it is shell text typed into the pane's live shell, nor that the `args` half of the contract exists only on the RPC. Documented |

### What three agents could not break

109,560 shell round-trips over ~36,500 random argvs from a hostile alphabet
across `/bin/sh`, bash and zsh: **0 mismatches**. 2,637 end-to-end checks over
816 real panes: 0 word-count breaks, 0 field-count breaks, 0 executions. Plain's
four-field contract held on every row of every input, including cwds containing
tab, newline, CR, ESC, BEL, quotes and `$(id)`; plain matched JSON byte-for-byte
on id, cwd, quoted argv and title. Header/footer trims correct at every width
24–200 with 128-character session *and* window names. `args` type rejection
correct and indexed for every malformed shape; rejections never touch the PTY,
and a valid call always works after a rejected one. Size caps exact at their
boundaries.

## Known residuals — measured, filed, not papered over

- **The egress guard is not injective, so two distinct argvs can still render
  identically.** `["/bin/sleep", "a\nb"]` and `["/bin/sleep", "a\\u{a}b"]` both
  print `/bin/sleep 'a\u{a}b'`; `--format json` distinguishes them. This is the
  #135 defect class displaced from boundaries to content, and it is
  pre-existing (`e7fe0fa` prints the same). It is **not** fixed here because the
  obvious fix — escaping a backslash that would start one of the guard's own
  escapes — **breaks `safe_label`'s idempotence**, which is asserted by
  `test_safe_label_is_idempotent` and relied on by `print_success` (whose
  callers escape their own arguments first). Injectivity and idempotence cannot
  both hold for this escape alphabet; choosing between them is a decision about
  issue #104's guard, which every listing and every confirmation message shares.
  Filed with the reproduction rather than decided here.
- **`-w 1` names the *second* window.** `window list` shows the default window at
  index 0 with the name `1`, and `resolve_window_id` tries the index before the
  name. Pre-existing and unrelated to this issue — but it made the first cut of
  the evidence script photograph the wrong window, so it is worth someone's
  decision. Filed.
- **A pane running the default shell shows a blank COMMAND.** `pane.list`
  genuinely returns `command: []` there — the resolved login shell is not
  recorded on the pane. The column is truthful; making it useful means changing
  what the daemon stores, which is a larger change than this task.
- **`MAX_ARGV_BYTES` bounds what is stored, not what is echoed.** `'` → `'\''`
  is a 4× expansion, so a 240 KiB argv of quotes produced a 960 KB single line
  in `--format plain` (measured). Bounded, but at 4× the stated bound.
- **Below the minimum boxable width the frame still overflows.** An id and a
  focus marker are not droppable — the id is what every other verb takes as an
  argument — so a terminal too narrow for them cannot have a correct box. Worse
  on `e7fe0fa` (up to 47 columns); the invariant is asserted from the derived
  minimum upward.
- **`render_empty_state` ignores the budget**, so a `(no panes)` box floors at
  44 columns. Shared with `session list` and `window list`; unreachable for
  `pane list` (a window always has a pane). Pinned by a test that will fail when
  someone fixes it, so the pin cannot outlive the residual.

## Testing Matrix

| Level | Coverage |
|---|---|
| Unit — `shux-pty::command` | allowlist vs denylist across 16 metacharacter shapes; empty argument; ordinary arguments left bare; every shape round-tripped through a **real** `/bin/sh`, and the whole table through `/bin/sh` + bash + **zsh** (which is the one that disagrees, on leading `=`); the injected line cannot be broken out of; the no-argument path |
| Unit — `style.rs` | plain arm's four columns, order and content; one argument with a space vs several; an empty argument; the text arm's title and command; a fitted cell is exactly its allocated width (VS16, ZWJ, RI, CJK, combining marks, at every width 0–39); the box never exceeds the terminal at **every** width from the derived minimum to 200 × unicode/ASCII × focused/zoomed; frame never ragged; truncation never splits a wide character; never ends on a zero-width character; a control character cannot forge a plain column; a zoomed unfocused pane is marked; `session list` / `window list` byte-identical; all three `col_widths` callers agree |
| Unit — `pane_command.rs` | every control character rejected by index and code point; every printable shape accepted including `""` |
| E2E — `tests/pane_list_columns.rs` | real daemon, real PTYs: the human formats name every pane; one space-bearing argument is not printed as several **and the printed line re-splits into the argv it came from through a real shell**; a shell-wrapped `--cmd` pane; text/plain/json agree on id, title, cwd and program; the listed pane still renders all three colour classes (asserted on the **pen** via `glance --cells`); `args` rejects a null by index; a metacharacter argument arrives whole **and the call completes** (not `timed_out`); an empty argument survives; a control byte is refused and the pane still works afterwards |
| Mutation — `.shux/scripts/issue_135_mutation_check.sh` | 21 mutations, each the pre-fix behaviour or a plausible wrong fix, each killed by a **named** test. A mutation whose edit matched nothing is a failure, not a kill |
| Shell — `.shux/scripts/issue_135_evidence.sh` | six scenes, both binaries, through the shipped binary, under an isolated runtime with a cleanup trap. The list is run **inside a real pane** because `--format text` downgrades to plain the moment stdout is a pipe — a screenshot taken through a pipe would be of the wrong code path. `EXPECT_DEFECT=1` inverts the verdict so the baseline arm fails if the defect has already gone away |
| Dogfood | vim, top and less in real panes, listed at 110 columns through a real TTY: titles read `vim`/`top`/`less`, the `--cmd` pane's script is one quoted argument, all three TUIs still rendering |

## Acceptance Criteria

- [x] `pane list` names every pane in both human formats.
- [x] A quoted argument is distinguishable from several, and the printed line
      re-splits into the argv it came from.
- [x] `pane run`'s argument list cannot execute a second command.
- [x] A malformed `args` element is an error naming it, never a silent drop.
- [x] An argument that cannot be typed into a terminal is refused, and refusing
      it leaves the pane usable.
- [x] The box never renders wider than the terminal, at any width at or above
      the derived minimum, for focused and zoomed panes alike.
- [x] `session list` and `window list` are byte-identical to before.
- [x] Zero leaked daemons or child processes.

## DoD

- [x] RED test observed failing for every defect above before the fix, including
      both regressions this task introduced.
- [x] 21/21 mutations killed by named tests.
- [x] `make check` green (2,075 tests).
- [x] Before/after evidence on both binaries, every scene asserted, baseline arm
      verdict-inverted.
- [x] Visual proof published, six scenes side by side:
      <https://claude.ai/code/artifact/a12e9911-2c71-4ab4-996f-410eb64c4595>.
      This session is headless and has neither the `browsing-as-you` skill nor a
      `gh` CLI, so the Claude Artifact fallback in the feature protocol applies;
      the PNGs it embeds stay in gitignored `.shux/out/issue-135/` and are
      regenerated by `.shux/scripts/issue_135_evidence.sh`.
