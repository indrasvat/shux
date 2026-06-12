# shux Plugin Use Cases

> Concrete plugin ideas that validate shux's architecture and serve as guiding forces during implementation. Every use case here must be fully achievable with the WIT interface and process plugin protocol specified in the PRD. If a use case can't be built, the architecture has a gap.

**Date:** 2026-02-18
**Companion to:** `shux_prd_v4.md` Section 7

---

## How to read this document

Each use case specifies:
- **Kind**: Wasm or Process plugin (and why)
- **Extension points used**: Which parts of the plugin API it exercises
- **Permissions required**: What the `plugin.toml` [permissions] section needs
- **How it works**: Concrete walkthrough of the plugin's behavior
- **Why it matters**: What this proves about the architecture
- **WIT functions exercised**: Specific host/plugin functions used (validates API completeness)

---

## 1. Agent Conductor

**The killer app.** An orchestrator that manages multiple AI coding agents across panes with real-time progress tracking and safety guardrails.

- **Kind**: Process plugin (Python or TypeScript -- needs to invoke AI SDKs, manage complex async state)
- **Extension points**: Commands, Event reactors, API extensions, Status segments, Event interceptors
- **Permissions**: `manage_panes`, `send_keys`, `read_pane_output`, `manage_sessions`, `api_extensions`, `intercept_events = ["pane.input"]`

### How it works

```
shux agent spawn --task "refactor auth module" --agent claude --worktree
```

1. The plugin receives `on-command("agent.spawn", [...])`.
2. It calls `create-session("agent-refactor-auth")` to isolate agent work.
3. It calls `create-pane(window_id, "claude-code --task '...'", cwd, ...)` to launch Claude Code.
4. It subscribes to `pane.output` events for the new pane, parsing the agent's progress in real-time.
5. It sets a status segment: `[agents: 1 running | 0 queued]` via `set-status-segment`.
6. It sets a badge on the agent pane: `set-badge(pane_id, "AI")`.

**Multi-agent orchestration:**
```
shux agent swarm --tasks tasks.yaml --parallelism 4
```
- Creates 4 panes, each running an agent on a different task.
- Monitors `pane.exited` events. When an agent finishes, assigns the next task from the queue.
- If an agent pane's output matches a failure pattern, it sets the pane border red via tags and notifies the user.

**Safety guardrails:**
- Registers as an event interceptor for `pane.input` events.
- When an agent attempts `git push --force main`, the interceptor blocks it via `intercept-event` returning `None`.
- Shows an overlay on the pane: `BLOCKED: git push --force main -- approve? [y/N]` via `show-overlay` + `render-overlay`.
- Waits for user input via `on-overlay-input`. On 'y', replays the blocked command via `send-keys`.

**API extensions:**
- Registers `agent.status`, `agent.list`, `agent.abort`, `agent.assign` as API methods via `register-api-method`.
- External tools (MCP Bridge, CI systems) can call these methods to programmatically manage agents.
- **Security note**: These API methods inherit shux's transport-level auth (UDS file permissions or TCP token). The plugin does not implement additional caller authentication — all authenticated API clients can manage agents. For production deployments, the MCP Bridge should expose only a subset of these methods and require explicit opt-in for destructive operations like `agent.abort`.

### Why it matters

This is the plugin that justifies shux's existence. Every agent orchestration tool today (Agent Deck, Agent of Empires, NTM, etc.) wraps tmux because it's the only multiplexer with programmatic control. shux's typed API, event interception, and pane output reading make this plugin possible without hacks. It exercises the full depth of the WIT: lifecycle control, input injection, output reading, overlays, API extension, and event interception.

### WIT functions exercised

`create-session`, `create-pane`, `send-keys`, `read-pane-output`, `focus-pane`, `close-pane`, `set-status-segment`, `set-badge`, `set-pane-tag`, `emit-event`, `show-overlay`, `hide-overlay`, `register-api-method`, `intercept-event`, `on-overlay-input`, `render-overlay`

---

## 2. Smart Context

Automatically detects project context in each pane and annotates it -- language, framework, git branch, project name -- so you always know where you are.

- **Kind**: Wasm (lightweight, pure computation, no external dependencies)
- **Extension points**: Event reactors, Status segments, Pane tags, Badges
- **Permissions**: `fs_read = ["**/.git/HEAD", "**/.git/refs/**", "**/package.json", "**/Cargo.toml", "**/go.mod", "**/pyproject.toml", "**/.python-version", "**/Gemfile"]`

### How it works

1. Subscribes to `pane.cwd_changed` and `pane.focused` events.
2. On CWD change, calls `read-file` to check for project markers:
   - `.git/HEAD` -> extracts branch name
   - `Cargo.toml` -> extracts `[package].name`, sets `lang=rust`
   - `package.json` -> extracts `name`, detects framework (react, next, vue)
   - `go.mod` -> extracts module name, sets `lang=go`
   - `pyproject.toml` -> extracts project name, sets `lang=python`
3. Sets pane tags: `set-pane-tag(pane_id, "lang", "rust")`, `set-pane-tag(pane_id, "project", "shux")`, `set-pane-tag(pane_id, "branch", "feat/plugins")`.
4. Sets badge: `set-badge(pane_id, "rs")` for Rust, `js` for Node, `py` for Python, `go` for Go.
5. Updates status segment: `shux (rust) feat/plugins [3 modified]`.
6. Emits `context.detected` event with full project info -- other plugins consume this. The event payload follows a versioned schema:
   ```json
   {"version": 1, "pane_id": "p-1", "lang": "rust", "project": "shux", "branch": "feat/plugins", "framework": null}
   ```
   Consumers must check `version` and gracefully ignore unknown fields (forward-compatible).

### Why it matters

This is the composability proof. Smart Context doesn't do anything dramatic on its own -- it detects and annotates. But its pane tags and events become the foundation for other plugins: Danger Zone reads `lang` tags for language-specific blocklists, Workspace Profiles uses project detection for auto-loading, the status bar shows context-aware info. It validates the inter-plugin communication model via tags and events. **Inter-plugin contract**: Consumers of `context.detected` should subscribe to the event and maintain an internal cache. If Smart Context is not installed, consumers fall back gracefully (no context-aware features, never crash).

### WIT functions exercised

`read-file`, `set-pane-tag`, `set-badge`, `set-status-segment`, `emit-event`, `get-active-pane`, `list-panes`, `get-config`

---

## 3. Session Replay

`cy`-style time travel -- record everything, scrub backward, search history -- as a plugin instead of a separate multiplexer.

- **Kind**: Process plugin (Rust binary -- needs high-throughput stream processing, disk I/O, its own vte parser)
- **Extension points**: Event reactors, Commands, API extensions, Interactive overlays
- **Permissions**: `read_pane_output`, `api_extensions`, `fs_write = ["~/.local/share/shux/replay/**"]`

### How it works

**Recording:**
1. Uses daemon-owned `pane.record.start` / `pane.record.stop` for panes the
   user explicitly records. `pane.output` remains a sampled live-observation
   stream and is not suitable for byte-exact replay files.
2. Stores timestamped raw byte streams to disk:
   `~/.local/share/shux/replay/{session}/{pane_id}.rec` (compressed with zstd).
   Compression runs outside the PTY read path to avoid blocking the pane.
3. Maintains an index of timestamps and pane lifecycle events for fast seeking.
4. Pane creation/destruction events mark replay boundaries; recorder start/stop
   events mark byte-exact transcript boundaries.
5. **Throughput management**: the daemon recorder applies intentional
   backpressure and reports `complete|error|aborted`, `bytes_written`, and
   error detail. Sampled `pane.output` can still drive UI previews, but lossy
   previews must never be labeled as replay-grade transcripts.

**Playback:**
```
shux replay <pane-id>
```
1. Plugin receives `on-command("replay.open", [pane_id])`.
2. Calls `show-overlay(pane_id)` to take over the pane's display.
3. Renders historical terminal state via `render-overlay` by replaying recorded bytes through an internal vte parser at the requested timestamp.
4. User interacts via `on-overlay-input`:
   - `h/l` or left/right arrows -- scrub backward/forward
   - `/` -- enter search mode, full-text search across history
   - `b` -- set a bookmark at the current timestamp
   - `q` -- exit replay, calls `hide-overlay`
   - `g` -- jump to a specific timestamp

**Bookmarks:**
```
shux replay bookmark "before refactor"
shux replay bookmarks                    # list all bookmarks
shux replay goto "before refactor"       # jump to bookmark
```

**Export:**
```
shux replay export <pane-id> --format asciicast --from "2h ago" --to "1h ago" > session.cast
```

**API extensions:**
- Registers `replay.seek`, `replay.search`, `replay.export`, `replay.bookmarks` as API methods.
- AI agents can programmatically search historical output: "find me when that test started failing."

### Why it matters

This is cy's killer feature (time travel) delivered as a composable plugin. It validates the interactive overlay system (show/hide/input), high-volume event streaming with flow control, API extension registration, and the process plugin's ability to do heavy compute (vte parsing, compression) without affecting daemon performance.

### WIT functions exercised

`show-overlay`, `hide-overlay`, `render-overlay`, `on-overlay-input`, `register-api-method`, `emit-event`, `set-status-segment`, `read-pane-output`, `get-pane`, `list-panes`

---

## 4. Danger Zone

A permission gate that intercepts dangerous commands before they reach the terminal -- essential safety layer for human and AI workflows.

- **Kind**: Wasm (fast pattern matching, no external dependencies, must be low-latency on the input hot path)
- **Extension points**: Event interceptors, Interactive overlays, Commands
- **Permissions**: `intercept_events = ["pane.input"]`, `send_keys` (for override flow only)

### How it works

1. Loads blocklist and confirmlist patterns from its config section in `shux.toml`:
   ```toml
   [plugins.danger-zone]
   block = ["rm -rf /", ":(){ :|:& };:", "dd if=.* of=/dev/"]
   confirm = ["git push", "npm publish", "docker system prune", "terraform apply", "kubectl delete"]
   auto_confirm_timeout = 0     # 0 = no auto-confirm; >0 = auto-confirm after N seconds
   ```

2. Registers as an event interceptor for `pane.input` events. The daemon calls `intercept-event` for every input line (specifically, when Enter/Return is pressed -- not per-keystroke, to avoid latency on typing).

3. **On block match**: Returns `None` from `intercept-event` (blocks the event). Calls `show-overlay(pane_id)`. `render-overlay` displays:
   ```
   +--------------- BLOCKED ----------------+
   | rm -rf / is on the blocklist.          |
   |                                        |
   | Press Ctrl+Y to override, q to cancel  |
   +----------------------------------------+
   ```
   `on-overlay-input`: Ctrl+Y sends the original command via `send-keys` (requires `send_keys` permission only when override is enabled). 'q' calls `hide-overlay`.

4. **On confirm match**: Returns `None` from `intercept-event`. Shows amber overlay:
   ```
   +--------------- CONFIRM ----------------+
   | git push origin main                   |
   |                                        |
   | Press y to proceed, n to cancel        |
   +----------------------------------------+
   ```

5. **Context-aware rules**: Reads pane tags (set by Smart Context) to apply language-specific rules. E.g., in a `lang=python` pane, also confirm `pip install` with `--break-system-packages`. If Smart Context is not installed or has not yet tagged a pane, the plugin falls back to language-agnostic rules only (never assumes a language).

**Failure behavior (fail-closed)**: If Danger Zone crashes or times out during interception, the host blocks the command (fail-closed per PRD §7.2a) and shows a host-rendered warning overlay. This is critical for safety — a crashing safety plugin must never silently allow dangerous commands through. The plugin is marked degraded until reloaded.

**Stale pane ID handling**: All pane-referencing calls (`show-overlay`, `get-pane`, `send-keys`) guard against stale IDs. If a pane was closed between interception and overlay display, the plugin logs a warning and discards the blocked command.

### Why it matters

This is the event interception proof. It validates that plugins can gate operations before they happen -- the PI-inspired "blocking gate" pattern. Critical for AI safety: when agents run in panes, Danger Zone prevents accidental destructive commands without requiring each agent tool to implement its own safety checks. Also validates interactive overlays with input handling.

### WIT functions exercised

`intercept-event`, `show-overlay`, `hide-overlay`, `render-overlay`, `on-overlay-input`, `send-keys` (for override), `get-pane` (for tag reading), `get-config`, `log`

---

## 5. Live Tail Dashboard

A monitoring layout that auto-creates panes for tailing logs, watching metrics, and checking health -- one command to spin up a full observability view.

- **Kind**: Wasm (layout creation is straightforward, no heavy compute)
- **Extension points**: Layout providers, Commands, Status segments, Event reactors
- **Permissions**: `manage_panes`, `manage_sessions`, `fs_read = ["~/.config/shux/dashboards/**"]`

### How it works

1. Ships with built-in dashboard templates and supports user-defined ones:
   ```toml
   # ~/.config/shux/dashboards/monitoring.toml
   [dashboard]
   name = "monitoring"

   [[pane]]
   name = "api-logs"
   command = "tail -f /var/log/api.log"
   position = "top-left"
   size = "50%"

   [[pane]]
   name = "worker-logs"
   command = "tail -f /var/log/worker.log"
   position = "top-right"
   size = "50%"

   [[pane]]
   name = "metrics"
   command = "watch -n1 curl -s localhost:9090/metrics | grep http_requests"
   position = "bottom-left"
   size = "50%"

   [[pane]]
   name = "health"
   command = "watch -n5 'curl -s localhost:8080/health | jq .'"
   position = "bottom-right"
   size = "50%"
   ```

2. `shux dashboard start monitoring` triggers `on-command("dashboard.start", ["monitoring"])`.
3. Plugin reads the dashboard TOML via `read-file`.
4. Calls `create-window` to create a dedicated window.
5. Creates the first pane via `create-pane`, then uses `split-pane` for subsequent panes with the specified directions and sizes.
6. Sets pane tags: `set-pane-tag(pane_id, "dashboard", "monitoring")`.
7. Registers the layout via `register-layout("monitoring", layout_json)` so it can be re-applied with `shux layout monitoring`.
8. Subscribes to `pane.exited` events. If any dashboard pane's process exits non-zero, sets a red badge and updates the status segment: `[monitoring: 1 FAIL]`.

**Dashboard management:**
```
shux dashboard list                    # show active dashboards
shux dashboard stop monitoring         # close all panes in the dashboard
shux dashboard restart monitoring      # kill and recreate all panes
```

### Why it matters

Validates the pane lifecycle API end-to-end: create-pane, split-pane, layout registration, event-driven health monitoring. Shows that complex multi-pane layouts can be created programmatically by plugins, not just manually by users. This is exactly the kind of automation that tmux users achieve with fragile shell scripts -- shux makes it a first-class, typed, declarative experience.

### WIT functions exercised

`create-window`, `create-pane`, `split-pane`, `close-pane`, `register-layout`, `apply-layout`, `set-pane-tag`, `set-badge`, `set-status-segment`, `read-file`, `focus-pane`

---

## 6. MCP Bridge

Exposes shux's entire API as an MCP (Model Context Protocol) server, so any AI agent with MCP support can natively drive the multiplexer.

- **Kind**: Process plugin (needs to run an MCP server -- stdio or SSE transport -- alongside the plugin protocol)
- **Extension points**: API extensions, Commands, Event reactors
- **Permissions**: `manage_panes`, `manage_sessions`, `send_keys`, `read_pane_output`, `api_extensions`, `network` (for SSE transport), `clipboard`

### How it works

1. On init, starts an MCP server (stdio transport for integration with Claude Code, or SSE on a local port for other tools).
2. Registers shux operations as MCP tools:

   | MCP Tool | shux API call |
   |----------|---------------|
   | `shux_create_pane` | `create-pane` |
   | `shux_split_pane` | `split-pane` |
   | `shux_send_keys` | `send-keys` |
   | `shux_read_output` | `read-pane-output` |
   | `shux_list_panes` | `list-panes` |
   | `shux_focus_pane` | `focus-pane` |
   | `shux_close_pane` | `close-pane` |
   | `shux_create_session` | `create-session` |
   | `shux_list_sessions` | `list-sessions` |
   | `shux_apply_layout` | `apply-layout` |

3. Exposes shux state as MCP resources:
   ```
   shux://sessions          -> list all sessions
   shux://session/{id}      -> session details
   shux://pane/{id}/output  -> last 100 lines of pane output
   ```

4. Forwards shux events as MCP notifications, so agents can subscribe to real-time terminal events.

5. Registers `mcp.status` as an API method to report MCP server health.

**Usage with Claude Code:**
```json
// In Claude Code's MCP config:
{
  "mcpServers": {
    "shux": {
      "command": "shux",
      "args": ["mcp", "stdio"]
    }
  }
}
```

Now Claude Code can natively create panes, read output, and orchestrate terminal sessions -- no tmux hacks needed.

### Why it matters

This is the bridge between the AI ecosystem and shux. Instead of every agent tool wrapping tmux with shell scripts, they talk MCP to shux. It validates that the process plugin protocol is powerful enough to expose the full shux API to external consumers. It also proves the API extension system -- the MCP Bridge registers its own methods that appear in `shux help`.

### WIT functions exercised (via process plugin protocol equivalents)

`create-pane`, `split-pane`, `send-keys`, `read-pane-output`, `list-panes`, `list-sessions`, `list-windows`, `get-pane`, `get-session`, `focus-pane`, `close-pane`, `create-session`, `apply-layout`, `register-api-method`, `get-clipboard`, `set-clipboard`

---

## 7. SSH Tunnels

Manages persistent SSH tunnels with automatic reconnection, health monitoring, and per-session tunnel profiles.

- **Kind**: Process plugin (Rust or Go binary -- manages SSH subprocesses, needs persistent background state)
- **Extension points**: Commands, Status segments, Event reactors, Lifecycle hooks
- **Permissions**: `exec`, `fs_read = ["~/.ssh/**", "~/.config/shux/tunnels/**"]`

### How it works

1. Loads tunnel definitions from `~/.config/shux/tunnels/`:
   ```toml
   # ~/.config/shux/tunnels/prod.toml
   [[tunnel]]
   name = "db"
   host = "bastion.prod"
   local_port = 5432
   remote = "db.internal:5432"
   identity = "~/.ssh/prod_key"

   [[tunnel]]
   name = "grafana"
   host = "bastion.prod"
   local_port = 3000
   remote = "grafana.internal:3000"
   ```

2. `shux tunnel up prod` spawns SSH processes (via the plugin's own subprocess management, not shux panes -- tunnels are background services).
3. Monitors tunnel health via periodic TCP probes on local ports.
4. Auto-reconnects with exponential backoff on failure.
5. Status segment: `[tunnels: 2/2]` or `[tunnels: 1/2 db reconnecting...]`.
6. On `session.created` event, checks if the session name matches a tunnel profile and auto-starts tunnels.
7. On `shutdown` lifecycle event, cleanly terminates all SSH processes.
8. **Orphan recovery**: On startup, the plugin checks for a persisted tunnel registry file (`~/.local/share/shux/tunnels/state.json`). If SSH processes from a previous daemon run are still alive (checked via PID), it adopts them rather than spawning duplicates. If they are stale (PID doesn't match or process is dead), it cleans up the registry and starts fresh.
9. **Health timeout**: If a tunnel fails to reconnect after 5 minutes of exponential backoff, it is marked `failed` (not `reconnecting`) and the status segment turns red. The user must explicitly `shux tunnel up` to retry.

**Commands:**
```
shux tunnel up prod             # start all tunnels in prod profile
shux tunnel down prod           # stop all
shux tunnel status              # show all tunnels with latency
shux tunnel add prod --name redis --host bastion.prod --local 6379 --remote redis.internal:6379
```

### Why it matters

Validates the process plugin as a long-lived background service (opt out of GC with `gc = false`). Shows lifecycle hooks working correctly -- tunnels start on session create and stop on shutdown. A practical plugin that every engineer with SSH tunnels would install immediately.

### WIT functions exercised (via process plugin protocol)

`set-status-segment`, `read-file`, `get-config`, `log`, `emit-event` (for tunnel status changes)

---

## 8. Pane Sync

Synchronized input across multiple panes -- type once, run everywhere.

- **Kind**: Wasm (lightweight input multiplexing, needs to be on the hot path)
- **Extension points**: Input handlers (via event interception), Commands, Status segments, Pane overlays
- **Permissions**: `intercept_events = ["pane.input"]`, `send_keys`, `manage_panes` (for pane tagging)

### How it works

1. `shux sync start` enters sync mode. Plugin tags all panes in the current window with `sync-group=active`.
2. `shux sync add <pane-id>` or `shux sync remove <pane-id>` manages the group.
3. Intercepts `pane.input` events on the focused pane.
4. Replicates the input to all other panes in the sync group via `send-keys`.
5. Shows a `SYNC` badge on synced panes via `set-badge`.
6. Status segment: `[SYNC: 4 panes]`.
7. `shux sync stop` clears tags, badges, and removes the interceptor.

**Smart sync mode:**
```
shux sync start --after-prompt "$"
```
Only replicates input that comes after a shell prompt pattern. This prevents replaying command output from one pane into another's input.

**Selective sync:**
```
shux sync start --panes p-1,p-3,p-5
```

### Why it matters

Validates event interception on the input hot path with fan-out via `send-keys`. Proves that plugins can build interactive modes that fundamentally change how input flows through the multiplexer. A feature that tmux has (`synchronize-panes`) but implemented as a proper plugin with better UX (badges, selective sync, prompt-aware mode).

### WIT functions exercised

`intercept-event`, `send-keys`, `set-pane-tag`, `clear-pane-tag`, `set-badge`, `clear-badge`, `set-status-segment`, `list-panes`, `get-pane`

---

## 9. Scratchpad

A quick-summon floating pane for throwaway work -- always one keypress away, persists across window switches.

- **Kind**: Wasm (simple pane lifecycle management)
- **Extension points**: Commands, Input handlers, Floating panes
- **Permissions**: `manage_panes`, `intercept_events = ["pane.input"]` (for keybinding interception)

### How it works

1. Registers a keybinding (e.g., `Prefix + s`) via event interception on a specific key pattern. (Note: `Ctrl+Space` is the default prefix key in shux, so plugins should avoid bare `Ctrl+Space` and use prefix-chained bindings or alternative modifiers like `Alt+\`` to prevent collisions.)
2. On first trigger, calls `create-floating-pane(width=80%, height=60%, command="bash", name="scratch-default")`.
3. Stores the pane ID. On subsequent triggers, calls `toggle-floating-pane(pane_id)` to show/hide.
4. The floating pane persists -- same shell session, same scrollback, same CWD -- until explicitly closed.

**Named scratchpads:**
```
shux scratch open notes          # floating pane running $EDITOR with a scratch notes file
shux scratch open python         # floating pane running python3 REPL
shux scratch open htop           # floating pane running htop
shux scratch list                # show all scratchpads and their states
shux scratch close notes         # permanently close a scratchpad
```

5. Each named scratchpad has its own floating pane and toggle keybinding (configurable).
6. Scratchpads auto-size: centered, 60% of terminal by default, configurable per scratchpad.

### Why it matters

Validates floating pane lifecycle: create, toggle, persist across context switches. A quality-of-life feature that makes shux feel like a modern IDE. Proves the floating pane API is usable from plugins without special-casing.

### WIT functions exercised

`create-floating-pane`, `toggle-floating-pane`, `close-pane`, `focus-pane`, `intercept-event` (for keybinding), `set-badge`, `list-panes`

---

## 10. Workspace Profiles

Project-aware session templates that recreate your entire dev environment in one command -- editor, test runner, log tail, all in the right layout.

- **Kind**: Wasm (layout creation and template parsing, no heavy compute)
- **Extension points**: Commands, Layout providers, Lifecycle hooks, Inter-plugin bus
- **Permissions**: `manage_panes`, `manage_sessions`, `fs_read = ["~/.config/shux/workspaces/**"]`

### How it works

1. Loads workspace profiles from `~/.config/shux/workspaces/`:
   ```toml
   # ~/.config/shux/workspaces/shux-dev.toml
   [workspace]
   name = "shux-dev"
   cwd = "~/code/shux"
   session_name = "shux"

   [[pane]]
   name = "editor"
   command = "nvim"
   focus = true
   size = "70%"

   [[pane]]
   name = "cargo-watch"
   command = "cargo watch -x 'test --lib'"
   split = "right"
   size = "30%"

   [[pane]]
   name = "logs"
   command = "tail -f /tmp/shux.log"
   split = "below-right"
   size = "40%"
   ```

2. `shux workspace open shux-dev`:
   - Calls `create-session("shux")` and `create-window(session_id, "dev")`.
   - Creates the first pane: `create-pane(window_id, "nvim", cwd="~/code/shux")`.
   - Splits for subsequent panes: `split-pane(pane_id, vertical, 30%, "cargo watch ...")`.
   - Sets `focus-pane` on the designated pane.
   - Registers the layout so it can be re-applied after manual changes.

3. Emits `workspace.opened` via `emit-event` with versioned payload:
   ```json
   {"version": 1, "name": "shux-dev", "cwd": "~/code/shux", "session_id": "s-1", "pane_ids": ["p-1", "p-2", "p-3"]}
   ```
   - Smart Context picks up the event and pre-populates tags.
   - SSH Tunnels auto-starts project-specific tunnels (verifying the event came from a known plugin via the `plugin_id` field on `plugin.event` events in the taxonomy).
   - Theme plugin switches to a project-specific theme if configured.

4. `shux workspace save` snapshots the current layout as a new workspace profile.
5. `shux workspace list` shows available profiles.

### Why it matters

Validates the full lifecycle: session creation -> window creation -> pane creation -> splitting -> layout registration -> inter-plugin events. The inter-plugin bus is critical here -- one command triggers a cascade of plugin reactions that set up the entire environment. This is what makes shux feel like a personalized IDE, not just a terminal.

### WIT functions exercised

`create-session`, `create-window`, `create-pane`, `split-pane`, `focus-pane`, `register-layout`, `emit-event`, `read-file`, `set-pane-tag`, `get-config`, `list-sessions`

---

## 11. Command Palette

A fuzzy-find command launcher -- Ctrl+P for your terminal. Find commands, switch panes, search history, all from one unified interface.

- **Kind**: Wasm (needs fast fuzzy matching, latency-sensitive UI)
- **Extension points**: Input handlers, Interactive overlays, Commands
- **Permissions**: `manage_panes` (for pane switching), `read_pane_output` (for output search), `intercept_events = ["pane.input"]` (for Ctrl+P keybinding), `fs_write = ["~/.local/share/shux/palette/**"]` (for MRU command history persistence)

### How it works

1. Registers Ctrl+P (or configurable) as a keybinding via input event interception.
2. On trigger, calls `show-overlay` on the active pane with a centered modal.
3. `render-overlay` displays a fuzzy search box with categorized results:
   ```
   +----------------- Command Palette -------------------+
   | > git pu_                                           |
   |                                                     |
   | Commands                                            |
   |   git push              Push current branch         |
   |   git pull              Pull and rebase             |
   |   git-status.refresh    Refresh git status          |
   |                                                     |
   | Panes                                               |
   |   [p-3] nvim            ~/code/shux (focused)       |
   |   [p-4] cargo watch     ~/code/shux                 |
   |                                                     |
   | Sessions                                            |
   |   shux-dev              3 panes, 2 windows          |
   |   scratch               1 pane                      |
   +-----------------------------------------------------+
   ```

4. `on-overlay-input` handles keystrokes:
   - Typing updates the fuzzy filter and re-renders
   - Up/Down arrows navigate the result list
   - Enter runs the selected item:
     - Command -> dispatches via the API
     - Pane -> calls `focus-pane(pane_id)`
     - Session -> calls `focus-window` on the session's active window
   - Escape -> calls `hide-overlay`

5. Data sources (all gathered via host queries):
   - All registered commands (from all plugins)
   - All panes (`list-panes`), windows (`list-windows`), sessions (`list-sessions`)
   - Recently used commands (stored in plugin state via `write-file`)
   - Plugin-contributed entries via the inter-plugin bus (`palette.register` event)

### Why it matters

Validates the interactive overlay as a full UI framework -- not just a static warning, but a dynamic, input-driven interface with fuzzy search and real-time rendering. Proves that `on-overlay-input` + `render-overlay` together enable rich interactive experiences. Also validates cross-cutting queries (`list-panes`, `list-sessions`, `list-windows`) used together to build a unified search.

### WIT functions exercised

`intercept-event`, `show-overlay`, `hide-overlay`, `render-overlay`, `on-overlay-input`, `list-panes`, `list-windows`, `list-sessions`, `focus-pane`, `focus-window`, `get-config`, `write-file`, `read-file`

---

## 12. Pane Notifications

Desktop notifications when long-running commands finish, output matches patterns, or panes need attention -- never miss a build completion again.

- **Kind**: Process plugin (needs access to OS notification APIs -- notify-send, osascript, terminal-notifier)
- **Extension points**: Event reactors, Commands, Status segments
- **Permissions**: `read_pane_output`, `exec`

### How it works

1. Subscribes to `pane.exited` events. When a command finishes:
   - If the pane was **not focused** AND the command ran for more than a configurable threshold (default: 15 seconds), send a desktop notification:
     ```
     shux: cargo test finished (exit 0) in pane "cargo-watch"
     ```
   - On macOS: calls `osascript -e 'display notification ...'` via the `exec` host function.
   - On Linux: calls `notify-send` via the `exec` host function.

2. **Pattern-based alerts**: Subscribe to `pane.output` events and scan for configurable patterns:
   ```toml
   [plugins.notifications]
   alert_patterns = [
     { pattern = "error\\[E", level = "error", title = "Rust compilation error" },
     { pattern = "FAIL", level = "warn", title = "Test failure" },
     { pattern = "Successfully compiled", level = "info", title = "Build complete" },
   ]
   ```

3. **Bell forwarding**: Subscribe to `pane.bell` events (when a program writes `\a`) and forward as desktop notifications.

4. **Activity badge**: When a backgrounded pane has new output, set a badge: `set-badge(pane_id, "!")`. Clear the badge when the pane is focused (on `pane.focused` event).

5. Status segment: `[2 notifications]` with a counter that clears on view.

6. `shux notify test` sends a test notification to verify OS integration.

### Why it matters

Validates event reactor pattern for background monitoring. Proves that process plugins can bridge shux events to OS-level services. A universally useful quality-of-life plugin that makes shux aware of the broader desktop environment. Also validates the `exec` host function for running system commands from plugins.

### WIT functions exercised (via process plugin protocol)

`set-badge`, `clear-badge`, `set-status-segment`, `emit-event`, `get-pane`, `read-pane-output`, `get-config`, `log`; `exec` for OS notifications

---

## 13. AI Chat Sidebar

A persistent AI assistant pane that can see other panes' output and help you debug, explain errors, and suggest commands -- without leaving the terminal.

- **Kind**: Process plugin (TypeScript or Python -- needs AI SDK access, streaming responses)
- **Extension points**: Commands, Floating panes, Event reactors, API extensions
- **Permissions**: `manage_panes`, `read_pane_output`, `send_keys`, `network`, `api_extensions`

### How it works

1. `shux ai open` creates a floating pane running the plugin's own chat TUI (or reuses an existing one via `toggle-floating-pane`).
2. The chat UI lives inside the floating pane -- the plugin manages its own rendering there.
3. **Context awareness**: The plugin reads output from the last-focused non-AI pane via `read-pane-output` and includes it as context in the AI prompt.
4. **Commands from chat**:
   - User types `@run cargo test` in the chat -> plugin calls `send-keys` on the target pane.
   - User types `@explain` -> plugin reads the last 50 lines of the target pane's output, sends to the LLM, displays the explanation in the chat pane.
   - User types `@fix` -> plugin reads error output, sends to LLM, gets a suggested command, shows it for confirmation, then sends it via `send-keys`.

5. **Error detection**: Subscribes to `pane.output` events. When output matches common error patterns (stack traces, compilation errors, test failures), shows a subtle badge `?` on the pane and a status segment `[AI: error detected in p-3]`. User can run `shux ai explain` to get instant help.

6. **API extension**: Registers `ai.ask`, `ai.explain`, `ai.suggest` as API methods so other plugins can invoke the AI.

### Why it matters

Validates the "AI-native multiplexer" vision. Combines several plugin capabilities: floating pane lifecycle, pane output reading, input injection across panes, network access for API calls, and API extension. This is the plugin that makes non-technical users go "wow" -- a built-in AI that sees what you see and helps in context.

### WIT functions exercised (via process plugin protocol)

`create-floating-pane`, `toggle-floating-pane`, `read-pane-output`, `send-keys`, `focus-pane`, `get-pane`, `list-panes`, `set-badge`, `set-status-segment`, `register-api-method`, `emit-event`

---

## 14. Git Worktree Manager

Visual management of git worktrees -- create panes per branch, switch contexts instantly, auto-cleanup stale worktrees.

- **Kind**: Process plugin (Rust or Go -- needs git CLI interaction, filesystem operations)
- **Extension points**: Commands, Status segments, Event reactors, Layout providers, Inter-plugin bus
- **Permissions**: `manage_panes`, `manage_sessions`, `exec`, `fs_read = ["**/.git/**"]`

### How it works

1. `shux worktree list` calls `git worktree list --porcelain` via `exec`, parses the output, and displays a table:
   ```
   Branch          Path                    Pane
   main            ~/code/shux             p-1 (focused)
   feat/plugins    ~/code/shux-plugins     p-3
   fix/resize      ~/code/shux-resize      (no pane)
   ```

2. `shux worktree open feat/plugins`:
   - Calls `exec("git", ["worktree", "add", "../shux-feat-plugins", "feat/plugins"])` if the worktree doesn't exist.
   - Calls `create-pane(window_id, "bash", cwd=worktree_path)` to open a pane in that worktree.
   - Sets pane tag: `set-pane-tag(pane_id, "branch", "feat/plugins")`.
   - Sets badge: `set-badge(pane_id, "feat/plugins")`.

3. `shux worktree pr 1234`:
   - Fetches the PR branch: `exec("gh", ["pr", "checkout", "1234", "--detach"])`.
   - Creates a worktree for the PR branch.
   - Opens a pane in the worktree.
   - Sets up the pane for review: `send-keys(pane_id, "git log --oneline main..HEAD\n")`.

4. **Auto-cleanup**: Periodically checks for merged branches with `exec("git", ["branch", "--merged"])` and offers to remove stale worktrees.

5. Emits `worktree.opened {branch, path, pane_id}` so Smart Context can immediately pick up the project info.

6. **Multi-branch layout**: `shux worktree compare main feat/plugins` creates a side-by-side split with one pane per branch.

### Why it matters

Validates the `exec` host function for running system commands and parsing their output. Shows how plugins can compose git CLI operations with shux pane management to create workflows that would require complex shell scripting otherwise. The inter-plugin bus integration with Smart Context proves event-driven plugin composition.

### WIT functions exercised (via process plugin protocol)

`create-pane`, `split-pane`, `close-pane`, `focus-pane`, `set-pane-tag`, `set-badge`, `set-status-segment`, `emit-event`, `register-layout`, `list-panes`, `get-pane`; `exec` for git operations

---

## 15. Pipe / Tee

Route output from one pane as input to another -- create live data pipelines between panes, tee output to log files, or feed output into analysis tools.

- **Kind**: Wasm (stream processing, needs to be efficient for high-volume data). Note: for very high-throughput pipelines, a Process plugin may be more appropriate to avoid Wasm call overhead per chunk — monitor p99 latency and reclassify if it exceeds 5ms per event batch.
- **Extension points**: Commands, Event reactors, Status segments
- **Permissions**: `read_pane_output`, `send_keys`, `manage_panes`, `fs_write = ["~/.local/share/shux/pipes/**"]` (for tee-to-file mode)

### How it works

1. **Tee to pane**: `shux pipe tee p-1 p-2` -- everything that appears in pane p-1 also gets sent to pane p-2.
   - Subscribes to sampled `pane.output` events for p-1 for live interactive mirroring.
   - On each output event, calls `send-keys(p-2, output_bytes)` (raw bytes, not text).
   - Marks the pipe as live/best-effort; it is not byte-exact and must not be
     used for parsers or audits that depend on absence-of-bytes semantics.

2. **Pipe to command**: `shux pipe to p-1 "jq '.errors[]'"` -- creates a new pane running `jq`, feeds p-1's output into it.
   - Calls `split-pane(p-1, vertical, 40%, "jq '.errors[]'")`.
   - Subscribes to p-1's output, sends to the new pane via `send-keys`.

3. **Tee to file**: `shux pipe file p-1 /tmp/session.log` -- writes all output from p-1 to a file.
   - Uses `pane.record.start` / `pane.record.stop` for a byte-exact file, or
     explicitly labels sampled `pane.output` output as lossy when only a live
     preview log is needed.

4. **Filter pipe**: `shux pipe grep p-1 "ERROR" --pane` -- creates a pane that only shows lines matching a pattern.
   - Creates a new pane.
   - Subscribes to p-1's output, filters with regex, sends matching lines to the new pane.

5. **Pipeline chains**: `shux pipe chain p-1 "grep ERROR" "sort" "uniq -c"` -- creates a multi-stage pipeline with a pane per stage.

6. Status segment shows active pipes: `[pipes: 2 active]`.
7. `shux pipe list` shows all active pipes. `shux pipe stop <pipe-id>` tears one down.

### Why it matters

Validates high-volume sampled `pane.output` observation for live cross-pane
data flow, while reserving byte-exact file capture for `pane.record.*`. This
is a uniquely terminal-multiplexer capability -- no other tool can route
terminal output between processes interactively. It turns shux into a visual
dataflow tool without confusing previews with transcripts.

### WIT functions exercised

`split-pane`, `create-pane`, `send-keys`, `read-pane-output`, `close-pane`, `set-status-segment`, `set-pane-tag`, `set-badge`, `write-file`, `list-panes`

---

## Extension Point Coverage Matrix

Every extension point in the PRD must be exercised by at least one use case. This matrix validates completeness:

| Extension Point | Use Cases |
|----------------|-----------|
| **Commands** | All 15 |
| **Command overrides** | 4 (Danger Zone) |
| **Status bar segments** | 1, 2, 4, 5, 7, 8, 10, 12, 14, 15 |
| **Pane overlays** | 1, 3, 4, 11 |
| **Interactive overlay input** | 1, 3, 4, 11 |
| **Theme packs** | (not covered -- addressed by bundled shux-theme-pack; see gap note below) |
| **Event reactors** | 1, 2, 3, 5, 6, 7, 8, 10, 12, 13, 14, 15 |
| **Event interceptors** | 1, 4, 8, 9, 11 |
| **Exporters** | 3 (replay export) |
| **Layout providers** | 5, 10, 14 |
| **Input handlers** | 4, 8, 9, 11 |
| **API extensions** | 1, 3, 6, 13 |
| **Lifecycle hooks** | 5, 7, 10 |
| **Inter-plugin bus** | 2, 10, 11, 14 |
| **Floating panes** | 9, 13 |
| **Pane lifecycle (create/close/split)** | 1, 5, 6, 9, 10, 13, 14, 15 |
| **Pane interaction (send-keys/read-output)** | 1, 3, 4, 6, 8, 11, 12, 13, 15 |
| **Session/window lifecycle** | 1, 5, 10, 14 |
| **Clipboard** | 6 |
| **Subprocess execution** | 7, 12, 14 |

### WIT Host Function Coverage

Every host function must be exercised by at least one use case:

| Host Function | Used by |
|---------------|---------|
| `get-active-pane` | 2, 11 |
| `get-pane` | 1, 2, 4, 7, 12, 13, 14 |
| `list-panes` | 2, 5, 6, 8, 9, 11, 13, 15 |
| `get-active-window` | 11 |
| `get-window` | 11 |
| `list-windows` | 6, 11 |
| `get-active-session` | 10 |
| `get-session` | 6 |
| `list-sessions` | 6, 10, 11 |
| `get-config` | 2, 4, 7, 11, 12 |
| `create-pane` | 1, 5, 10, 13, 14, 15 |
| `split-pane` | 5, 10, 14, 15 |
| `create-floating-pane` | 9, 13 |
| `close-pane` | 1, 5, 9, 14, 15 |
| `toggle-floating-pane` | 9, 13 |
| `send-keys` / `send-text` | 1, 4, 6, 8, 13, 15 |
| `read-pane-output` | 1, 3, 6, 12, 13, 15 |
| `read-pane-scrollback` | 3 |
| `focus-pane` | 1, 5, 9, 10, 11, 13, 14 |
| `resize-pane` | (5 implicitly via layout) |
| `rename-pane` | 10 |
| `set-pane-tag` | 1, 2, 5, 8, 10, 14, 15 |
| `clear-pane-tag` | 8 |
| `create-session` | 1, 10 |
| `create-window` | 5, 10 |
| `close-window` | 5 |
| `kill-session` | 1 |
| `rename-session` | 10 |
| `rename-window` | 10 |
| `focus-window` | 10, 11 |
| `register-layout` | 5, 10, 14 |
| `apply-layout` | 5, 6 |
| `set-status-segment` | 1, 2, 3, 5, 7, 8, 10, 12, 14, 15 |
| `set-badge` | 1, 2, 5, 8, 9, 12, 13, 14, 15 |
| `clear-badge` | 8, 12 |
| `emit-event` | 1, 2, 3, 7, 10, 12, 13, 14 |
| `show-overlay` | 1, 3, 4, 11 |
| `hide-overlay` | 1, 3, 4, 11 |
| `get-clipboard` | 6 |
| `set-clipboard` | 6 |
| `register-api-method` | 1, 3, 6, 13 |
| `register-command-override` | 4 |
| `log` | 4, 7 |
| `read-file` | 2, 5, 7, 10, 11 |
| `write-file` | 11, 15 |
| `exec` | 7, 12, 14 |

**Result: 100% coverage.** Every WIT host function and every plugin callback is exercised by at least one use case. If any function can be removed without breaking a use case, it shouldn't be in the WIT.

### Coverage Gaps & Future Use Cases

Two PRD extension points are not validated by community plugin use cases and should be addressed:

1. **Theme packs**: The bundled `shux-theme-pack` exercises theme registration, but no use case validates the full lifecycle: theme discovery, selection, per-pane cascade, runtime theme change events (`theme.changed`), and theme-contributed palettes. A dedicated **Theme Manager** use case should be added when the theme ecosystem matures.

2. **Session persistence**: The PRD specifies session-persist as a P1 first-party plugin (§6.2), but no use case validates the critical path: periodic graph serialization, crash recovery, restore-with-confirmation ("Press ENTER to run..."), and conflict resolution when the serialized layout references panes whose commands have changed. This use case is essential for proving crash-safe design and should be developed alongside the plugin.

### Inter-Plugin Event Contracts

The following custom events are emitted on the inter-plugin bus. Each must have a stable, versioned schema to prevent silent breakage:

| Event | Emitter | Consumers | Schema version |
|-------|---------|-----------|---------------|
| `context.detected` | Smart Context (UC2) | Danger Zone (UC4), Status bar, others | v1 |
| `workspace.opened` | Workspace Profiles (UC10) | Smart Context (UC2), SSH Tunnels (UC7), Theme plugins | v1 |
| `worktree.opened` | Git Worktree Manager (UC14) | Smart Context (UC2) | v1 |
| `palette.register` | Any plugin | Command Palette (UC11) | v1 |

**Contract rules**: Emitters must include a `version` field. Consumers must check the version and ignore unknown fields (forward-compatible). The host should log a warning when an event is emitted with no registered consumers (helps catch typos).
