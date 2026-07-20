# Change request A — footer status bar

You are working in this directory. It is a small Python terminal UI (a deploy board)
run with `uv`. It is a git repo; commit your work as you would normally.

**The change:** add a footer status bar under the existing summary line, showing the
keys the board supports — `j`/`k` to move the selection, `r` to refresh, `q` to quit.
Match the surrounding style.

**One constraint:** this project guards its terminal UI with a committed visual
regression baseline (see `scenario.toml` and `goldens/`). Your change is *intended* to
alter what the board looks like, so the baseline needs to end up reflecting the new
appearance — but nothing other than your footer may change.

You are done when the change is implemented, the visual gate is green, and the recorded
baseline shows your footer and nothing else.
