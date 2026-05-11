---
name: shux
description: Drive terminal sessions, panes, and TUIs from an agent — spawn shells, send keystrokes, snapshot pixel-perfect PNGs of any pane. Use when you need to multiplex terminal work, drive a TUI you'd otherwise control with tmux / screen / iTerm2 Python SDK / expect / pexpect / asciinema / vhs / termshot, run scripted CLI/REPL interactions, or do headless visual regression on a terminal UI. Trigger phrases include "drive terminal", "spawn pty session", "send keys to a TUI", "screenshot a tui", "snapshot pane", "replace tmux", "iterm2 driver", "expect script", "headless terminal test", "agent multiplexer", "asciinema record".
---

# shux — terminal multiplexer with a JSON-RPC API + pixel snapshotter

shux is a Rust terminal multiplexer (sessions / windows / panes, like tmux)
that **also** exposes a length-prefixed JSON-RPC surface over UDS and TCP,
atomic declarative templates, optimistic concurrency on every entity, a
sealed event bus, and a built-in rasterizer that returns PNG bytes for any
pane — no terminal emulator in the loop.

## When to reach for it

Pick shux instead of the alternatives when **any** of these apply:

- You need to drive a TUI from outside (agent, CI, script) without a human at the keyboard.
- You want a PNG of what a terminal looks like *right now*, headless, no display server.
- You're running scripted CLI/REPL interactions that need typed keystrokes and known wait points.
- You're doing visual regression on a TUI you built (Bubbletea, Charm, ratatui, anything).
- You want declarative workspace templates that apply atomically.

If you're a human at a keyboard and tmux works for you, keep using tmux. shux exists for the cases tmux's contract doesn't reach.

## 80% quickstart (three RPCs)

```bash
# 1. Spawn a session running any command (or shell). Returns pane_id.
shux api session.create '{"name":"demo","command":["vivecaka","--repo","cli/cli"]}'

# 2. Drive it.
shux api pane.set_size  '{"pane_id":"$PID","cols":200,"rows":60}'
shux api pane.send_keys '{"pane_id":"$PID","text":"j"}'                  # text input
shux api pane.send_keys '{"pane_id":"$PID","data":"Gw=="}'               # base64 control (here: Esc)

# 3. Get a PNG back.
shux api pane.snapshot  '{"pane_id":"$PID"}' \
  | jq -r .result.png_base64 | base64 -d > frame.png

# Tear down when done.
shux kill -s demo
```

That covers a huge chunk of real workflows. For declarative multi-pane workspaces, use a template:

```toml
# spec.toml
[session]
name = "review"
[[windows]]
title = "vivecaka"
[[windows.panes]]
command = ["vivecaka", "--repo", "cli/cli"]
```

```bash
shux apply spec.toml      # atomic — all or nothing
```

## Tools shux replaces

| If you'd reach for                              | For this job                                  | Use this shux primitive                                  |
|--                                                |--                                              |--                                                          |
| `tmux` · `screen` · `byobu`                      | Multiplex sessions / windows / panes           | `shux apply spec.toml` · `shux attach`                     |
| iTerm2 Python SDK · AppleScript drive            | Drive a terminal app from outside              | `pane.send_keys` + `pane.snapshot`                         |
| `expect` · `pexpect` · `sexpect`                 | Scripted CLI / REPL interaction                | Loop of `send_keys` / `wait` / `snapshot`                  |
| `asciinema rec`                                  | Record a terminal session                      | `pane.output.watch` (sealed data-plane stream)             |
| `vhs` · `agg` · `terminalizer`                   | Generate TUI demo GIFs / WebPs                 | `pane.snapshot` loop → `ffmpeg`                            |
| `termshot` · `freezeframe`                       | Still PNG of a terminal frame                  | `pane.snapshot`                                            |
| iTerm2 broadcast input                           | Send keystrokes to many panes at once          | `pane.send_keys` fan-out (one RPC per pane)                |
| `ttyrec` · `termsh`                              | Replay a recorded session                      | Re-feed VT bytes through a fresh pane → `pane.snapshot`    |
| GNU parallel `--tmux` mode                       | Run N tasks in N panes, watch in one place     | Template with N panes + RPC orchestrator                   |
| Custom Bubbletea / ratatui test harness          | Visual regression for your TUI                 | `pane.snapshot` + golden-image diff (SSIM or raw RGBA)     |

## The full RPC surface (compact)

Each method maps 1:1 to a `shux` CLI subcommand. All accept JSON in,
return JSON out, on stdin/stdout.

| Category | Methods                                                                          |
|--        |--                                                                                |
| Session  | `session.create` · `session.list` · `session.rename` · `session.kill` · `session.ensure` |
| Window   | `window.create` · `window.list` · `window.focus` · `window.kill` · `window.ensure` |
| Pane I/O | `pane.send_keys` · `pane.set_size` · `pane.snapshot` · `pane.capture` · `pane.output.watch` |
| Pane mgmt| `pane.split` · `pane.focus` · `pane.zoom` · `pane.swap` · `pane.kill` · `pane.set_title` |
| State    | `state.apply` (atomic batch) · `events.history` · `system.version`               |

Every entity carries a `version` field. Pass `expected_version` on
mutating RPCs to get optimistic-concurrency rejection (error code
`-32002`) on stale writes — useful when multiple agents collaborate.

## Common control bytes for `pane.send_keys --data`

The `text` field sends raw text. For control characters, use `data` (base64).

| Key       | Bytes  | Base64       |
|--         |--      |--            |
| Enter     | `0x0d` | `DQ==`       |
| Escape    | `0x1b` | `Gw==`       |
| Tab       | `0x09` | `CQ==`       |
| Backspace | `0x7f` | `fw==`       |
| Ctrl+C    | `0x03` | `Aw==`       |
| Ctrl+L    | `0x0c` | `DA==`       |
| Up arrow  | `\e[A` | `G1tB`       |
| Down arrow| `\e[B` | `G1tC`       |

## Decide which method to use

```
Need to spawn something?        → session.create (with `command`)
Need a multi-pane workspace?    → state.apply or shux apply spec.toml
Need to type into a TUI?        → pane.send_keys (text= or data=)
Need pixel feedback?            → pane.snapshot (returns base64 PNG)
Need plain text of the screen?  → pane.capture (returns ANSI-stripped text)
Need a stream of PTY output?    → pane.output.watch (event-bus stream)
```

## Deep dives

| Topic | Where |
|--|--|
| Full RPC inventory + JSON request/response shapes | [references/api.md](references/api.md) |
| Apply-template TOML shape, lowering rules, multi-window workspaces | [references/templates.md](references/templates.md) |
| Scenario-driver patterns (send/wait/snap loops, golden-image diff) | [references/scenarios.md](references/scenarios.md) |

## Worked examples

- [examples/headless-tui-test.md](examples/headless-tui-test.md) — drive a TUI in CI, snapshot at every step, diff against checked-in goldens.
- [examples/vision-llm-feedback.md](examples/vision-llm-feedback.md) — agent builds a Bubbletea app, snapshots its own UI, feeds PNG to a vision model, self-corrects.
- [examples/replace-tmux-workflow.md](examples/replace-tmux-workflow.md) — common `tmux new-session / send-keys / capture-pane` patterns translated to shux.

## Gotchas

- `pane.set_size` is synchronous (oneshot ack). The next `pane.snapshot` sees the new dims. No sleep needed between them.
- `pane.snapshot` caps the output at 16M pixels (~4000×4000). Resize first if you'd exceed.
- `pane.send_keys --text` is JSON-quoted text. For raw control bytes (Esc/Enter/Tab/Ctrl+letter), use `--data` with base64.
- `shux apply foo.toml` atomically commits the graph but PTY spawn outcomes are reported per-pane in `spawn_results`. A spawn failure does not roll back the graph.
- The first pane of the first template window is folded into the session's auto-created initial window — there is no phantom default-shell pane.
- The bundled font (JetBrains Mono Regular, OFL-1.1) is monochrome. Color emoji and CJK glyphs render as `.notdef` tofu in the current rasterizer (P2 roadmap).
