---
name: shux
description: Drive terminal sessions, panes, and TUIs from an agent — spawn shells, send keystrokes, snapshot pixel-perfect PNGs of any pane, and extend shux itself with line-delimited JSON-RPC plugins in any language. Use when you need to multiplex terminal work, drive a TUI you'd otherwise control with tmux / screen / iTerm2 / expect / pexpect / asciinema / vhs / termshot, run scripted CLI/REPL interactions, do headless visual regression on a terminal UI, or write a process plugin that subscribes to the shux event bus and calls back through `window.rename`, `pane.send_keys`, `state.apply`, etc. Trigger phrases include "drive terminal", "spawn pty session", "send keys to a TUI", "screenshot a tui", "snapshot pane", "replace tmux", "iTerm2 automation", "expect script", "headless terminal test", "agent multiplexer", "asciinema record", "write a shux plugin", "extend shux", "shux plugin install".
---

# shux — terminal multiplexer with a JSON-RPC API + pixel snapshotter

## Install

```sh
npx skills add indrasvat/shux --global --yes
```

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

## Where shux artifacts live: `.shux/`

Run `shux init` once per project. It creates a top-level `.shux/` dir:

```
.shux/
├── templates/       # spec.toml files you commit            (committed)
├── scripts/         # automation scripts you commit         (committed)
├── goldens/         # reference PNGs for visual regression  (committed)
├── out/             # snapshots, diffs, logs, anything ephemeral  (gitignored)
└── .gitignore       # ignores `out/`
```

When you write code that produces shux artifacts:

- Put **templates** under `.shux/templates/` (apply with `shux state apply .shux/templates/<name>.toml`).
- Put **driver scripts** under `.shux/scripts/`.
- Write **snapshots, diffs, debug logs** into `.shux/out/` (gitignored by default).
- Commit **golden images** to `.shux/goldens/` so visual-regression diffs have a ground truth.

Never pollute `.claude/`, `~/`, or the project root with shux output.

## 80% quickstart (three RPCs)

```bash
# 1. Spawn a session running any command (or shell). Capture the
#    pane_id from the response so the next calls can target it.
RESP=$(shux --format json session create demo -d -- lazygit)
PID=$(echo "$RESP" | jq -r .pane_id)

# 2. Drive it.
shux pane set-size  -s demo --cols 200 --rows 60
shux pane send-keys -s demo --text 'j'           # text input
shux pane send-keys -s demo --data 'Gw=='        # base64 control (here: Esc)

# 3. Get a PNG back.
shux --format json pane snapshot -s demo \
  | jq -r .png_base64 | base64 -d > frame.png

# Tear down when done.
shux session kill demo
```

Every CLI verb maps 1:1 to an RPC method (RPC dots become CLI spaces —
`session.create` ↔ `shux session create`). Drop to the raw form with
`shux rpc call <method> --params @file` whenever you'd rather write
the payload in JSON directly. `--params -` reads from stdin.

For declarative multi-pane workspaces, use a template:

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
shux state apply spec.toml      # atomic — all or nothing
```

## Tools shux replaces

| If you'd reach for                              | For this job                                  | Use this shux primitive                                  |
|--                                                |--                                              |--                                                          |
| `tmux` · `screen` · `byobu`                      | Multiplex sessions / windows / panes           | `shux state apply spec.toml` · `shux session attach`       |
| iTerm2 (Python SDK / AppleScript)                | Drive a terminal app from outside              | `shux pane send-keys` + `shux pane snapshot`               |
| `expect` · `pexpect` · `sexpect`                 | Scripted CLI / REPL interaction                | `pane send-keys` → `pane wait-for` → `pane capture`        |
| iTerm2 `wait_for_text` / `wait_for_absent`       | Block until screen contains (or stops containing) a needle | `shux pane wait-for` (text · regex · `--absent`)           |
| `asciinema rec` · `script(1)`                    | Record a terminal session                      | `pane.output.watch` (sealed data-plane stream)             |
| `vhs` · `agg` · `terminalizer`                   | Generate TUI demo GIFs / WebPs                 | `shux window snapshot` loop → `ffmpeg`                     |
| `termshot` · `freezeframe`                       | Still PNG of a terminal frame                  | `shux pane snapshot` or `shux window snapshot`             |
| iTerm2 broadcast input                           | Send keystrokes to many panes at once          | `shux pane send-keys` fan-out (one RPC per pane)           |
| `ttyrec` · `termsh`                              | Replay a recorded session                      | Re-feed VT bytes through a fresh pane → `pane snapshot`    |
| GNU parallel `--tmux` mode                       | Run N tasks in N panes, watch in one place     | Template with N panes + RPC orchestrator                   |
| Custom Bubbletea / ratatui test harness          | Visual regression for your TUI                 | `shux window snapshot` + golden-image diff (SSIM or raw RGBA) |

## The common RPC surface

Every CLI verb maps 1:1 to an RPC method — RPC dots become CLI spaces
(`session.create` ↔ `shux session create`, `pane.send_keys` ↔
`shux pane send-keys`). All RPCs accept JSON in, return JSON out, on
stdin/stdout. `references/api.md` lists the full request/response
shape per method.

| Category | Methods                                                                          |
|--        |--                                                                                |
| Session  | `session.create` · `session.list` · `session.rename` · `session.kill` · `session.ensure` |
| Window   | `window.create` · `window.list` · `window.focus` · `window.kill` · `window.ensure` |
| Pane I/O | `pane.send_keys` · `pane.set_size` · `pane.snapshot` · `pane.capture` · `pane.output.watch` |
| Pane mgmt| `pane.split` · `pane.focus` · `pane.zoom` · `pane.swap` · `pane.kill` · `pane.set_title` |
| Window snap | `window.snapshot` · `session.snapshot` (composed multi-pane PNG)            |
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
Need to spawn something?           → session.create  (shux session create -- <cmd>)
Need a multi-pane workspace?       → state.apply     (shux state apply spec.toml)
Need to type into a TUI?           → pane.send_keys  (shux pane send-keys --text|--data)
Need pixel feedback of one pane?   → pane.snapshot   (shux pane snapshot)
Need a snapshot of the whole
window (borders, titles, status)?  → window.snapshot (shux window snapshot)
Need a snapshot of the session's
active window?                     → session.snapshot (shux session snapshot)
Need plain text of the screen?     → pane.capture    (shux pane capture)
Need to block until text appears?  → pane.wait_for   (shux pane wait-for --text|--regex)
Need a stream of PTY output?       → pane.output.watch (event-bus, sealed)
Want raw RPC for a new method?     → shux rpc call <method> --params @file
```

## Extend shux with a process plugin

shux has a process-plugin host. A plugin is any executable that speaks
shux's line-delimited JSON-RPC dialect on stdin/stdout — bash, python,
node, anything. It:

- **Subscribes** to bus events listed in its manifest.
- **Calls** any registered shux RPC method (`window.rename`,
  `pane.send_keys`, `state.apply`, …) to react.
- **Publishes** its own events via `event.publish`. The daemon
  namespaces them under `plugin.<plugin_id>.<type>` so other
  plugins (or `shux events watch --filter plugin.<id>.`) can
  subscribe to them cleanly — see [references/plugins.md](references/plugins.md).
- **Persists** its own state across hot reload via
  `plugin.state.get/set/delete`. The CLI pins it to the calling
  project's root at install time (walks up from cwd for `.shux/`,
  anchors there) — so a daemon shared across checkouts keeps each
  project's state isolated. Atomic writes, 256 KiB cap, per-plugin
  isolation. Path: `<project-root>/.shux/plugins/<name>/state.json`.

```bash
shux plugin install ./my-plugin.sh   # spawn, handshake, register. Hot reload ON
                                      #   by default — saves respawn the plugin
                                      #   in <500ms. Use `--no-watch` to opt out.
shux plugin list                      # name · version · pid · subscribes · watching
shux plugin reload <name>             # manual hot-reload tick (kill + respawn)
shux plugin kill <name>               # graceful shutdown (2s) → SIGKILL
```

Reference plugins:

- [`examples/plugins/hello/plugin.sh`](https://github.com/indrasvat/shux/blob/main/examples/plugins/hello/plugin.sh) — smallest working example (~50 lines): handshake + PTY output + state mutation.
- [`examples/plugins/watcher/plugin.sh`](https://github.com/indrasvat/shux/blob/main/examples/plugins/watcher/plugin.sh) — subscribes to `pane.exited`, emits a namespaced `plugin.watcher.command_exit` via `event.publish` for downstream plugins.

Full protocol — handshake, event payload shape, RPC-out direction,
`event.publish`, shutdown grace, UUID vs name rule, what's not in v0 — lives in
[references/plugins.md](references/plugins.md).

## Deep dives

| Topic | Where |
|--|--|
| Full RPC inventory + JSON request/response shapes | [references/api.md](references/api.md) |
| Apply-template TOML shape, lowering rules, multi-window workspaces | [references/templates.md](references/templates.md) |
| Scenario-driver patterns (send/wait/snap loops, golden-image diff) | [references/scenarios.md](references/scenarios.md) |
| Process plugins — protocol, manifest, event/RPC shapes, gotchas | [references/plugins.md](references/plugins.md) |

## Worked examples

- [examples/headless-tui-test.md](examples/headless-tui-test.md) — drive a TUI in CI, snapshot at every step, diff against checked-in goldens.
- [examples/vision-llm-feedback.md](examples/vision-llm-feedback.md) — agent builds a Bubbletea app, snapshots its own UI, feeds PNG to a vision model, self-corrects.
- [examples/replace-tmux-workflow.md](examples/replace-tmux-workflow.md) — common `tmux new-session / send-keys / capture-pane` patterns translated to shux.

## Gotchas

- `pane.set_size` is synchronous (oneshot ack). The next `pane.snapshot` sees the new dims. No sleep needed between them.
- `pane.snapshot` caps the output at 16M pixels (~4000×4000). Resize first if you'd exceed.
- `pane wait-for -s SESSION` targets the session's **active pane** (often the last-spawned). In multi-pane templates pass `--pane <UUID>` (from `pane list` or `state.apply`'s `spawn_results`) — otherwise the wait will silently watch the wrong pane and time out.
- `pane.send_keys --text` is JSON-quoted text. For raw control bytes (Esc/Enter/Tab/Ctrl+letter), use `--data` with base64.
- `shux state apply foo.toml` atomically commits the graph but PTY spawn outcomes are reported per-pane in `spawn_results`. A spawn failure does not roll back the graph.
- The first pane of the first template window is folded into the session's auto-created initial window — there is no phantom default-shell pane.
- The bundled font (JetBrains Mono Regular, OFL-1.1) is monochrome. Color emoji and CJK glyphs render as `.notdef` tofu in the current rasterizer (P2 roadmap).
