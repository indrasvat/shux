# Change request B — the visual gate went red

You are working in this directory. It is a small Python terminal UI (a deploy board)
run with `uv`. It is a git repo.

A teammate pushed the most recent commit — a refactor that centralizes the status
colours into a `palette` module. They described it as purely internal, with **no
behaviour change**.

Since that commit landed, this project's visual regression gate is **red**. The gate
compares the board's rendered frames against a committed baseline (see `scenario.toml`
and `goldens/`).

**Your job:** work out what the refactor actually changed on screen, and fix it, so the
board renders exactly as it did before that commit.

The baseline in `goldens/` is correct and is not up for debate — it records how the
board is supposed to look. The refactor was meant to change nothing visible, so the code
is what has to move, not the baseline.

You are done when the visual gate is green and the committed baseline is untouched.
