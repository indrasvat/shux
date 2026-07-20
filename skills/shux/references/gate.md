# `shux lens gate` — visual regression gate for terminal UIs

`lens run`/`glance`/`diff` prove a change worked **once, by hand**. `lens gate` is the
CI form: a committed TOML **scenario** drives a TUI in a deterministic sandbox, captures
named frames, and compares them to **committed goldens**. A regression fails the build
with a machine-readable report and a heat PNG showing exactly which cells moved.

Think `insta`/`jest --ci` snapshots, but the snapshot is a terminal frame — including
**colour**. A text diff cannot see `bright_green` becoming `green`; the cell tier can.

```bash
shux lens gate scenario.toml                 # compare against committed goldens
shux lens gate scenario.toml --report -      # report.json on stdout, summary on stderr
shux lens gate scenario.toml --update        # re-bless the failing frames (never in CI)
shux lens gate review scenario.toml          # step through changed frames interactively
shux lens gate init myapp                    # scaffold myapp.toml + its first goldens
```

New to it? [../examples/headless-tui-test.md](../examples/headless-tui-test.md) walks the
whole lifecycle. This file is the reference.

## Exit codes (frozen contract — key CI on these)

| Code | Meaning |
|--|--|
| 0 | pass (or a valid xfail) |
| 1 | regression — a frame differs, is missing, is stale, or never settled |
| 2 | usage / scenario error |
| 3 | infra error (couldn't spawn, quota, …) |
| 4 | the run finished, but its report/trace could not be written — **not** a regression |
| 5 | the child process died |
| 6 | update refused (in CI, dirty golden tree, or a secret was detected) |

A frame with **no committed golden is a regression** (exit 1), not a silent pass. That is
deliberate: a golden can never be self-minted in CI.

Exit `4` is a CLI-level I/O failure — a bad `--report`/`--trace` path. No gate VERDICT
produces it, which is what makes it unambiguous: the check itself did not fail.

A regression **outranks** an operational error in the rollup: a run that both fails a frame
and has its bless refused exits `1`, not `6`, and keeps every per-frame verdict. An error
can never mask a regression.

## The scenario file

```toml
name = "mock-rich-tui"
description = "Deploy board: initial frame, then a j-key selection move."
command = ["uv", "run", "--offline", "board.py"]
cwd = "."                    # OPTIONAL, relative to THIS file's directory, and
                             # contained within it (symlinks out are refused)
deadline_ms = 60000          # optional, whole-scenario budget

[env]
# The sandbox starts from an EMPTY environment. Only what's here (plus the
# deterministic defaults) reaches the child.
PATH = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
allow = ["HOME"]             # opt specific HOST vars through (see the trap below)

[terminal]
rows = 24
cols = 80
respond_to_queries = false

[[mask]]                     # optional, scenario-wide redaction rects
row = 0
col = 60
width = 20

[[steps]]
action = "wait_for_text"
text = "shux deploy board"
timeout_ms = 20000

[[steps]]
action = "hold_settle"       # settle once the frame CONTENT has held still
hold_ms = 300
quiet_ms = 400
timeout_ms = 10000

[[steps]]
action = "expect_golden"
name = "start"               # -> goldens/<scenario>/start.*
tier = "cell"
retries = 2                  # re-settle + re-capture on a mismatch
```

`name` becomes a filesystem path component, so it must be a single safe segment. Two
names are rejected outright: anything made only of dots (it names no file), and a frame
called `failing` — that is the `--update failing` selector, and a frame with that name
could never be blessed on its own.

### Steps

| Action | Fields |
|--|--|
| `wait_for_text` | one of `text` or `regex`, plus `absent` (wait until it's GONE), `timeout_ms` |
| `wait` | `ms` |
| `settle` | `quiet_ms`, `timeout_ms` — quiet-based; fine for a TUI that stops painting |
| `hold_settle` | `hold_ms`, `quiet_ms`, `timeout_ms` — settles when the frame CONTENT is unchanged for `hold_ms`. **The right default for anything that repaints** |
| `stable_frames` | `n`, `quiet_ms`, `timeout_ms` — `n` contiguous identical revisions; for a *continuous* repainter only (a static TUI never produces `n` new revisions → `settle_never_stable`) |
| `type_text` / `paste` | `text` |
| `keys` | `keys` — vim notation, e.g. `["j", "<CR>", "<C-c>", "<Esc>"]` |
| `resize` | `rows`, `cols` |
| `expect_golden` | `name`, `tier`, optional `retries`, `hold_ms`, `stable_frames`, `quiet_ms`, `timeout_ms`, `[[steps.mask]]`, `[steps.xfail]` |
| `assert_contains` / `assert_not_contains` | `text` |
| `expect_exit` | `code`, `timeout_ms` |

A scenario with **no `expect_golden` proves nothing** and is refused (exit 2).

### Tiers

- `cell` — the canonical grid: characters **and colour/attributes**. Portable across
  machines. Start here.
- `pixel` — rasterized pixels, with a tolerance recorded in the blessed sidecar. Baselines
  are per-OS/arch.
- `exact` — byte-exact pixels.

Tiers are **conjunctive**: a matching PNG never overrides a failing cell compare.

## The normal loop

```bash
# 1. Write scenario.toml (or: shux lens gate init myapp)
# 2. First run: no goldens yet -> exit 1, "no committed golden"
shux lens gate scenario.toml

# 3. Mint the first goldens, then REVIEW them before committing
shux lens gate scenario.toml --on-missing create --reason "first baseline"
git add goldens/ && git commit

# 4. In CI, from now on:
shux lens gate scenario.toml            # exit 0 while nothing moves

# 5. A change lands. If the diff is a REGRESSION -> fix the code and re-run.
#    If the change was INTENDED -> re-bless it:
shux lens gate scenario.toml --update --reason "new footer status bar"
```

### What a golden is, and how to review one

Goldens live in `<scenario-dir>/goldens/<scenario>/` (override with `--golden-dir`), NOT
under `.shux/goldens/`. At the `cell` tier each frame is two text files — `<name>.capture.json`
(the frame: characters plus colour/attributes) and `<name>.fingerprint.json` (the run
identity) — plus a shared `BASELINE-APPROVAL.md`. **There is no PNG at the cell tier**, so
there is nothing to "open" in an image viewer; the capture is reviewable as text and diffs
readably in a PR.

To eyeball what a fresh baseline actually captured, render the same command instead:

```bash
RUN=$(shux --format json lens run --size 96x26 -- <your command>)
PANE=$(echo "$RUN" | jq -r .result.pane_id)
shux pane wait-settled "$PANE" --quiet 300ms
shux pane glance "$PANE" --png baseline-preview.png
shux session kill "$(echo "$RUN" | jq -r .result.session_id)"
```

`shux lens gate review` is for CHANGED frames — it steps through a failing run, not a fresh
baseline. A `pixel`/`exact` tier golden *is* a committed PNG, per OS/arch.

**Blessing is for intended changes only, and nothing enforces that.** The guards are
mechanical — CI, dirty tree, secret scan — and none of them can tell an intended
redesign from a bug. If the gate is red and you did not mean to change the UI, `--update`
will bless the regression, exit 0, and commit it as the new truth. Read the heat PNG first.

## Reading a failure

```
lens gate  scenario=mock-rich-tui  verdict=fail  frames=2  time=2.8s
FRAME     | STATUS | CHANGED | DETAIL
----------+--------+---------+-------
start     | fail   | 50      |
after-nav | fail   | 50      |
```

- `--out DIR` (default `.shux/out/<scenario>/`) receives a **heat PNG per failing frame**
  at `<out>/<frame>.heat.png`. Open it — it localizes the regression instantly.
- `--report PATH|-` writes `report.json`: per frame a `status`, `diff.changed_cells`,
  `diff.regions` (row + column span of every changed run), `diff.heat_png`, and
  `diff.style_deltas` — **what** changed, not just where:

  ```json
  "style_deltas": [
    {"row": 4, "col": 25, "col_end": 35, "expected": "fg=bright_green", "actual": "fg=green"},
    {"row": 5, "col": 25, "col_end": 35, "expected": "fg=bright_green", "actual": "fg=green"}
  ]
  ```

  This is the field to read for a **colour-only** regression: the text is byte-identical,
  so a text diff shows nothing and the coordinates alone don't say what moved. One entry
  per contiguous run (`[col, col_end)`), capped at **16** so a full-screen recolour can't
  bloat the report. Absent when only text changed. If the cap truncates, `diff.style_deltas_total`
  appears with the true number of runs — a partial list is never presented as the whole
  story.
- `--cast [PATH]` records a replayable asciinema v2 file of the whole run, so you can
  scrub how the TUI reached a failing frame.
- `--trace PATH|-` emits the raw runner-signal NDJSON.

`diff.regions` is usually enough to localize without opening anything: a fail confined to
`{"row": 4, "col_start": 25, "col_end": 35}` on several rows is one column of one table.

## The sandbox — read this before your first scenario

The child starts from an **empty environment** in an isolated HOME/XDG, with
`LC_ALL=C.UTF-8`, `LANG=C.UTF-8`, `TZ=UTC`, `TERM=xterm-256color`, `COLORTERM=truecolor`,
`SOURCE_DATE_EPOCH=0`, and `PATH=/usr/local/bin:/usr/bin:/bin`.

Two consequences bite everyone once:

1. **Your tool is probably not on that PATH.** Anything from Homebrew
   (`/opt/homebrew/bin`) or `~/.local/bin` — `uv`, `bat`, `lazygit`, `nvim` — is invisible.
   Set `[env] PATH = "…"` explicitly.

   Prefer setting `PATH` literally over `allow = ["PATH"]`. An `allow`-ed var passes the
   **host's** value through, and the environment is part of the run identity — so a golden
   blessed on your machine becomes `stale`/untrusted on a machine whose `PATH` differs.
   A literal value in the scenario is identical everywhere.

2. **The child's working directory is a scratch temp dir, not your project.** To run a
   program that lives beside the scenario, set `cwd` — a path **relative to the scenario
   file's directory** (absolute paths are refused, because an absolute host path in the run
   identity makes the committed golden untrusted elsewhere). It must also stay *inside*
   that directory: `..` is rejected when the scenario is parsed, and a symlink pointing out
   of the tree is rejected at spawn, after both paths are canonicalized.

### The determinism contract

A flaky scenario is worse than no scenario — it trains everyone to ignore the gate. The
sandbox gives you a fixed locale, timezone, terminal identity and epoch; the scenario owes
the other half:

- **Fixed data.** Point the app at a committed fixture, never at live state.
- **No clock.** Derive any rendered timestamp from `SOURCE_DATE_EPOCH`; `TZ=UTC` alone
  does not stop "3 minutes ago" from changing.
- **No network.** Run offline (`uv run --offline`, `--no-network`, a stub server).
- **No randomness.** Seed it, or mask the region.
- **Fixed geometry.** `[terminal] rows`/`cols` — never inherit the caller's size.
- **Pin the toolchain** that renders (a lockfile), since a library upgrade can change
  glyphs or spacing.

Anything genuinely volatile that you cannot pin — a clock, a duration, a hostname — gets a
`[[mask]]`, not a looser tier.

### Masks

A mask is a row-span rectangle, `row` / `col` / `width` (there is no height — one rect per
row). Declare them scenario-wide as `[[mask]]`, or per frame as `[[steps.mask]]`; a frame
gets both. Masked cells are redacted **before** the capture is hashed, compared, or
rasterized, so a masked region can't fail a compare, can't leak into a committed golden,
and can't destabilize a settle.

> The key is `mask`, singular — `[[mask]]` and `[[steps.mask]]`. `masks` is rejected with
> `unknown field`, exit 2.

## xfail — a governed, expiring waiver

An `xfail` says "this frame is known-broken; don't fail the build yet". It is inline with
the frame, and it is **accountable by construction**:

```toml
[[steps]]
action = "expect_golden"
name = "after-nav"
tier = "cell"

[steps.xfail]
reason  = "status column mis-coloured after the palette refactor"
owner   = "aria"
issue   = "#412"
expiry  = "2026-09-30"                      # canonical YYYY-MM-DD, zero-padded
fingerprint = "a1b2c3…"                     # optional: pins the waiver to ONE diff
```

`reason`, `owner` and `issue` are mandatory and must be non-blank — a blank field is an
authoring error, not a licence to differ. Then:

| Situation | Status | Exit |
|--|--|--|
| The frame still differs, before `expiry` | `xfail` | 0 |
| The frame **matches** again | `xpass` — force-promote | 1 |
| Past `expiry` | `xfail_expired` | 1 |
| Malformed / blank field / non-canonical date | `scenario_error` | 2 |
| `fingerprint` set and the diff is a DIFFERENT one | `fail` | 1 |

Two of these are the point of the design. **`xpass` fails the build**: once the frame is
green again the waiver is a lie, and you must delete it — a waiver can never quietly
outlive the bug. And `fingerprint` scopes the waiver to exactly the diff you inspected, so
a *second, unrelated* regression in the same frame is not silently absorbed by it.

## Blessing is guarded

`--update` and `--on-missing create` write goldens through an approval-gated writer that
refuses when: it detects CI, the golden tree has uncommitted changes, or a **secret scan**
over the reassembled visible frame text trips. Every bless appends to
`<golden-dir>/BASELINE-APPROVAL.md` (who/when/why) and emits a changed-golden manifest —
so a reviewer can see exactly which goldens moved and why. Pass `--reason "…"` to record
intent.

`--update` re-blesses every failing frame; `--update <name>` re-blesses one. A refusal
never downgrades the run's verdict: if a frame genuinely regressed, the run still exits 1
with its per-frame verdicts and heat evidence intact, and the refusal is recorded as a note.

### Redaction

The scan runs over the **visible text** (reassembled from the cell runs, so a secret split
across styled runs or wrapped across lines is still caught), plus the scenario and frame
names and the `--reason`. It reports **rule IDs only, never the matched value** — the
whole point is to avoid copying a secret into a log while complaining about it. There is a
high-entropy backstop for tokens that match no specific rule.

Notes that reach `report.json` are flattened to one line, stripped of control characters,
capped at 240 characters, and replaced wholesale if anything secret-shaped survives. Note
text is sanitized to ASCII at the output boundary, so non-ASCII in a scenario `description`
or `--reason` reaches the reader as `?`.

## Clean up the daemon when you're done

The gate talks to a `shux` daemon and starts one if none is running, so a daemon outlives
your gate run and will show up in any process-hygiene check:

```sh
shux daemon stop        # SIGTERMs this runtime dir's daemon; exit 0 if none is running
```

It is idempotent, so it is safe in a cleanup trap. Set a short private `XDG_RUNTIME_DIR`
(e.g. `/tmp/mygate`) before the run to keep that daemon off any other shux on the machine;
a long path also overruns the Unix-socket length limit. Do **not** pattern-kill `shux`
processes — a broad pattern kills other checkouts' and other agents' daemons, and
`pgrep -f "$XDG_RUNTIME_DIR"` finds nothing at all because the runtime dir is not in the
daemon's argv.

## Gotchas

- **The command must draw and then BLOCK.** A program that prints and exits trips the
  child-exit check before any compare runs. End with something that holds the frame
  (`exec cat`, a real input loop). `expect_exit` is for scenarios that *should* end.
- **Use `hold_settle`, not `settle`, for anything that repaints.** A TUI that clears the
  screen and redraws can be captured mid-repaint by a quiet settle — an intermittent
  whole-screen diff. `hold_ms` settles only when the frame content has held still.
- **A shell fixture starts in canonical mode.** A `read`/`head -c 1` in a `/bin/sh` prop
  won't see a keystroke until a newline, so send `["j", "<CR>"]`, not `["j"]`. A real TUI
  that sets raw mode is unaffected.
- `retries` on `expect_golden` re-settles and re-captures on a mismatch, but a retry can
  only turn FAIL into PASS by **matching the golden** — it can never launder a real
  regression, and divergent failing captures always fail.
- Changing the scenario's structure (steps, command, `cwd`, geometry) changes the run
  identity, so existing goldens become **stale** and must be re-blessed.
- `shux lens gate` is CLI-only. It composes the lens RPCs; there is no `gate.*` method.
