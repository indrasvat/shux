# Example: gating a TUI against regressions in CI

You're building a TUI (Bubbletea, ratatui, Charm, curses — doesn't matter). You want every
PR to fail if the rendering drifts. This is the full lifecycle: scaffold, baseline, wire CI,
bless an intended change, catch a real regression.

No display server, no macOS runner, no iTerm2, no `sleep`. Full grammar lives in
[../references/gate.md](../references/gate.md); this is the path through it.

## 1. Scaffold

```bash
shux lens gate init myapp        # a NAME, not a filename — writes ./myapp.toml
```

It writes a runnable scenario against a placeholder command and mints its first golden, so
you can confirm the machinery works before touching your own app.

## 2. Point it at your app

Edit `myapp.toml`. The three things that trip everyone are `command`, `[env] PATH`, and `cwd`:

```toml
name = "myapp"
description = "Board loads, then a j-key selection move."
command = ["myapp", "--fixtures", "tests/fixtures"]
cwd = "."          # relative to THIS file; the child otherwise starts in a temp dir

[env]
# The sandbox env starts EMPTY. Your app is invisible unless its dir is on this PATH.
PATH = "/usr/local/bin:/usr/bin:/bin"

[terminal]
cols = 100
rows = 30

[[steps]]
action = "wait_for_text"
text = "myapp"            # something only the loaded frame draws
timeout_ms = 20000

[[steps]]
action = "hold_settle"    # capture only once the frame has stopped changing
hold_ms = 300
timeout_ms = 10000

[[steps]]
action = "expect_golden"
name = "01_loaded"
tier = "cell"

[[steps]]
action = "keys"
keys = ["j"]

[[steps]]
action = "hold_settle"
hold_ms = 300
timeout_ms = 10000

[[steps]]
action = "expect_golden"
name = "02_after_j"
tier = "cell"
```

**Your command must draw and then block.** A program that prints and exits trips the
child-exit check before any frame is compared. A real input loop does this naturally; use
`expect_exit` for scenarios that are *supposed* to end.

## 3. Baseline, then actually look at it

```bash
shux lens gate myapp.toml                                  # exit 1: no committed golden
shux lens gate myapp.toml --on-missing create --reason "first baseline"
```

Goldens land in `goldens/myapp/`. **Open them before committing** — a baseline is a claim
that this is what correct looks like, and everything downstream trusts it:

```bash
shux lens gate review myapp.toml       # renders each frame; PNG paths when inline graphics aren't available
git add goldens/ myapp.toml && git commit -m "test: gate myapp rendering"
```

## 4. Wire CI

The whole check is one command keyed on its exit code:

```yaml
# .github/workflows/ci.yml
- run: curl -fsSL https://shux.pages.dev/install.sh | sh
- run: shux lens gate myapp.toml --report gate-report.json
- if: failure()
  uses: actions/upload-artifact@v4
  with:
    name: gate-evidence
    path: |
      gate-report.json
      .shux/out/myapp/          # heat PNGs — the changed cells, marked
```

Nothing else is needed. A missing golden is a regression (exit 1), never a silent pass, so
CI can't mint its own baseline — and `--update` is refused in CI outright.

## 5. You change the UI on purpose

The gate goes red. That is correct: you changed what it renders. Confirm the diff is *only*
what you meant, then re-bless:

```bash
shux lens gate myapp.toml --update --reason "new footer status bar"
git add goldens/ && git commit
```

Every bless appends to `goldens/myapp/BASELINE-APPROVAL.md` (who, when, why), so review sees
which frames moved and on what grounds. Blessing runs a secret scan over the visible frame
text first, and is refused on a dirty golden tree.

## 6. Something regresses by accident

```
lens gate  scenario=myapp  verdict=fail  frames=2  time=2.8s
FRAME       | STATUS | CHANGED | DETAIL
------------+--------+---------+-------
01_loaded   | fail   | 50      |
02_after_j  | fail   | 50      |
```

Read it in this order:

1. `.shux/out/myapp/01_loaded.heat.png` — the changed cells, marked. Usually enough.
2. `report.json` → `diff.regions` — the row and column span of every changed run.
3. `report.json` → `diff.style_deltas` — expected-vs-actual **colour** per run. This is the
   field that matters when the text is byte-identical and only styling moved; a text diff
   and your eyes both miss that, and it is the single most common silent TUI regression.

**Do not reach for `--update` to make it green.** If you did not mean to change the UI,
blessing hides the bug and commits it as the new truth.

## 7. Clean up

The gate starts a daemon if none is running, and it outlives the run:

```bash
shux daemon stop      # idempotent — exit 0 when none is running
```

Set a short private `XDG_RUNTIME_DIR` before the run so this stays scoped to your own
daemon. Never `pkill -f shux` — it kills other checkouts and other agents.

## Already have a bash + python snapshot harness?

Don't port it by hand — [../references/scenarios.md](../references/scenarios.md) maps every
piece of it (`sleep` → settle, PIL comparator → cell tier, `cp` → `--update`) onto the
scenario above.
