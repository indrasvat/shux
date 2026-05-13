# shux-conductor — design doc

> **Status:** Draft for review
> **Owner:** indrasvat
> **Phase:** M2 (packaged plugins)
> **Spikes:** `.shux/out/spikes/SPIKES.md`
> **Prior art:** [cmux](https://cmux.com/), [dmux](https://dmux.ai/),
> [amux](https://github.com/mixpeek/amux), [Superset](https://superset.sh/),
> [Zed ACP](https://zed.dev/acp)

## 1. Problem

Coding-agent orchestrators (cmux, dmux, amux, Superset) cluster
around the same primitives — isolated git worktrees per agent,
watchdog recovery, status notifications, agent-to-agent comms.
Every one of them ships as a *separate program* (a TUI app, an
Electron desktop, a web server + dashboard). shux already has the
substrate to do this work as **a single process plugin** living
inside a normal shux session, with two capabilities no other tool
has:

1. **Pixel-perfect snapshots** of any pane at any moment
   (`pane.snapshot` → PNG bytes).
2. **Sealed byte-level PTY transcripts** (`pane.output.watch` →
   base64 chunks with sequence numbers).

These let conductor offer an **agent-settle archive** (PNG + JSON
transcript per agent, per turn) — a feature literally impossible in
any tool that doesn't own its own rasterizer.

Goal: a packaged plugin, `plugins/conductor/`, that brings the
useful subset of dmux/amux/Superset's feature surface to shux while
exploiting shux's unique primitives. Installable in 30 s. Hot
reloadable. ~300 lines per phase, no extra binaries.

## 2. Non-goals (v0)

- **Web UI / dashboard.** Conductor is shux-native; status surfaces
  through shux's status bar, per-pane title overlays, and `events
  watch`. A web bridge is a separate plugin (`shux-conductor-web`)
  that could compose on top, post-v1.
- **Cloud sync / multi-machine.** Single-host only.
- **Replacing tmux/dmux/amux feature parity 1:1.** Conductor picks
  the *highest-leverage* subset.
- **Cost / token tracking.** Punt to v1.x — needs per-agent meter
  hooks we don't yet have a clean place for.

## 3. Architecture

```
┌──────────────────────────────────────────────────────────┐
│  shux daemon                                              │
│                                                            │
│  ┌────────────────┐    ┌────────────────────────────┐    │
│  │  event bus     │    │  RPC registry              │    │
│  │  + state graph │◄───┤  pane.* / window.* / ...   │    │
│  └────┬───────────┘    └────────────────────────────┘    │
│       │ subscribes / emits                                │
└───────┼──────────────────────────────────────────────────┘
        │ line-delimited JSON-RPC over stdio (plugin protocol)
        │
┌───────▼──────────────────────────────────────────────────┐
│  conductor plugin process                                 │
│                                                            │
│  ┌────────────────────────┐  ┌──────────────────────┐   │
│  │  VT-poll loop          │  │  ACP client (v0.5+)  │   │
│  │  (always-on baseline)  │  │  per-agent ACP child │   │
│  │                        │  │                      │   │
│  │  - pane.capture every  │  │  - spawn agent --acp │   │
│  │    POLL_MS             │  │  - parse session/    │   │
│  │  - regex-match agent   │  │    update events     │   │
│  │    state (idle /       │  │  - re-emit on bus as │   │
│  │    thinking / stuck /  │  │    plugin events     │   │
│  │    rate-limited)       │  │  - route tool-calls  │   │
│  │  - dismiss prompts     │  │    through shux RPCs │   │
│  │  - send /compact       │  │                      │   │
│  └────────────────────────┘  └──────────────────────┘   │
│            │                            │                 │
│            └────┬───────────────────────┘                 │
│                 │ state transition events                 │
│                 ▼                                          │
│  ┌─────────────────────────────────────────────────────┐│
│  │  side effects (per transition)                       ││
│  │  - pane.set_title <agent · state>                    ││
│  │  - on idle→settled: pane.snapshot →                  ││
│  │      .shux/conductor/snapshots/<agent>-<ts>.png      ││
│  │  - if all panes idle: notify (osascript/notify-send) ││
│  └─────────────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────┘

artifacts on disk:
  .shux/conductor/
    config.toml          — per-project (committed)
    board.toml           — task board (committed; agents claim atomic)
    notes.md             — cross-agent message log (committed)
    snapshots/           — settle PNGs (gitignored by default)
    transcripts/         — NDJSON byte streams per agent (gitignored)
```

The plugin is a long-lived process spawned by `shux plugin install
plugins/conductor/plugin.sh`. It subscribes to `pane.created`,
`pane.exited`, and `window.*` events from the bus. For every pane
running a known agent command (claude / codex / opencode / gemini)
it spins up a per-pane watchdog goroutine (or, in shell, a per-pane
background poll loop).

## 4. Detection model

The watchdog tracks per-pane state in a small FSM:

```
        ┌─────────┐ pane.created
        │ unknown │◄────────────┐
        └────┬────┘              │
             │ splash pattern    │ pane.exited
             ▼                   │
        ┌─────────┐               │
        │ ready   │───stuck-prompt-pattern──┐
        └────┬────┘                          │
   thinking-pattern                          │
             │                                │
             ▼                                │
        ┌──────────┐ idle-pattern  ┌─────────▼─┐
        │ thinking │─────────────► │  stuck    │
        └──────────┘                └───┬───────┘
                                        │ Enter sent
                                        ▼
                                  back to ready

  rate-limit-pattern from any state → ┌──────────┐
                                       │ paused   │ resume after parsed reset_time
                                       └──────────┘
```

Patterns live in `plugins/conductor/lib/patterns/<agent>.toml` so
agent-version drift can be fixed without re-deploying the plugin.

State transitions trigger side effects:
- **any → settled** (defined as idle for `SETTLE_MS`, default 5 s):
  `pane.snapshot` to `.shux/conductor/snapshots/`, plus update
  border title to `<agent> · ✓`.
- **any → stuck**: `pane.send_keys --data DQ==` (Enter).
- **any → thinking**: update border title to `<agent> · ⚡`.
- **all panes in window → settled**: emit one OS notification.

## 5. Phased delivery

Each phase is a self-contained PR. Phase N+1 doesn't require
phase N+1 features — earlier phases stay useful even if the
later ones aren't merged yet.

### v0.1 — VT-poll watchdog (single agent)

- Subscribe to `pane.created`; identify the agent by inspecting
  `command[]` against a registry of known agent prefixes.
- Poll `pane.capture` every 2 s; classify state from text patterns.
- Dismiss stuck-on-prompt by sending `Enter` (configurable).
- Set per-pane title to `<agent> · <state-emoji>`.
- DoD: a single claude pane in a shux session shows ✓ in its title
  after going idle, and ⚡ during a typed-prompt response. Verified
  visually via `pane.snapshot`.

### v0.2 — Settle-snapshot archive ⭐ (shux-unique)

- On idle→settled transition, `pane.snapshot` to
  `.shux/conductor/snapshots/<agent>-<ISO-8601>.png`.
- Maintain a rolling index `.shux/conductor/snapshots/INDEX.tsv` with
  one line per snapshot for fast review.
- DoD: drive a 3-agent demo (re-use `three_agent_split_shoot.sh`
  pattern), let each settle, verify 3 PNGs land in the snapshots/
  dir with sane filenames and the index file.

### v0.3 — Multi-pane state + notifications

- Track state across every pane in every window of every session
  the daemon knows about.
- Emit one OS notification per *window* the moment its last
  non-idle pane goes idle.
- DoD: a 3-pane window goes silent ⇒ exactly one
  `osascript`/`notify-send` invocation fires.

### v0.4 — Worktree-per-pane

- On pane.created for a known agent command, if the pane's CWD is
  inside a git repo and conductor is configured for worktrees,
  create a worktree at `.shux/conductor/worktrees/<pane-short>` on
  a branch named `agent/<pane-short>` and `cd` the pane into it.
- DoD: spawning two agent panes in the same repo yields two distinct
  worktrees + branches; the second agent's edits don't conflict with
  the first's.

### v0.5 — ACP fast-path (opencode + gemini first)

- For agents that report `--acp` capability, additionally spawn the
  agent's ACP server as a child of conductor and speak ACP to it.
- Re-publish `session/update` events onto shux's bus as
  `plugin.conductor.agent_message_chunk`, etc., so other plugins can
  subscribe.
- Visual pane still mirrors the agent's TUI; ACP channel is purely
  for structured state.
- DoD: opencode's `Thinking…` state is detected via ACP `tool_call`
  events with p99 latency < 100 ms (measured) — faster and more
  reliable than the v0.1 regex path.

### v0.6 — Transcripts

- For every ACP-bridged pane, write `.shux/conductor/transcripts/
  <agent>-<session-id>.ndjson`. One line per ACP frame, with
  timestamp.
- Replayable: a separate `conductor replay <file>` tool can
  re-feed the transcript through a fresh agent for differential
  testing.
- DoD: an opencode session produces a transcript whose
  `session/update` count matches the visible message count in the
  pane, ± 1.

### v0.7 — Cross-agent notes + task board

- `.shux/conductor/notes.md`: any agent can append; conductor watches
  via FSEvents/inotify and broadcasts new lines as
  `pane.send_keys "[from <agent>]: ..."` to all other agent panes.
- `.shux/conductor/board.toml`: `[[tasks]]` blocks with
  `state` (todo / doing / done) and `claimed_by`. Atomic claim via
  `flock` on the file.
- DoD: agent A writes "look at file X" to notes.md; agent B's pane
  shows the message within 500 ms.

### v0.8 — Tool-call routing through shux RPCs

- For ACP-bridged panes, intercept `session/request_permission` and
  route execution through shux primitives:
  - `read_file` → `pane.capture` of a transient pane running `cat`
  - `take_screenshot` → `pane.snapshot` — agent gets PNG bytes back
    as an `image` content block (huge: vision-LLM self-correction).
- DoD: an opencode prompt requesting `take_screenshot` gets a real
  PNG of the requested pane back as an embedded image in its next
  turn.

## 6. Configuration surface

`.shux/conductor/config.toml` per project:

```toml
[conductor]
poll_ms      = 2000        # VT-poll cadence (v0.1)
settle_ms    = 5000        # idle-duration before "settled"
auto_dismiss = true        # dismiss trust prompts via Enter
auto_compact = true        # send /compact when context warning visible
acp_enabled  = true        # use ACP fast-path when available (v0.5)
worktree     = false       # auto-worktree agent panes (v0.4, off by default)
notify       = "system"    # "system" / "stdout" / "off"

[agents.claude]
acp_adapter  = "npx -y @agentclientprotocol/claude-agent-acp"
splash_re    = '^Claude Code v\d+\.\d+\.\d+'

[agents.opencode]
acp_argv     = ["opencode", "acp"]
splash_re    = '(?m)^opencode$'
```

## 7. Why this is well-suited to shux specifically

- **No new daemon.** dmux ships a TUI app, amux ships a server + web
  dashboard. Conductor is a single bash/python process that uses
  shux primitives. `shux plugin install plugins/conductor/plugin.sh`
  and you're done.
- **Hot reload from day one.** Edit `patterns/claude.toml`, save —
  daemon respawns conductor in <500 ms (PR #23). New patterns
  effective immediately; agent panes stay alive.
- **Status bar is the dashboard.** Per-pane border titles show live
  state via `pane.set_title`. shux's three-zone status bar already
  shows session + window + clock; conductor adds a fourth segment
  via `set_title` overlays.
- **Pixel proof is free.** Every settle gets a PNG. No tool in the
  cmux/dmux/amux space can offer this — they don't own their
  rasterizer.

## 8. Risks & open questions

| Risk | Mitigation |
|--|--|
| `pane.capture` polling cost at high pane count | Bound at 2 s default + per-pane backoff when idle. Profile in v0.1's DoD. |
| Pattern drift across agent versions | Patterns live in TOML files committed to the plugin; can be hot-patched. Add a `conductor validate-patterns` self-check. |
| ACP adapter quality (claude/codex shims) | v0.5 only ships opencode + gemini natively. claude/codex via adapter is its own task once vetted. |
| OS notification mechanism portability | macOS osascript + Linux notify-send + Windows (skip for v0); pluggable via config. |
| Worktree cleanup on pane.exited | Hook + `git worktree remove` on `pane.exited`; warn + leave-in-place if dirty. |
| Tool-call routing security (v0.8) | Each routed tool call needs an audit-log entry in `.shux/conductor/audit/` and a per-tool allow-list. **No** blanket `run_command` routing. |

## 9. Success criteria (whole roadmap)

- Conductor installable in one command (`shux plugin install ./plugin.sh`).
- Single bash file ≤ 600 lines total, zero non-bash dependencies for v0.1–v0.4 (Python optional for ACP client in v0.5+).
- Every phase ships with a visual proof in `pages/screenshots/` —
  shux-rendered PNG of the feature working in shux itself.
- Every phase has an automation script in
  `.shux/scripts/conductor/<phase>.sh` that re-creates the demo
  unattended.
- All shellcheck-clean, all lint-clean, all CI-green.

## 10. Out-of-scope flags

These look related but are explicitly NOT conductor's job:

- **Pane layout pre-templates** (the 3-agent demo). That's
  `state.apply` + a TOML template — already shipped.
- **Visual snapshots for review** in isolation. That's
  `window.snapshot` — already shipped.
- **Plugin hot reload**. That's the daemon's FSEvents watcher —
  already shipped (PR #23).

Conductor *composes* these. It doesn't re-implement them.
