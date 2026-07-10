---
name: shux
description: Drive terminal sessions, panes, and TUIs from an agent — spawn shells, send keystrokes, snapshot pixel-perfect PNGs of any pane, run the lens verify loop (run/settle/glance/diff) to prove a TUI fix actually worked with pixel proof, run deterministic terminal UI QA, and extend shux itself with line-delimited JSON-RPC plugins in any language. Use when you need to multiplex terminal work, drive a TUI you'd otherwise control with tmux / screen / iTerm2 / expect / pexpect / asciinema / vhs / termshot, run scripted CLI/REPL interactions, verify terminal UI layout/keyboard/color behavior, prove a visual bug is fixed before/after, do headless visual regression on a terminal UI, or write a process plugin that subscribes to the shux event bus and calls back through `window.rename`, `pane.send_keys`, `state.apply`, etc. Trigger phrases include "drive terminal", "spawn pty session", "send keys to a TUI", "screenshot a tui", "snapshot pane", "verify a TUI", "TUI QA", "terminal UI regression", "keyboard navigation", "color bleed", "replace tmux", "iTerm2 automation", "expect script", "headless terminal test", "agent multiplexer", "asciinema record", "prove this UI change worked", "diff two frames of a TUI", "wait for a TUI to settle", "run a TUI in the background and glance at it", "write a shux plugin", "extend shux", "shux plugin install".
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
- You're asked to verify terminal UI behavior: layout, alignment, keyboard navigation, color rendering, or screenshot evidence.
- You want declarative workspace templates that apply atomically.

If you're a human at a keyboard and tmux works for you, keep using tmux.
When a human does attach to shux, the normal interactions should still feel
modern: click panes to focus, drag borders to resize, drag visible text to copy
via OSC 52, and right-click a visible selection for the inline Copy / Clear
menu. Reserve prefix copy mode for scrollback, search, and keyboard-only
selection.

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
#    pane_id from the response so the next calls can target it. CLI
#    session creation starts in the caller's current directory unless
#    you pass --cwd.
RESP=$(shux --format json session create demo -d --title demo -- lazygit)
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

## lens — prove a TUI change worked, with pixel proof

You fixed a rendering bug, or built a new TUI screen, and need to *prove*
it — not just "it compiled", but "here's the before/after pixels and the
exact cells that changed". That's `lens`: **run** (spawn hidden, no shell)
→ **settle** (block until the screen stops repainting — no sleeps) →
**glance** (atomic PNG+text+revision of one frame) → drive
(`pane.send_keys`, already above) → **diff** (exactly which cells changed,
with a heat-map PNG). Five commands total, and `shux lens --help` prints
this recipe on demand:

```bash
RUN=$(shux --format json lens run --size 120x30 -- ./my-tui)
PANE=$(echo "$RUN" | jq -r .result.pane_id)

shux pane wait-settled "$PANE" --quiet 300ms --timeout 10s
REV=$(shux --format json pane glance "$PANE" --checkpoint --png before.png | jq -r .result.revision)

shux pane send-keys -s "$(echo "$RUN" | jq -r .result.session_id)" -p "$PANE" --text 'q'
shux pane wait-settled "$PANE" --quiet 300ms --timeout 10s
shux pane diff "$PANE" --since "$REV" --heat delta.png

shux session kill "$(echo "$RUN" | jq -r .result.session_id)"
```

`lens run` spawns into a hidden, quota-bounded, self-cleaning **scratch
session** — excluded from `session list` unless you pass
`--include-scratch`, auto-reaped `--ttl` after the command exits or at
`--max-runtime` regardless. Reach for `lens run` when you're spawning
something *new* to verify; reach for plain `session create` + `pane
snapshot` when you're screenshotting a pane a human (or a longer-lived
workflow) already owns.

Glance/diff output is exactly what's on screen — secrets included, no
automated redaction. Don't glance a pane you didn't spawn yourself unless
the user asks you to.

Full grammar, exit-code table, checkpoint/FIFO semantics, and the
`--wait` signal-death caveat: [references/lens.md](references/lens.md).

## Tools shux replaces

| If you'd reach for                              | For this job                                  | Use this shux primitive                                  |
|--                                                |--                                              |--                                                          |
| `tmux` · `screen` · `byobu`                      | Multiplex sessions / windows / panes           | `shux state apply spec.toml` · `shux session attach`       |
| iTerm2 (Python SDK / AppleScript)                | Drive a terminal app from outside              | `shux pane send-keys` + `shux pane snapshot`               |
| `expect` · `pexpect` · `sexpect`                 | Scripted CLI / REPL interaction                | `pane send-keys` → `pane wait-for` → `pane capture`        |
| iTerm2 `wait_for_text` / `wait_for_absent`       | Block until screen contains (or stops containing) a needle | `shux pane wait-for` (text · regex · `--absent`)           |
| `asciinema rec` · `script(1)`                    | Record a terminal session                      | `shux pane record --to FILE` (lossless raw PTY bytes)      |
| `vhs` · `agg` · `terminalizer`                   | Generate TUI demo GIFs / WebPs                 | `shux window snapshot` loop → `ffmpeg`                     |
| `termshot` · `freezeframe`                       | Still PNG of a terminal frame                  | `shux pane snapshot` or `shux window snapshot`             |
| iTerm2 broadcast input                           | Send keystrokes to many panes at once          | `shux pane send-keys` fan-out (one RPC per pane)           |
| `ttyrec` · `termsh`                              | Replay a recorded session                      | Re-feed VT bytes through a fresh pane → `pane snapshot`    |
| GNU parallel `--tmux` mode                       | Run N tasks in N panes, watch in one place     | Template with N panes + RPC orchestrator                   |
| Custom Bubbletea / ratatui test harness          | Visual regression for your TUI                 | `shux window snapshot` + golden-image diff (SSIM or raw RGBA) |
| Manual "screenshot, eyeball it, screenshot again"| Prove a TUI fix changed exactly what you intended | `shux lens run` → `pane wait-settled` → `pane glance --checkpoint` → fix → `pane diff --since REV --heat` |

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
| Pane I/O | `pane.send_keys` · `pane.set_size` · `pane.snapshot` · `pane.capture` · `pane.output.watch` · `pane.record.start` · `pane.record.stop` |
| Pane mgmt| `pane.split` · `pane.focus` · `pane.zoom` · `pane.swap` · `pane.kill` · `pane.set_title` |
| Window snap | `window.snapshot` · `session.snapshot` (composed multi-pane PNG)            |
| Lens (verify loop) | `lens.run` · `pane.wait_settled` · `pane.glance` · `pane.checkpoint` · `pane.diff_since` — [references/lens.md](references/lens.md) |
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
Need to spawn something?           → session.create  (shux session create --title <label> -- <cmd>)
Need a multi-pane workspace?       → state.apply     (shux state apply spec.toml)
Need to type into a TUI?           → pane.send_keys  (shux pane send-keys --text|--data)
Need pixel feedback of one pane?   → pane.snapshot   (shux pane snapshot)
Need a snapshot of the whole
window (borders, titles, status)?  → window.snapshot (shux window snapshot)
Need a snapshot of the session's
active window?                     → session.snapshot (shux session snapshot)
Need plain text of the screen?     → pane.capture    (shux pane capture)
Need to block until text appears?  → pane.wait_for   (shux pane wait-for --text|--regex)
Need live sampled PTY output?      → pane.output.watch (sealed data-plane, sampled)
Need a byte-exact transcript?      → shux pane record --to FILE (lossless recorder)
Need repeatable TUI QA evidence?   → Sightline verifier; read references/sightline.md
Need to spawn+verify a TUI fix
in one hidden throwaway pane?      → shux lens run -- <argv>  (then wait-settled → glance)
Need to block until a pane stops
repainting (not "process exited")? → pane.wait_settled (shux pane wait-settled <PANE>)
Need atomic PNG+text+revision of
one frame (no glance/capture tear)?→ pane.glance (shux pane glance <PANE> --checkpoint)
Need exactly which cells changed,
with proof?                        → pane.checkpoint + pane.diff_since (shux pane diff <PANE> --since REV)
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

shux plugin grant <name> <method>     # opt the plugin in to a sensitive RPC.
                                      #   Default-deny model: every plugin RPC
                                      #   passes through a permission check
                                      #   before reaching the daemon router
                                      #   (v0.19+).
shux plugin grant <name> <method> --target <id>   # scoped to one entity
shux plugin grant <name> <filter> --subscribe     # widen manifest subscribes
shux plugin revoke <name> <method>    # mirror of grant
shux plugin grants <name>             # show the allow-set
shux plugin audit <name> --tail 50    # tail NDJSON audit log
```

Reference plugins:

- [`examples/plugins/hello/plugin.sh`](https://github.com/indrasvat/shux/blob/main/examples/plugins/hello/plugin.sh) — smallest working example (~50 lines): handshake + PTY output + state mutation.
- [`examples/plugins/watcher/plugin.sh`](https://github.com/indrasvat/shux/blob/main/examples/plugins/watcher/plugin.sh) — subscribes to `pane.exited`, emits a namespaced `plugin.watcher.command_exit` via `event.publish` for downstream plugins.
- [`examples/plugins/conductor/plugin.sh`](https://github.com/indrasvat/shux/blob/main/examples/plugins/conductor/plugin.sh) — VT-poll watchdog **+ settle-snapshot archive ⭐ + window-aggregation OS notifications** for coding-agent panes (claude / codex / opencode / gemini). Identifies the agent on `pane.created`, polls `pane.capture` every 2 s, classifies state, updates the pane border title (`agent · ○|●|✓|!`), auto-dismisses trust prompts via `pane.send_keys`. **On every `ready→idle` transition, calls `pane.snapshot` and saves the resulting PNG to `.shux/conductor/snapshots/` with a rolling `INDEX.tsv` — a feature literally impossible in any tool that doesn't own its own rasterizer.** Tracks per-window in-flight counts and fires ONE `osascript` / `notify-send` notification when a window's last in-flight agent goes idle.

Full protocol — handshake, event payload shape, RPC-out direction,
`event.publish`, shutdown grace, UUID vs name rule, what's not in v0 — lives in
[references/plugins.md](references/plugins.md).

## Deep dives

| Topic | Where |
|--|--|
| Full RPC inventory + JSON request/response shapes | [references/api.md](references/api.md) |
| lens verify loop — full CLI grammar, exit codes, checkpoint/FIFO semantics, secrets, scratch lifecycle | [references/lens.md](references/lens.md) |
| Apply-template TOML shape, lowering rules, multi-window workspaces | [references/templates.md](references/templates.md) |
| Scenario-driver patterns (send/wait/snap loops, golden-image diff) | [references/scenarios.md](references/scenarios.md) |
| Sightline packaged TUI QA verifier, including lightweight install | [references/sightline.md](references/sightline.md) |
| Process plugins — protocol, manifest, event/RPC shapes, gotchas | [references/plugins.md](references/plugins.md) |

## Worked examples

- [examples/lens-verify-loop.md](examples/lens-verify-loop.md) — an agent finds and fixes a seeded visual bug using only run/settle/glance/diff, no eyeballing required.
- [examples/headless-tui-test.md](examples/headless-tui-test.md) — drive a TUI in CI, snapshot at every step, diff against checked-in goldens.
- [examples/vision-llm-feedback.md](examples/vision-llm-feedback.md) — agent builds a Bubbletea app, snapshots its own UI, feeds PNG to a vision model, self-corrects.
- [examples/replace-tmux-workflow.md](examples/replace-tmux-workflow.md) — common `tmux new-session / send-keys / capture-pane` patterns translated to shux.

## Gotchas

- `pane.set_size` is synchronous (oneshot ack). The next `pane.snapshot` sees the new dims. No sleep needed between them.
- `pane.snapshot` caps the output at 16M pixels (~4000×4000). Resize first if you'd exceed.
- `pane wait-for -s SESSION` targets the session's **active pane** (often the last-spawned). In multi-pane templates pass `--pane <UUID>` (from `pane list` or `state.apply`'s `spawn_results`) — otherwise the wait will silently watch the wrong pane and time out.
- `pane.send_keys --text` is JSON-quoted text. For raw control bytes (Esc/Enter/Tab/Ctrl+letter), use `--data` with base64.
- The four lens `pane` verbs (`glance`/`wait-settled`/`checkpoint`/`diff`) take the pane as a **bare positional UUID** — `shux pane glance <PANE>`, not `-p <PANE>`. Every OTHER pane command (`send-keys`, `set-size`, `wait-for`, `snapshot`, `capture`, …) still uses `-s/--session` (+ optional `-w/--window`, `-p/--pane`). Don't mix the two calling conventions up.
- `session kill` and every `-s/--session` flag accept a session NAME **or** a UUID — including the `session_id` a `lens run` response hands you for its scratch session. No need to look the name up first.
- `lens run --wait` on a signal-killed child (e.g. reaped by `--max-runtime`) reports RPC `exit_code: -1`, which the shell sees as `$? == 255` (Unix truncates negative process exits to 8 bits) — 255 there means "never exited on its own", not a literal child exit code.
- `lens run`, `pane glance`, and `pane diff --heat` output can contain whatever the pane displays, including secrets. No automated redaction — don't glance/diff a pane you didn't spawn yourself unless asked to.
- `shux state apply foo.toml` atomically commits the graph but PTY spawn outcomes are reported per-pane in `spawn_results`. A spawn failure does not roll back the graph.
- The first pane of the first template window is folded into the session's auto-created initial window — there is no phantom default-shell pane.
- PNG snapshots use a bundled font chain by default: JetBrains Mono Nerd Font
  for primary monospace metrics and Nerd Font icons, Noto Sans Math for arrows
  such as `↻`, Noto Sans Symbols 2 for braille spinners/status glyphs such as
  `⠹`, Noto Sans Symbols for older technical symbols such as `⎇`, and
  monochrome Noto Emoji for standalone emoji. You normally do not need local
  font config for common TUI glyphs.
- `appearance.font` is the only config knob that changes PNG snapshot cell
  metrics. `appearance.font_fallbacks` is snapshot-only fallback coverage; omit
  it for the default chain. If set, it must be a non-empty ordered list of
  builtin tokens (`builtin:nerd-font`, `builtin:math`, `builtin:symbols`,
  `builtin:symbols-legacy`, `builtin:emoji`) or font paths. If `font` is unset,
  bundled JetBrains Mono still anchors metrics.
- Still not full terminal font parity: color emoji, composed emoji/ZWJ
  sequences, ligatures, RTL shaping, CJK/system-font discovery, and platform
  font fallback are renderer-v2 work. For those, trust live attach or keep
  visual tests scoped to currently supported scalar glyphs.
