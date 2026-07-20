# Migrating a `sleep`-driven snapshot harness to a gate scenario

If you already drive a TUI from bash — spawn, `sleep 3`, send a key, `sleep 0.5`,
snapshot, then diff PNGs in python — **stop maintaining that harness**. It is what
`shux lens gate` replaces, and the gate is strictly better at the two things the bash
version cannot do: it waits on *frame content* instead of guessing with `sleep`, and it
compares **cell semantics including colour** rather than pixels through a tolerance.

New to the gate? Read [gate.md](gate.md) first — this file is only the conversion.

## The 1:1 conversion

| Your bash harness | The scenario equivalent |
|--|--|
| `shux session create … -- mytui --fixtures ...` | `command = ["mytui", "--fixtures", "..."]` |
| `SOURCE_DATE_EPOCH=… ` + a hand-pruned env | `[env]` (the sandbox starts EMPTY; `SOURCE_DATE_EPOCH=0` is already set) |
| running the tool from the project dir | `cwd = "."` — relative to the scenario file |
| `pane set-size --cols 160 --rows 48` | `[terminal] cols = 160`, `rows = 48` |
| `sleep 3` after spawn | `wait_for_text` on something the first frame draws |
| `sleep 0.5` between keys | `hold_settle` — settles when the frame stops changing |
| `send-keys --text 'j'` | `{ action = "type_text", text = "j" }` |
| `send-keys --data "$(printf '\r' \| base64)"` | `{ action = "keys", keys = ["<CR>"] }` — vim notation, no base64 |
| `snap 01_loaded` → `.shux/out/01_loaded.png` | `{ action = "expect_golden", name = "01_loaded", tier = "cell" }` |
| numpy/PIL `max pixel diff > 2` comparator | built in — the `cell` tier, exact and portable |
| `cp .shux/out/*.png .shux/goldens/` | `shux lens gate scn.toml --update --reason "…"` (audited, secret-scanned) |
| `sys.exit(1)` on mismatch | the frozen exit contract (`0/1/2/3/5/6`) |
| a `.png` per label in git | one `.capture.json` per frame — text + colour, diffable in review |

Two rows deserve emphasis, because they are the reason the harness was fragile:

- **Every `sleep` becomes a settle.** A fixed sleep is either too short (you capture a
  half-painted frame — the classic intermittent whole-screen diff) or too slow. Use
  `wait_for_text` to reach a known state and `hold_settle` to capture one.
- **The pixel comparator becomes the `cell` tier.** Pixel tolerance exists to absorb
  anti-aliasing drift, and that same tolerance is what silently swallows a
  `bright_green` → `green` regression. Cell compare has no tolerance to hide in.

## Worked conversion

The old harness — spawn, settle, snap, press `j`, settle, snap:

```bash
shux session create visual-test -d -- mytui --fixtures tests/fixtures
shux pane set-size -s visual-test --cols 160 --rows 48
sleep 3    ; snap 01_loaded
shux pane send-keys -s visual-test --text 'j'
sleep 0.5  ; snap 02_after_j
# …then ~30 lines of python comparing PNGs to .shux/goldens/
```

The same scenario, complete:

```toml
name = "mytui"
description = "Initial frame, then a j-key selection move."
command = ["mytui", "--fixtures", "tests/fixtures"]
cwd = "."

[env]
PATH = "/usr/local/bin:/usr/bin:/bin"   # the sandbox PATH does NOT inherit yours

[terminal]
cols = 160
rows = 48

[[steps]]
action = "wait_for_text"
text = "my tui"          # something only the loaded frame draws
timeout_ms = 20000

[[steps]]
action = "hold_settle"   # replaces `sleep 3`
hold_ms = 300
timeout_ms = 10000

[[steps]]
action = "expect_golden"
name = "01_loaded"
tier = "cell"

[[steps]]
action = "type_text"
text = "j"

[[steps]]
action = "hold_settle"   # replaces `sleep 0.5`
hold_ms = 300
timeout_ms = 10000

[[steps]]
action = "expect_golden"
name = "02_after_j"
tier = "cell"
```

Then, once:

```bash
shux lens gate scn.toml --on-missing create --reason "first baseline"  # mint + LOOK at them
git add goldens/ && git commit
```

and in CI, forever after: `shux lens gate scn.toml`.

Delete the bash driver and the python comparator. The scenario is the test.

## What is still a driver script

The gate owns *repeatable regression*. A few jobs remain plain RPC work:

- **One-off exploration** — spawn, look, adjust. That is the lens loop; see [lens.md](lens.md).
- **Broadcast input** — send one keystroke to every pane at once, the iTerm2-broadcast
  equivalent:

  ```bash
  for pid in $(shux --format json pane list -s "$SESSION" | jq -r '.[].id'); do
    shux rpc call pane.send_keys --params "{\"pane_id\":\"$pid\",\"text\":\"ls\\n\"}" >/dev/null &
  done
  wait
  ```

- **Reacting to a pane exiting** — subscribe rather than poll:
  `shux events watch --filter pane.exited`, or a process plugin ([plugins.md](plugins.md)).
