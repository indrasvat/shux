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
shux lens gate init scenario.toml            # scaffold a new scenario + first goldens
```

## Exit codes (frozen contract — key CI on these)

| Code | Meaning |
|--|--|
| 0 | pass |
| 1 | regression (a frame differs, is missing, or is stale) |
| 2 | usage / scenario error |
| 3 | infra error (couldn't spawn, quota, …) |
| 5 | the child process died |
| 6 | update refused (in CI, dirty golden tree, or a secret was detected) |

A frame with **no committed golden is a regression** (exit 1), not a silent pass. That is
deliberate: a golden can never be self-minted in CI.

## The scenario file

```toml
name = "mock-rich-tui"
description = "Deploy board: initial frame, then a j-key selection move."
command = ["uv", "run", "--offline", "board.py"]
cwd = "."                    # OPTIONAL, relative to THIS file's directory
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

### Steps

| Action | Fields |
|--|--|
| `wait_for_text` | `text`, `timeout_ms` |
| `wait` | `ms` |
| `settle` | `quiet_ms`, `timeout_ms` — quiet-based; fine for a TUI that stops painting |
| `hold_settle` | `hold_ms`, `quiet_ms`, `timeout_ms` — settles when the frame CONTENT is unchanged for `hold_ms`. **The right default for anything that repaints** |
| `stable_frames` | `n`, `quiet_ms`, `timeout_ms` — `n` contiguous identical revisions; for a *continuous* repainter only (a static TUI never produces `n` new revisions → `settle_never_stable`) |
| `type_text` / `paste` | `text` |
| `keys` | `keys` — vim notation, e.g. `["j", "<C-c>", "gg"]` |
| `resize` | `rows`, `cols` |
| `expect_golden` | `name`, `tier`, optional `retries`, `hold_ms`, `stable_frames`, `quiet_ms`, `timeout_ms`, `masks`, `xfail` |
| `assert_contains` / `assert_not_contains` | `text` |
| `expect_exit` | `code` |

A scenario with **no `expect_golden` proves nothing** and is refused (exit 2).

### Tiers

- `cell` — the canonical grid: characters **and colour/attributes**. Portable across
  machines. Start here.
- `pixel` — rasterized pixels, with a tolerance recorded in the blessed sidecar. Baselines
  are per-OS/arch.
- `exact` — byte-exact pixels.

## The normal loop

```bash
# 1. Write scenario.toml (or: shux lens gate init scenario.toml)
# 2. First run: no goldens yet -> exit 1, "no committed golden"
shux lens gate scenario.toml

# 3. Mint the first goldens, then LOOK at them before committing
shux lens gate scenario.toml --on-missing create --reason "first baseline"
git add goldens/ && git commit

# 4. In CI, from now on:
shux lens gate scenario.toml            # exit 0 while nothing moves

# 5. A change lands. If the diff is a REGRESSION -> fix the code and re-run.
#    If the change was INTENDED -> re-bless it:
shux lens gate scenario.toml --update --reason "new footer status bar"
```

**Blessing is for intended changes only.** If the gate is red and you did not mean to
change the UI, `--update` hides the bug instead of fixing it. Read the heat PNG first.

## Reading a failure

```
lens gate  scenario=mock-rich-tui  verdict=fail  frames=2  time=2.8s
FRAME     | STATUS | CHANGED | DETAIL
----------+--------+---------+-------
start     | fail   | 50      |
after-nav | fail   | 50      |
```

- `--out DIR` (default `.shux/out/<scenario>/`) receives a **heat PNG per failing frame**
  showing the changed cells. Open it — it localizes the regression instantly.
- `--report PATH|-` writes `report.json`: per frame a `status`, `diff.changed_cells`,
  `diff.regions` (row + column span of every changed run), `diff.heat_png`, and
  `diff.style_deltas` — **what** changed, not just where:

  ```json
  "style_deltas": [
    {"row": 4, "col": 25, "expected": "fg=bright_green", "actual": "fg=green"},
    {"row": 5, "col": 25, "expected": "fg=bright_green", "actual": "fg=green"}
  ]
  ```

  This is the field to read for a **colour-only** regression: the text is byte-identical,
  so a text diff shows nothing and the coordinates alone don't say what moved. One entry
  per contiguous run of the same change, capped so a full-screen recolour can't bloat the
  report. Absent when only text changed.
- `--cast [PATH]` records a replayable asciinema v2 file of the whole run, so you can
  scrub how the TUI reached a failing frame.
- `--trace PATH|-` emits the raw runner-signal NDJSON.

`diff.regions` is usually enough to localize without opening anything: a fail confined to
`{"row": 4, "col_start": 25, "col_end": 35}` on several rows is one column of one table.

## The sandbox — read this before your first scenario

The child starts from an **empty environment** in an isolated HOME/XDG, with
`LC_ALL=C.UTF-8`, `TZ=UTC`, `TERM=xterm-256color`, `COLORTERM=truecolor`,
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
   identity makes the committed golden untrusted elsewhere).

Determinism is the whole game: fixed data, no clock, no network, no randomness. Derive any
timestamp from `SOURCE_DATE_EPOCH`. A flaky scenario is worse than no scenario.

## Blessing is guarded

`--update` and `--on-missing create` write goldens through an approval-gated writer that
refuses when: it detects CI, the golden tree has uncommitted changes, or a **secret scan**
over the visible frame text trips. Every bless appends to `BASELINE-APPROVAL.md`
(who/when/why) and emits a changed-golden manifest — so a reviewer can see exactly which
goldens moved and why. Pass `--reason "…"` to record intent.

## Clean up the daemon when you're done

The gate talks to a `shux` daemon, and starts one if none is running. There is **no
`shux daemon stop` verb yet**, so a daemon outlives your gate run:

```sh
pkill -f "shux __daemon"
```

This matters if you run your own no-leak / process-hygiene check after a gate invocation —
the daemon will show up as a new process and trip it. Use an isolated, SHORT
`XDG_RUNTIME_DIR` (e.g. `XDG_RUNTIME_DIR=/tmp/mygate`) to keep that daemon away from any
other shux on the machine; a long path can also blow the Unix-socket path limit.

## Gotchas

- **The command must draw and then BLOCK.** A program that prints and exits trips the
  child-exit check before any compare runs. End with something that holds the frame
  (`exec cat`, a real input loop). `expect_exit` is for scenarios that *should* end.
- **Use `hold_settle`, not `settle`, for anything that repaints.** A TUI that clears the
  screen and redraws can be captured mid-repaint by a quiet settle — an intermittent
  whole-screen diff. `hold_ms` settles only when the frame content has held still.
- `retries` on `expect_golden` re-settles and re-captures on a mismatch, but a retry can
  only turn FAIL into PASS by **matching the golden** — it can never launder a real
  regression, and divergent failing captures always fail.
- Changing the scenario's structure (steps, command, `cwd`, geometry) changes the run
  identity, so existing goldens become **stale** and must be re-blessed.
- Frames are captured with masks applied; use `masks` on `expect_golden` (or a top-level
  `[[mask]]`) for genuinely volatile regions rather than loosening the tier.
