# shux — Product Requirements Document (PRD) v4

**Version:** 4.0
**Date:** 2026-02-19
**Author:** इन्द्रस्वत् (indrasvat)
**Status:** Research-Validated Draft

> `shux` is a modern, batteries-included terminal multiplexer built in Rust. Tiny core, powerful plugin system, first-class support for both humans and AI agents.

---

## 0. Why shux exists

Terminal multiplexers are experiencing a renaissance. Claude Code, Codex CLI, and Gemini CLI are frequently orchestrated with tmux directly or through wrappers in multi-agent workflows. A rapidly growing ecosystem of tmux-wrapping tools — Agent Deck (901 stars), Agent of Empires (743 stars), Agentboard (301 stars), AWS CLI Agent Orchestrator (243 stars), NTM (148 stars) — exists solely because tmux's string-based API is barely adequate for programmatic control. Claude Code's Agent Teams feature is deeply coupled to tmux's control mode. Every single agent orchestration tool in this space wraps tmux. There is no agent orchestrator built on any other multiplexer.

Meanwhile, Zellij proved that a modern, discoverable UX beats tmux's 1990s-era defaults. `cy` showed that session replay is a killer feature. TUIOS (2.4k stars) demonstrated that Go + Bubble Tea can deliver a polished multiplexer experience. Yet no tool combines all of this — a typed API, event-driven architecture, capability-based plugin sandbox, per-pane theming, and agent-first ergonomics — into a cohesive, extensible whole.

**shux is what tmux would be if designed today** — in the age of agentic AI, modern terminals, and developers who expect tools to just work out of the box.

### 0.1 This document

This PRD specifies shux v1.0 completely. Every feature is explicitly scoped (P0/P1/P2), every technical decision is validated against current implementations and documentation, and every architecture choice is justified. v4 incorporates validated research on: Wasmtime v41 / WASI Preview 2, JSON-RPC transport options, nushell's plugin protocol, PI's extension architecture, Zellij's internal architecture, and the exploding agent-orchestration ecosystem.

### 0.2 Non-goals

- Not a terminal emulator (shux runs *inside* your terminal — Ghostty, iTerm2, WezTerm, Kitty, etc.)
- Not a tmux drop-in replacement (different config, different keybindings, different philosophy)
- Not a remote desktop / VNC alternative
- No native Windows support in v1 (architecture must allow it later)
- No multiplayer / real-time collaboration in v1
- No embedded scripting engine in v1 (architecture designed for v2+ scripting layer — Lua/Rhai/Starlark)

### 0.3 v4 validation notes

- Wasmtime v41.0.3 (Feb 2026): WASI Preview 2 stable, WIT snippet parsed and validated, epoch interruption production-ready, hot reload feasible via Store drop/recreate.
- JSON-RPC transport: jsonrpsee lacks native UDS support; hand-rolled JSON-RPC over tokio UDS is the validated approach (matches Zellij's proven pattern).
- Process plugin protocol: Length-prefixed JSON validated against nushell's protocol; added flow control (Ack), plugin GC, and cancellation patterns from nushell and LSP.
- Competitive landscape updated: tmux 3.6a, Zellij 0.43.1 (web client shipped, switching to wasmi), TUIOS 0.6.0 (2.4k stars, more serious than v3 implied).
- PI's extension architecture analyzed: registration-activation separation, tool override by name, event interception/blocking, self-modification via hot reload — patterns incorporated into shux's plugin design.
- Crate versions validated: crossterm 0.29, vte 0.15, ratatui 0.30, pty-process 0.5.3, wasmtime 41.

---

## 1. Design philosophy

Five principles govern every decision in shux. When in conflict, earlier principles win.

### P1: It just works (Ghostty philosophy)

Zero config needed for 90% of use cases. Beautiful defaults. Smart detection of terminal capabilities. No `.shux.toml` archaeology required to get a good experience. The first `shux` invocation should feel polished.

### P2: Fast is a feature

Input-to-render latency is sacred. No feature may regress core latency budgets. The daemon is always responsive. Plugins that misbehave are killed, not tolerated. Users must never feel "the multiplexer is slow."

### P3: Extend, don't bloat

The core is deliberately small: PTY management, layout, rendering, input decoding, API server, plugin host, event bus. Everything else — status bar segments, themes, session persistence, MCP integration, floating panes, session replay — is a plugin. The plugin system is the crown jewel of shux and must be rock-solid, well-documented, and a joy to develop for.

### P4: Humans and agents are equal citizens

Every operation is available through both keyboard and API. State is always queryable, always deterministic. Events are always streamable. An agent should be able to drive shux as effectively as a human — and a human should never need to "learn the API" to use shux normally.

### P5: Prove it works

Every feature ships with automated verification. Visual elements are screenshot-tested. Keyboard navigation is exercised programmatically. API contracts are tested against real daemon instances. "It compiles" is not "it works."

---

## 2. Competitive landscape & differentiation

### 2.1 Landscape (validated February 2026)

| Tool | Stars | Strengths | Weaknesses |
|------|-------|-----------|------------|
| **tmux** (3.6a) | 41.9k | Ubiquitous, battle-tested, session persistence, huge ecosystem, sixel support (opt-in), pane scrollbars, passthrough improvements | String-based API, poor defaults, TERM/color issues, no per-pane theming, no typed automation, painful config, shell-script plugins only, TPM in maintenance mode |
| **Zellij** (0.43.1) | 29.2k | Beautiful OOTB, floating/stacked panes, modes-based keybindings, Wasm plugins, web client (shipped Aug 2025), multiplayer, plugin manager | ~80MB idle memory (vs tmux ~6MB), switching to wasmi interpreter (slower than wasmtime JIT), KDL config is niche, WASI P1 only (no component model), 1.5k open issues |
| **TUIOS** (0.6.0) | 2.4k | Vim-like modal interface, BSP tiling, tape automation, web terminal (WebGL), SSH server mode, Windows support, TOML config, Kitty graphics | Go-based (GC pauses), smaller ecosystem, no typed API |
| **cy** (1.11.1) | 199 | Session replay/time-travel, Janet scripting, daily commits, innovative vision | Small community, obscure scripting language, Go-based |
| **screen** | - | Legacy ubiquity | Effectively unmaintained, ancient UX |

### 2.2 Terminal emulators with built-in multiplexing

| Terminal | Multiplexing | Limitation |
|----------|-------------|------------|
| **Ghostty** (44k stars) | H/V splits, zoom, navigation | No session persistence, no API, no layout automation |
| **WezTerm** (24.3k stars) | Full multiplexer domains, remote mux, Lua config | Last stable release Feb 2024, 1.6k open issues |
| **Warp** | Tabs, splits, synchronized input | Closed-source, evolved into "ADE", no session persistence or API |

### 2.3 Agent orchestration tools (all wrapping tmux)

| Tool | Stars | Description |
|------|-------|-------------|
| **Agent Deck** | 901 | Go/Bubble Tea. Conductor sessions, MCP Socket Pool, fork conversations |
| **Agent of Empires** | 743 | Rust. Multi-agent TUI, git worktrees, Docker sandboxing |
| **Agentboard** | 301 | Web GUI for tmux optimized for AI agent TUIs |
| **AWS CLI Agent Orchestrator** | 243 | Lightweight multi-agent tmux orchestration |
| **NTM** | 148 | Spawns, tiles, coordinates AI agents in tmux panes |
| **Tmux MCP Server** | - | Exposes tmux operations as MCP tools |
| **TmuxAI** | - | Non-intrusive terminal assistant in tmux |

**Key insight**: Every agent orchestration tool wraps tmux because no multiplexer provides a first-class typed API. shux's JSON-RPC API, event streaming, and `ensure` operations directly undercut the need for all these wrappers.

### 2.4 shux's differentiation

1. **Plugin system as crown jewel** — Wasm (WASI Preview 2 + Component Model) + process plugins with capability-based permissions, hot reload, typed WIT interfaces, tool override, event interception, and a rich extension surface. Status bar, themes, floating panes, session persistence, MCP, session replay — all plugins.
2. **Dual-citizen API** — JSON-RPC over Unix domain socket (agent-friendly, zero codegen) + optional gRPC for typed streaming. Every CLI command is a thin API wrapper.
3. **Graded keybindings** — Most common actions on bare keys (no prefix). Less common behind a simple prefix. Fully discoverable via command palette. No memorization needed.
4. **Per-pane theming** — Color-code prod vs dev, highlight active agent panes, theme by project. A long-standing tmux gap.
5. **Observability-first** — Structured logs, tracing spans, metrics, diagnostics TUI, doctor bundle. Not an afterthought.
6. **Agent orchestration substrate** — Designed from day one to replace tmux as the substrate for Claude Code Agent Teams, Agent Deck, and similar tools. Typed events, idempotent `ensure` operations, deterministic state snapshots.
7. **Provably tested** — Multi-layer test pyramid with visual regression, PTY integration, API contract tests, and agent scenario tests.

---

## 3. Target users & jobs-to-be-done

### 3.1 Personas

**P1: Power developer (daily driver)**
Lives in terminal 8+ hours/day. Runs 10-20+ panes across multiple projects. Wants fast muscle-memory navigation, stable sessions, per-project layouts, beautiful defaults. Currently uses tmux with 15+ plugins and a 200-line config.

**P2: SRE / incident responder**
Needs "attach from anywhere," session snapshots, obvious observability, low cognitive overhead under stress. Color-coded panes (prod=red, staging=yellow) are not cosmetic — they prevent mistakes.

**P3: AI coding agent**
Claude Code, Codex CLI, Gemini CLI, or custom agents. Needs full programmatic control: create sessions/panes, run commands, capture output, subscribe to events, verify state. Must be deterministic and idempotent. Increasingly uses MCP for tool integration. Claude Code Agent Teams is the primary integration target.

**P4: Plugin author**
Wants a stable, versioned API surface. Clear extension points. Good docs. Fast iteration cycle (hot reload). Sandboxed but not crippled. Inspired by PI's extension DX — registration should be simple, testing should be fast, debugging should be transparent.

### 3.2 Jobs to be done

| ID | Job | Primary persona |
|----|-----|-----------------|
| JTBD-01 | Keep my workspace alive across sleep, VPN flips, SSH hops | P1, P2 |
| JTBD-02 | Split, navigate, resize, and search without thinking | P1 |
| JTBD-03 | Template my setup per project and share it with my team | P1 |
| JTBD-04 | Drive panes deterministically, capture output, react to events | P3 |
| JTBD-05 | Color-code my environment per task/pane/context | P1, P2 |
| JTBD-06 | Start using shux with zero config and have it look great | P1 |
| JTBD-07 | Extend shux with custom logic without forking | P4 |
| JTBD-08 | Debug what happened in a session after the fact | P1, P2, P3 |
| JTBD-09 | Monitor agent activity across multiple panes at a glance | P1, P3 |
| JTBD-10 | Replace tmux as the substrate for multi-agent workflows | P3 |

---

## 4. Architecture

### 4.1 Single binary, client/server

shux ships as a **single binary** (`shux`) with subcommands. The daemon starts automatically on first use (like tmux's invisible server) and stays running. No separate `shuxd` install or management.

```text
$ shux                              # attach last session (TTY-only); JSON help otherwise
$ shux session create work          # create session named "work" in caller cwd
$ shux session create work --title work  # also pin the initial pane border title
$ shux pane split -s work -d v      # vertical split in current pane
$ shux session list                 # list sessions (alias: shux ses ls)
$ shux session attach work          # attach to "work"
$ shux rpc call system.version      # raw RPC fallthrough
```

The daemon auto-exits when the last session is destroyed (configurable, with a 5-second grace timer to prevent flapping).

### 4.2 System diagram

```text
┌─────────────────────────────────────────────────────┐
│               Host Terminal                          │
│  Ghostty / iTerm2 / WezTerm / Kitty / Warp / ...    │
└───────────────────┬─────────────────────────────────┘
                    │ (PTY I/O, ANSI escape sequences)
                    │
┌───────────────────▼─────────────────────────────────────────┐
│                    shux daemon                               │
│                                                              │
│  ┌────────────┐  ┌──────────────┐  ┌────────────────────┐   │
│  │ PTY Manager│  │ Session Graph│  │ Layout Engine       │   │
│  │ (pty-proc, │  │ (sessions,   │  │ (binary tree +      │   │
│  │  async I/O,│  │  windows,    │  │  plugin overlays)   │   │
│  │  lifecycle)│  │  panes, tags)│  │                     │   │
│  └─────┬──────┘  └──────┬───────┘  └──────────┬─────────┘   │
│        │                │                      │              │
│  ┌─────▼──────┐  ┌──────▼───────┐  ┌──────────▼─────────┐   │
│  │ VT Parser  │  │ Input Decoder│  │ Theme Engine        │   │
│  │ (vte 0.15  │  │ (ANSI, Kitty │  │ (token-based,       │   │
│  │  + custom  │  │  kbd proto,  │  │  per-pane cascade)  │   │
│  │  grid)     │  │  crossterm)  │  │                     │   │
│  └─────┬──────┘  └──────┬───────┘  └──────────┬─────────┘   │
│        │                │                      │              │
│  ┌─────▼────────────────▼──────────────────────▼─────────┐   │
│  │              Core Event Bus                            │   │
│  │  (typed events, broadcast, sequence numbers,           │   │
│  │   gap detection, per-client filtering)                 │   │
│  └──────────┬──────────────────────┬─────────────────────┘   │
│             │                      │                          │
│  ┌──────────▼──────────┐  ┌───────▼────────────────────┐    │
│  │ API Server          │  │ Plugin Host                │    │
│  │ JSON-RPC (UDS+TCP)  │  │ Wasm (wasmtime 41+)       │    │
│  │ hand-rolled,        │  │ + Process plugins          │    │
│  │ length-prefixed     │  │ Sandbox + Permissions      │    │
│  │ gRPC (tonic, opt.)  │  │ Hot reload + GC            │    │
│  └──────────┬──────────┘  └───────┬────────────────────┘    │
└─────────────┼──────────────────────┼────────────────────────┘
              │                      │
   ┌──────────▼──────────┐  ┌───────▼─────────────────┐
   │ Clients             │  │ Plugins                  │
   │ • shux (TUI attach) │  │ • status-bar (bundled)   │
   │ • shux <subcommand> │  │ • theme-pack (bundled)   │
   │ • Python SDK        │  │ • diagnostics (bundled)  │
   │ • Node SDK          │  │ • session-persist (v1.x)  │
   │ • Any JSON-RPC      │  │ • mcp-server (1st-party) │
   │   client            │  │ • floating-panes (1st-p.)│
   └─────────────────────┘  │ • session-replay (1st-p.)│
                            │ • community plugins ...  │
                            └──────────────────────────┘
```

### 4.3 Architectural invariants

1. **Single source of truth**: The `SessionGraph` in the daemon owns all state. Clients are views. State is accessed lock-free via `ArcSwap` snapshots.
2. **CLI == API**: Every `shux` subcommand is a thin wrapper over a JSON-RPC call. There is no "CLI-only" functionality.
3. **Plugins cannot block core**: The plugin host enforces timeouts via wasmtime epoch interruption (~100ms kill threshold). A misbehaving plugin is killed and reported, never stalling the event loop. **Future-design caveat**: Sequential event interception (§7.2a) and any future exhaustive-stream backpressure (§7.6) can delay I/O for specific panes, but never the core render/input loop. Each interceptor has a hard per-call deadline (same 100ms kill threshold); if an interceptor times out, the event passes through unmodified and the plugin is flagged for restart. Current v0 pane-output observation is sampled; byte-exact capture uses daemon-owned `pane.record.*`, not plugin-owned exhaustive streams.
4. **Events are the integration surface**: Plugins, clients, and agents all subscribe to the same typed event stream via `tokio::sync::broadcast`. No polling needed.
5. **Deterministic state**: `state.snapshot` returns the complete graph. `state.apply` is atomic; it is idempotent only when every operation is idempotent or carries an idempotency key. Agents work in read→plan→apply→verify loops.
6. **Single writer, many readers**: All mutations flow through an mpsc channel to a single state-owner task. Readers access state via `ArcSwap` snapshots (lock-free). This avoids locks and serializes mutations naturally (following Zellij's proven pattern).

### 4.4 Key abstractions

| Abstraction | Description |
|-------------|-------------|
| `SessionId`, `WindowId`, `PaneId` | Stable UUIDs (not positional indexes). Survive reattach, reorder, move. |
| `SessionGraph` | Authoritative state tree. HashMap-per-level (sessions, windows, panes). Supports snapshots via `ArcSwap`, diffs, optimistic concurrency (version stamps). |
| `LayoutTree` | Binary split tree per window. Arena-allocated (`indextree` or custom). Plugins can register overlay layers (floating panes, popups). |
| `VirtualTerminal` | Per-pane virtual terminal grid powered by `vte` parser + custom grid (VecDeque-based, following Alacritty's pattern). Maintains scrollback. |
| `RenderCompositor` | Composes VirtualTerminal grids + chrome (borders, status bar, overlays) into per-client output. Diff-based incremental rendering. |
| `Event` | Strongly typed, sequenced (monotonic `AtomicU64`), timestamped. Emitted on the broadcast bus, streamed to subscribed clients/plugins. |
| `ClientCaps` | Negotiated capability profile per attached client (color depth, keyboard protocol, mouse, OSC 52, etc.). Populated at attach via env + escape sequence probes. |

### 4.5 Daemon lifecycle

**Auto-start**: CLI probes the Unix domain socket. On `ConnectionRefused`/`NotFound`, it spawns the daemon process via `fork()` (double-fork with `setsid()` for proper daemonization, using `daemonize` crate or manual `nix`), then retries with exponential backoff.

**Critical**: Fork BEFORE initializing the tokio runtime. `fork()` in a multi-threaded process is undefined behavior.

**Auto-exit**: When the last session is destroyed, start a 5-second grace timer. If no new sessions are created AND no plugins hold a daemon lease, initiate graceful shutdown via `CancellationToken` (from `tokio-util`) propagated to all subsystems.

**Daemon leases**: Long-lived service plugins (e.g., MCP Bridge, SSH Tunnels) can hold a daemon lease to prevent auto-exit while they are active. A plugin acquires a lease by setting `gc = false` in `plugin.toml` AND being in an enabled state. The daemon only auto-exits when: (a) no sessions exist, (b) no plugins hold leases. This prevents the MCP server from disappearing while external agents are connected.

**Signal handling**: `SIGTERM` → graceful shutdown. `SIGHUP` → config reload. Via `tokio::signal::unix`.

---

## 5. Core data model

### 5.1 Entities

```text
Session
  id:            SessionId (UUID)
  name:          String (unique, user-facing)
  created_at:    Timestamp
  windows:       Vec<WindowId> (ordered)
  active_window: WindowId
  env:           Map<String, String>
  theme:         Option<ThemeRef>
  tags:          Map<String, String>   ← plugin-visible metadata
  version:       u64                   ← optimistic concurrency

Window
  id:            WindowId (UUID)
  session:       SessionId
  title:         String
  layout:        LayoutNode (tree)
  active_pane:   PaneId
  cwd:           Option<PathBuf>
  theme:         Option<ThemeRef>
  tags:          Map<String, String>
  version:       u64

Pane
  id:            PaneId (UUID)
  window:        WindowId
  pty:           PtyHandle
  title:         String
  auto_title:    bool               ← derive title from running command
  cwd:           PathBuf
  command:       Vec<String>
  exit_status:   Option<i32>
  restart:       RestartPolicy       ← never | on-fail | always
  theme:         Option<ThemeRef>    ← KEY DIFFERENTIATOR
  tags:          Map<String, String>
  version:       u64
```

### 5.2 Layout tree

```text
LayoutNode =
  | Split { dir: H | V, ratio: f32, a: Box<LayoutNode>, b: Box<LayoutNode> }
  | Leaf  { pane: PaneId }

Invariants:
  - ratio ∈ [0.05, 0.95] (prevents invisible panes)
  - Every Leaf maps to exactly one Pane
  - Resize produces deterministic ratio updates
  - Zoom toggles save/restore previous ratios
```

The layout tree is a binary split tree in core, arena-allocated for cache performance. Plugins (e.g., floating-panes) register **overlay layers** that render above the base layout — the core doesn't need to know about floating panes at all.

### 5.3 Theme cascade

```text
Built-in Default → User Global Theme → Session Theme → Window Theme → Pane Theme → Runtime Override
```

Each level can override individual tokens or reference a named theme. The ThemeEngine resolves the cascade at render time.

### 5.4 Snapshots & diffs

The API provides:
- `state.snapshot` → complete SessionGraph (paginated via `cursor` token for large sessions; each page is a consistent snapshot at the sequence number returned in the response)
- `events.watch` → streaming event deltas (with sequence numbers for gap detection; clients provide `from_seq` to resume; see §8.4 for full contract)
- `state.apply` → atomic delta application (with version-based optimistic concurrency; idempotent only with idempotency keys/deterministic `ensure` ops)

**`state.apply` delta schema**:
```json
{
  "jsonrpc": "2.0", "id": "apply-1", "method": "state.apply",
  "params": {
    "client_request_id": "agent-tx-001",
    "operations": [
      {"op": "session.create", "params": {"name": "work", "cwd": "$PWD"}},
      {"op": "window.create", "params": {"session_id": "$0.id", "name": "editor"}},
      {"op": "pane.split", "params": {"pane_id": "$1.active_pane_id", "direction": "vertical", "command": ["nvim"]}}
    ]
  }
}
```
- Operations execute sequentially within a single transaction. If any operation fails, the entire batch is rolled back (all-or-nothing).
- `$N.field` references allow operations to use results from earlier operations in the same batch (positional back-references).
- `client_request_id` deduplicates the entire batch — replaying the same request is a no-op that returns the cached result.
- `ensure` operations (e.g., `session.ensure`) are inherently idempotent; non-ensure operations require `client_request_id` for safe retries.

Agents operate in **read → plan → apply → verify** loops.

### 5.5 Virtual terminal grid

Each pane maintains a `VirtualTerminal`:
- **Grid**: VecDeque-based (following Alacritty's proven pattern) with configurable scrollback (default 10,000 lines)
- **Cell representation**: Compact 4-byte cells for simple ASCII, extended storage for styled/wide characters
- **Parser**: `vte` crate (v0.15) with the `ansi` feature for typed handler callbacks
- **Lazy allocation**: Scrollback is not pre-allocated for panes that haven't produced output

Memory budget for idle daemon target (60 MB): 10,000 lines × 200 cols × ~4 bytes/cell × 14 panes ≈ 112 MB is too much. Solution: default scrollback of 5,000 lines (configurable), compact cell representation, and lazy allocation.

---

## 6. Exact v1 feature matrix

### 6.1 P0 — Must ship in v1.0

These are the features that make shux a usable daily-driver multiplexer AND a capable agent platform.

#### Core multiplexer

| Feature | Details |
|---------|---------|
| **Sessions** | Create, list, rename, kill, attach, detach. Auto-start daemon on first use. |
| **Windows** | Create, list, rename, kill, reorder, switch by index, switch by fuzzy search, MRU navigation. |
| **Panes** | Split (H/V), directional focus (up/down/left/right), resize (step + mouse drag), zoom/unzoom, close, swap. |
| **Copy mode** | Enter, scroll (keyboard + mouse wheel), search (incremental forward/backward), line selection, copy to system clipboard. |
| **Pane titles** | Manual set/unset, auto-title from running command/cwd (toggleable per pane). |
| **Session templates** | Declarative TOML files defining layout + commands + themes. `shux state apply <template>`. Parameterizable (cwd, env). |

#### UX & keybindings

| Feature | Details |
|---------|---------|
| **Graded keybindings** | Tier 1 (bare keys): `Alt+hjkl` = navigate panes, `Alt+n/p` = next/prev window, `Alt+z` = zoom. Tier 2 (prefix, default `Ctrl+Space`): split, new window, rename, copy mode, command palette, etc. |
| **Command palette** | `Prefix + :` opens searchable command list. Shows keybinding hints. Includes plugin-provided commands. |
| **Discoverability overlay** | `Prefix + ?` shows keybinding cheat sheet, searchable, contextual to current mode. |
| **Mouse support** | Click to focus pane, drag borders to resize, scroll wheel for scrollback. Toggleable globally. Enabled by default. |
| **Status bar** | Rendered by bundled status-bar plugin. Shows session name, window list, active pane info, clock. Clickable segments where terminal supports. |
| **Beautiful defaults** | Ships with a polished dark theme (and a light variant). Borders, colors, status bar look good on first launch with no config. |

#### API & automation

| Feature | Details |
|---------|---------|
| **JSON-RPC API** | Primary API. Hand-rolled over Unix domain socket (length-prefixed framing). TCP loopback opt-in. Covers 100% of operations. |
| **gRPC API** | Optional, for typed streaming use cases. tonic over UDS/TCP. Published `.proto` files. |
| **CLI = API wrapper** | Every `shux <subcommand>` is a JSON-RPC call. `--format json\|text` on all commands. |
| **Idempotent ops** | `ensure session`, `ensure window`, `ensure pane` — create-if-not-exists. Agents love these. |
| **Event stream** | `shux events watch [--filter ...]` — subscribe to typed, sequenced events. JSON lines output. |
| **Optimistic concurrency** | All resources carry `version` stamps. Mutations with stale versions are rejected with actionable errors. |
| **Batch/transaction** | `shux apply` accepts a delta (create window + split + set themes) atomically. |

#### Configuration

| Feature | Details |
|---------|---------|
| **TOML config** | Layered: system < user (`~/.config/shux/config.toml`) < project (`.shux/config.toml` walking up from CWD) < runtime overrides via API. |
| **Live reload** | Config changes trigger live update within 500ms. No daemon restart needed. SIGHUP also triggers reload. |
| **Schema validation** | Invalid config → actionable error with line/column/suggestion. `shux config validate`. |
| **Config explain** | `shux config explain` shows full schema with defaults and descriptions. |

#### Theming

| Feature | Details |
|---------|---------|
| **Token-based themes** | Small, stable token set (bg, fg, accent, border focused/unfocused, status bg/fg, selection, error/warn/info, ANSI palette overrides). |
| **Per-pane theming** | Any pane can reference a named theme and/or override individual tokens. Status bar and borders reflect per-pane theme. |
| **Theme files** | TOML files in `~/.config/shux/themes/`. Plugin-provided themes are auto-discovered. |
| **Live theme editing** | Edit a theme file → shux updates within 500ms. |

#### Plugin system

| Feature | Details |
|---------|---------|
| **Wasm plugins (WASI Preview 2)** | Preferred. Portable, sandboxed, fast (sub-microsecond call overhead). Component Model with WIT-based interface contracts. Powered by wasmtime 41+. |
| **Process plugins** | Any language. Length-prefixed framed JSON over stdio. Experimental in v1.0, disabled by default. |
| **Capability-based permissions** | Plugins declare required permissions in `plugin.toml`. Host enforces at the WASI level (`ResourceLimiter` for memory, epoch interruption for CPU). |
| **Hot reload** | Enable/disable/reload plugins at runtime without daemon restart. Wasm: drop Store, re-instantiate. Process: restart process. |
| **Versioned API** | Plugin API is semver'd (`shux:plugin@1.0.0`). Incompatible plugins rejected with clear error via Hello handshake. |
| **Extension points** | Commands, status bar segments, pane overlays, theme packs, event reactors, exporters, layout providers, input handlers, API extensions. |
| **Plugin lifecycle** | Discover → Validate → Enable → Start → Handle events → Stop → Disable. Plugin GC for idle process plugins (configurable, default 30s). |
| **Registration-activation separation** | During loading, plugins can only register handlers/commands. Action methods become available only after binding (prevents initialization order bugs — inspired by PI). |
| **Tool override** | Plugins can override built-in commands by registering the same name (inspired by PI). The last-loaded wins, with conflict detection and user notification. |
| **Event interception** | Plugins can intercept, modify, or block events before they propagate (inspired by PI's `tool_call` gate pattern). Enables permission gates, audit logging, access control. |

#### Observability

| Feature | Details |
|---------|---------|
| **Structured logging** | JSON + human-readable modes. Configurable sinks (stderr, rotating file, syslog/journald, macOS unified log). Correlation IDs. |
| **Tracing spans** | Key paths instrumented: input decode, render, PTY I/O, API calls. OTLP export optional. |
| **Metrics** | Input decode latency, PTY throughput, render time, event bus lag, API latencies. |
| **Diagnostics overlay** | TUI overlay showing health, PTY status, renderer stats, events, plugins, capabilities. |
| **Doctor bundle** | `shux doctor` → collects config, caps, plugin status, recent errors, terminal info. Redaction options. |

#### Terminal compatibility

| Feature | Details |
|---------|---------|
| **Capability negotiation** | At attach: detect TERM, TERM_PROGRAM, COLORTERM, DA2, XTVERSION, Kitty keyboard query. Store as `ClientCaps` per client. |
| **Truecolor** | Full 24-bit color when available, graceful degradation to 256/8. |
| **Modern keyboard** | Kitty keyboard protocol when available (crossterm 0.29 `PushKeyboardEnhancementFlags`), legacy fallback. |
| **OSC 8 hyperlinks** | Passthrough when terminal supports (raw escape sequence emission, as crossterm lacks built-in OSC 8). |
| **OSC 52 clipboard** | Supported via crossterm 0.29. |
| **Image passthrough** | DCS passthrough for focused pane (like tmux `allow-passthrough`). Kitty graphics, Sixel, iTerm2 inline images pass through to host terminal. Images cleared on pane switch. |
| **Synchronized output** | Mode 2026 via crossterm `BeginSynchronizedUpdate`/`EndSynchronizedUpdate`. |

#### Security

| Feature | Details |
|---------|---------|
| **Local-only by default** | UDS with `0700` permissions in user-owned directory. TCP loopback opt-in. |
| **Auth token** | TCP API requires token (auto-generated, strict file perms) unless explicitly disabled. |
| **Plugin sandbox** | Wasm: WASI sandbox + `ResourceLimiter` for memory caps + epoch interruption for CPU timeouts. Process: disabled by default, platform-dependent isolation when enabled. |

### 6.2 P1 — Should ship in v1.x (first-party plugins + enhancements)

| Feature | Implementation |
|---------|----------------|
| **Floating panes** | First-party plugin. Overlay layer on layout engine. Scratch terminals, popups, inline help. |
| **Session persistence** | First-party plugin. Save/restore session graph across reboots (Zellij-style: serialize as layout files every 1s, "Press ENTER to run..." on restore for safety). |
| **MCP server** | First-party plugin. Exposes shux operations as MCP tools for Claude Code, Codex, Gemini CLI. |
| **Session replay** | First-party plugin. Records events + PTY output. Replay UX to rewind any pane to any point (inspired by cy). |
| **Python SDK** | Generated from API schema. High-level helpers for ensure/watch/capture. |
| **Node SDK** | Generated from API schema. Same helpers. |
| **Pane respawn** | `restart=never\|on-fail\|always` policy per pane. |
| **URL picker** | Plugin to detect and open URLs in pane output. |
| **Notification on exit** | Plugin to notify when a long-running command finishes. |
| **Fuzzy session/window picker** | Enhanced chooser with preview (like tmux-sessionx). |

### 6.3 P2 — Future (v2+)

| Feature | Notes |
|---------|-------|
| Scripting layer | Embed Lua (mlua), Rhai, or Starlark for interactive composition. Architecture designed for this from v1. |
| Multiplayer / collaboration | Real-time terminal sharing with multiple clients (Zellij shipped this) |
| Web client | Access sessions from browser (Zellij has this via Aug 2025 release) |
| Windows native support | ConPTY, named pipes |
| Remote federation | Multi-host session management |
| Plugin marketplace/registry | Centralized discovery + install (plugin.toml metadata fields ready from v1) |
| Stacked panes | Zellij-style tabbed panes within a split |
| Modes (Zellij-style) | Plugin providing mode-based keybinding scheme as alternative to prefix model |

---

## 7. Plugin system — the crown jewel

The plugin system is shux's primary differentiator. It must be so good that the community builds on it, and so well-designed that first-party features are implemented as plugins (proving the API surface is sufficient).

### 7.1 Design goals

1. **Plugins are first-class**: Bundled v1.0 plugins (status-bar, theme-pack, diagnostics) and first-party v1.x plugins (session-persist, MCP, replay) prove the plugin API by using it. If a first-party plugin can't do something, the API is incomplete.
2. **Safe by default**: Wasm sandbox + capability-based permissions via WASI + `ResourceLimiter`. A theme plugin cannot read your filesystem. A status-bar plugin cannot send network requests (unless granted).
3. **Hot reloadable**: Enable/disable/reload at runtime. Wasm: drop `Store`, re-instantiate from new `.wasm` file. Process: kill and restart. State is reconstructed from persistent metadata, not in-memory continuity.
4. **Language-agnostic**: Wasm plugins work from Rust (excellent), Go/TinyGo (good), C/C++ (good), Python/componentize-py (experimental), JS/componentize-js (experimental). Process plugins work from any language with stdin/stdout.
5. **Debuggable**: Plugin logs are tagged and routable. Plugin performance is traced (per-plugin call latency histogram). Plugin errors are reported clearly with stack traces, never silently swallowed (following PI's error isolation pattern).
6. **Composable**: Plugins can communicate via the shared event bus (namespaced events). A git-status plugin can emit events that a status-bar plugin consumes.
7. **Overridable**: Plugins can override built-in commands by registering the same name (PI's tool override pattern). Users can customize any behavior without forking.
8. **Interception-capable**: Plugins can intercept, modify, or block events before they propagate (PI's event gate pattern). In v1.0, interception is scoped to events listed in the `intercept_events` permission (primarily `pane.input` for command-gating). API-level operation interception (e.g., gating `pane.create` calls) is a v2 extension point. This enables permission gates, audit logging, and policy enforcement.

### 7.2a Interception chain semantics

When multiple plugins intercept the same event type, they execute as a **sequential chain** in the order they appear in `shux.toml`'s `[plugins]` section. Rules:

1. Interceptors are called sequentially, not in parallel.
2. If a plugin returns `None` (block), the chain terminates immediately — no subsequent plugins or the core receive the event.
3. If a plugin returns a modified event, the next plugin in the chain receives the modified version.
4. If all plugins pass the event through, the core processes the original (or cumulatively modified) event.

This gives users explicit control over interception priority via config ordering. Example: Danger Zone should typically be first (so it can block before other interceptors see the event).

**Input interception timing**: `pane.input` events fire on **line submission** (when Enter/Return is pressed), not per-keystroke. The host buffers characters locally as they are typed to the PTY. When a newline is detected, the host pauses delivery to the PTY, calls the interception chain, and either forwards the buffered line (if all interceptors pass) or discards it (if any interceptor blocks). If blocked, the host sends a line-kill sequence to the PTY to clear any partially echoed input from the shell's line buffer. This avoids per-keystroke latency overhead while still catching dangerous commands before execution.

**Interceptor failure behavior (fail-closed)**: If an interceptor plugin crashes, times out (100ms), or returns an error during `intercept-event`, the event is **blocked** (fail-closed) and the host emits a `plugin.error` event. This is the safe default — a crashing Danger Zone plugin should not accidentally allow dangerous commands through. The user sees a host-rendered overlay: `Plugin <name> failed during interception — command blocked. Press Enter to dismiss.` The plugin is marked degraded and subsequent events bypass it with a warning until the plugin is reloaded.

### 7.2b Overlay z-ordering

Each pane maintains an **overlay stack**. When multiple plugins call `show-overlay` on the same pane:

1. Overlays are stacked in the order `show-overlay` was called (most recent on top).
2. The topmost overlay receives `on-overlay-input` first. If it returns `true` (consumed), the key is not passed further. If it returns `false`, the key falls through to the next overlay, then to the pane.
3. `render-overlay` is called for all visible overlays; the host composites them with the topmost overlay rendered last (on top).
4. `hide-overlay` removes only that plugin's overlay from the stack.
5. If a plugin calls `show-overlay` while it already has an overlay on that pane, it replaces its existing overlay (same stack position) rather than adding a new one.

### 7.2 Extension points

| Extension point | What plugins can do | Example |
|-----------------|--------------------|---------|
| **Commands** | Register new commands for CLI + command palette | `git-branch` command that creates a pane per branch |
| **Command overrides** | Replace built-in commands with custom implementations | Audited `read` that logs all file access |
| **Status bar segments** | Add left/center/right segments with text, colors, click handlers | Git branch, k8s context, battery, clock |
| **Pane overlays** | Render text/UI above pane content | Floating panes, notification badges, error markers |
| **Theme packs** | Provide named themes | Catppuccin, Gruvbox, Tokyo Night, Nord |
| **Event reactors** | Subscribe to events and trigger actions | "On pane exit error, set border red" |
| **Event interceptors** | Block, modify, or gate events before propagation | Permission gates, dangerous command confirmation |
| **Exporters** | Export data from shux | Prometheus metrics exporter, OTLP trace exporter |
| **Layout providers** | Register named layouts | "IDE", "chat", "monitoring" presets |
| **Input handlers** | Intercept and transform input | Vim-style modes plugin, leader key plugin |
| **API extensions** | Register new API endpoints | Session replay plugin exposing `replay.seek` |
| **Lifecycle hooks** | React to daemon/session lifecycle events | Auto-save on shutdown, setup on session create |
| **Inter-plugin bus** | Namespaced pub/sub between plugins | Git plugin emits branch change, status bar consumes |

### 7.3 Plugin packaging

```text
my-plugin/
├── plugin.toml         # Metadata, permissions, extension points
├── plugin.wasm         # Wasm module (or 'bin/' for process plugin)
├── themes/             # Optional: theme files
├── layouts/            # Optional: layout templates
├── README.md           # Required: user-facing docs
└── CHANGELOG.md        # Optional
```

### 7.4 Plugin manifest (`plugin.toml`)

```toml
[plugin]
id = "com.example.git-status"
name = "Git Status"
version = "0.1.0"
api = "shux:plugin@1.0.0"        # matches the WIT package version; compatibility: major must match
kind = "wasm"                    # "wasm" or "process"
description = "Git-aware status bar segments and pane badges"
homepage = "https://github.com/example/shux-git-status"
license = "MIT"
min_shux = "1.0.0"

# Registry-extensible metadata (ready for v2 marketplace)
[plugin.metadata]
categories = ["git", "status-bar", "developer-tools"]
keywords = ["git", "branch", "status", "vcs"]
icon = "🔀"

[permissions]
events = ["pane.focused", "pane.cwd_changed", "window.activated"]
read_pane_output = false         # Can call read-pane-output / read-pane-scrollback
send_keys = false                # Can call send-keys (input injection - security-sensitive)
manage_panes = false             # Can create, close, resize, focus, split panes and layouts
manage_sessions = false          # Can create, kill, rename sessions and windows
api_extensions = false           # Can register new JSON-RPC API methods
exec = false                     # SUPER-PERMISSION: can run arbitrary subprocess commands via the
                                 # exec() host function. Effectively bypasses fs_read/fs_write
                                 # restrictions since the subprocess can access the full filesystem.
                                 # Grant only to trusted plugins. When granted, subprocesses inherit
                                 # the daemon's user but run with a scrubbed environment (only PATH,
                                 # HOME, TERM, LANG, and plugin-declared env vars). CWD is restricted
                                 # to the pane's CWD or a plugin-declared working directory.
                                 # Max execution time: 30s (configurable).
fs_read = ["/abs/workspaces/**/.git/**"]  # Absolute scopes; host enforces canonicalized paths
fs_write = []
network = false
clipboard = false
intercept_events = []            # Events this plugin can block/modify
override_commands = []           # Built-in commands this plugin replaces

# Plugin dependencies (for v2 marketplace resolution)
[dependencies]
# "com.example.other-plugin" = ">=0.2.0"

# Conflict declarations
[conflicts]
# "com.example.incompatible" = "*"

[extensions]
status_segments = ["git_branch", "git_dirty"]
commands = ["git-status.refresh"]
themes = []
```

### 7.5 Wasm plugin interface (WIT)

**Validated**: This WIT parses correctly with current WIT tooling (February 2026). `result<_, error-type>` is valid WIT syntax — the underscore means "no associated data" (maps to `()` in Rust).

The WIT is designed as a **full control plane**, not just a read/display API. Plugins can observe the multiplexer, decorate it, AND control it — creating panes, sending input, reading output, managing sessions, and building interactive overlays. Every host function is gated by capability-based permissions declared in `plugin.toml`.

```wit
package shux:plugin@1.0.0;

interface host {
  // ═══════════════════════════════════════════════
  // Types
  // ═══════════════════════════════════════════════

  record pane-info {
    id: string,
    title: string,
    cwd: string,
    command: string,
    is-focused: bool,
    width: u16,
    height: u16,
    exit-code: option<s32>,
    tags: list<key-value>,
  }

  record window-info {
    id: string,
    name: string,
    pane-ids: list<string>,
    active-pane-id: string,
  }

  record session-info {
    id: string,
    name: string,
    window-ids: list<string>,
    active-window-id: string,
    created-at: u64,
  }

  record key-value {
    key: string,
    value: string,
  }

  enum log-level {
    trace,
    debug,
    info,
    warn,
    error,
  }

  enum split-direction {
    horizontal,
    vertical,
  }

  record host-error {
    code: s32,
    message: string,
  }

  record pane-create-options {
    window-id: string,
    command: option<string>,
    cwd: option<string>,
    env: list<key-value>,
    name: option<string>,
  }

  record split-options {
    target-pane-id: string,
    direction: split-direction,
    size-percent: option<u8>,
    command: option<string>,
    cwd: option<string>,
  }

  record floating-pane-options {
    width: option<u16>,
    height: option<u16>,
    x: option<u16>,
    y: option<u16>,
    command: option<string>,
    cwd: option<string>,
    name: option<string>,
  }

  // ═══════════════════════════════════════════════
  // Queries (no permissions required)
  // ═══════════════════════════════════════════════

  get-active-pane: func() -> result<pane-info, host-error>;
  get-pane: func(id: string) -> result<pane-info, host-error>;
  list-panes: func() -> result<list<pane-info>, host-error>;
  get-active-window: func() -> result<window-info, host-error>;
  get-window: func(id: string) -> result<window-info, host-error>;
  list-windows: func() -> result<list<window-info>, host-error>;
  get-active-session: func() -> result<session-info, host-error>;
  get-session: func(id: string) -> result<session-info, host-error>;
  list-sessions: func() -> result<list<session-info>, host-error>;
  get-config: func(key: string) -> result<option<string>, host-error>;

  // ═══════════════════════════════════════════════
  // Pane lifecycle (requires: manage_panes)
  // ═══════════════════════════════════════════════

  create-pane: func(options: pane-create-options) -> result<string, host-error>;
  split-pane: func(options: split-options) -> result<string, host-error>;
  create-floating-pane: func(options: floating-pane-options) -> result<string, host-error>;
  close-pane: func(pane-id: string) -> result<_, host-error>;
  toggle-floating-pane: func(pane-id: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // Pane interaction (requires: send_keys / read_pane_output)
  // ═══════════════════════════════════════════════

  /// Send input to a pane's PTY input stream (as if typed by the user).
  /// Bytes are written directly to the PTY master fd — this is async and
  /// does not guarantee the shell is ready to receive them. Use list<u8>
  /// because terminal input may include raw escape sequences that are not
  /// valid UTF-8 when split across calls.
  send-keys: func(pane-id: string, data: list<u8>) -> result<_, host-error>;

  /// Convenience wrapper: send UTF-8 text to a pane's PTY input stream.
  send-text: func(pane-id: string, text: string) -> result<_, host-error>;

  /// Read the last N lines of visible output from a pane's virtual terminal.
  /// Returns rendered text (UTF-8, with ANSI stripped). For raw byte-exact
  /// transcripts, use daemon-owned pane.record.*; pane.output is sampled live
  /// observation in current v0 builds.
  read-pane-output: func(pane-id: string, lines: u32) -> result<string, host-error>;

  /// Read from pane scrollback. offset=0 is the most recent scrollback line.
  read-pane-scrollback: func(pane-id: string, offset: u32, lines: u32) -> result<string, host-error>;

  // ═══════════════════════════════════════════════
  // Pane manipulation (requires: manage_panes)
  // ═══════════════════════════════════════════════

  focus-pane: func(pane-id: string) -> result<_, host-error>;
  resize-pane: func(pane-id: string, width: u16, height: u16) -> result<_, host-error>;
  rename-pane: func(pane-id: string, name: string) -> result<_, host-error>;
  set-pane-tag: func(pane-id: string, key: string, value: string) -> result<_, host-error>;
  clear-pane-tag: func(pane-id: string, key: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // Window/session lifecycle (requires: manage_sessions)
  // ═══════════════════════════════════════════════

  create-session: func(name: string) -> result<string, host-error>;
  create-window: func(session-id: string, name: string) -> result<string, host-error>;
  close-window: func(window-id: string) -> result<_, host-error>;
  kill-session: func(session-id: string) -> result<_, host-error>;
  rename-session: func(session-id: string, name: string) -> result<_, host-error>;
  rename-window: func(window-id: string, name: string) -> result<_, host-error>;
  focus-window: func(window-id: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // Layout (requires: manage_panes)
  // ═══════════════════════════════════════════════

  /// Register a named layout template. layout-json follows the shux layout schema.
  register-layout: func(name: string, layout-json: string) -> result<_, host-error>;

  /// Apply a registered layout to the active window.
  apply-layout: func(name: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // Display & status (no extra permissions)
  // ═══════════════════════════════════════════════

  set-status-segment: func(id: string, text: string) -> result<_, host-error>;
  set-badge: func(pane-id: string, badge: string) -> result<_, host-error>;
  clear-badge: func(pane-id: string) -> result<_, host-error>;
  emit-event: func(event-type: string, data-json: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // Overlays (no extra permissions; interactive overlays require input routing)
  // ═══════════════════════════════════════════════

  /// Show a plugin-managed overlay on a pane. While visible, the plugin
  /// receives on-overlay-input callbacks for keystrokes in that pane.
  show-overlay: func(pane-id: string) -> result<_, host-error>;

  /// Hide a plugin-managed overlay.
  hide-overlay: func(pane-id: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // Clipboard (requires: clipboard)
  // ═══════════════════════════════════════════════

  get-clipboard: func() -> result<string, host-error>;
  set-clipboard: func(content: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // API extension (requires: api_extensions)
  // ═══════════════════════════════════════════════

  /// Register a new JSON-RPC method. When called by external clients,
  /// the plugin receives an on-command callback with the method name.
  /// Method names MUST be prefixed with the plugin's short ID (e.g.,
  /// "replay.seek", "agent.status") to prevent namespace collisions.
  /// The host rejects names that collide with built-in methods or
  /// already-registered methods from other plugins.
  register-api-method: func(method-name: string, description: string) -> result<_, host-error>;

  /// Register a command override. Replaces a built-in command with the
  /// plugin's on-command handler for that name. Requires override_commands
  /// permission listing the specific command name.
  register-command-override: func(command-name: string) -> result<_, host-error>;

  // ═══════════════════════════════════════════════
  // Utilities
  // ═══════════════════════════════════════════════

  log: func(level: log-level, msg: string);
  read-file: func(path: string) -> result<list<u8>, host-error>;   // requires fs_read
  write-file: func(path: string, data: list<u8>) -> result<_, host-error>;  // requires fs_write

  /// Run a command and return its stdout. Requires exec permission.
  /// The command runs in a subprocess (not a pane). Max 30s timeout.
  exec: func(command: string, args: list<string>) -> result<string, host-error>;
}

interface plugin {
  record plugin-error {
    code: s32,
    message: string,
  }

  // Lifecycle
  init: func(config-json: string) -> result<_, plugin-error>;
  shutdown: func();

  // Event handling
  on-event: func(event-json: string) -> result<_, plugin-error>;

  // Event interception (return modified event JSON, or None to block)
  intercept-event: func(event-json: string) -> result<option<string>, plugin-error>;

  // Commands (also serves API extension callbacks and command overrides)
  on-command: func(name: string, args: list<string>) -> result<string, plugin-error>;

  // Rendering — status bar segments
  render-segment: func(id: string, width: u16) -> result<string, plugin-error>;

  // Rendering — pane overlays (return ANSI-styled text to display, or None to hide)
  render-overlay: func(pane-id: string, width: u16, height: u16) -> result<option<string>, plugin-error>;

  // Interactive overlay input — called when a keypress occurs while this
  // plugin's overlay is visible on the given pane. Return true to consume
  // the key, false to pass it through to the next overlay in the stack
  // (or to the pane if this is the bottom overlay). See Section 7.2b for
  // z-ordering rules.
  on-overlay-input: func(pane-id: string, key-event-json: string) -> result<bool, plugin-error>;
}

world runtime {
  import host;
  export plugin;
}
```

**Design rationale**: The WIT is structured as a **graduated control plane** with four tiers of power, each gated by permissions:

| Tier | Functions | Permission required |
|------|-----------|-------------------|
| **Read** | `get-*`, `list-*`, `get-config` | None (all plugins) |
| **Display** | `set-status-segment`, `set-badge`, `emit-event`, `show/hide-overlay` | None (all plugins) |
| **Control** | `create-pane`, `split-pane`, `send-keys`, `focus-pane`, `close-pane`, etc. | `manage_panes`, `send_keys`, `manage_sessions` |
| **Extend** | `register-api-method`, `register-command-override`, `exec` | `api_extensions`, `override_commands`, `exec` |

A theme plugin stays in Tier 1-2. An agent orchestrator gets Tier 3. A full MCP bridge gets Tier 4. The sandbox is meaningful at every level.

**Performance**: Wasmtime v41 call overhead is sub-microsecond for simple calls. A `render-segment` returning a short string takes <100μs. An `on-event` processing a small JSON event takes <500μs. The 5ms budget per plugin call is easily met. Heavier operations (create-pane, read-pane-output) are inherently async on the host side and the plugin blocks until completion, but these are infrequent control-plane operations, not hot-path rendering.

**Hot reload**: Drop the old `Store` (frees all Wasm instances, memories, tables), then create a new `Store` and re-instantiate from the (potentially new) `.wasm` file. The `Engine` and `Linker` are shared and persist. Registered API methods, command overrides, and layouts are re-registered during `init`.

**Sandboxing**:
- **Memory**: `ResourceLimiter` trait limits memory growth per plugin (e.g., 16 MB cap)
- **CPU**: Epoch interruption (~10% overhead) with configurable deadline; kill at 100ms
- **Filesystem**: WASI capabilities map to declared `fs_read`/`fs_write` paths
- **Network**: Disabled unless explicitly granted
- **Pane/session control**: Disabled unless `manage_panes`/`manage_sessions` granted
- **Input injection**: Disabled unless `send_keys` granted (critical for security)
- **Command execution**: Disabled unless `exec` granted

### 7.6 Process plugin protocol

Length-prefixed framed JSON over stdio (inspired by nushell, validated against nushell's protocol).

**Frame format**: 4-byte big-endian payload length + UTF-8 JSON payload.

```text
shux daemon ──spawn──► plugin binary
  stdin:  framed messages (requests/events from host)
  stdout: framed messages (responses/registrations from plugin)
  stderr: routed to shux log system (tagged with plugin ID)
```

**Handshake**:
```json
// Host → Plugin (first message)
{"type": "hello", "protocol": "shux-plugin", "version": "1.0.0", "plugin_id": "com.example.my-plugin", "features": []}

// Plugin → Host (first response)
{"type": "hello", "protocol": "shux-plugin", "version": "1.0.0", "features": []}
```

Version compatibility: `0.x.y` and `0.x'.y'` are incompatible if `x != x'`; for `>=1.0.0`, major version must match (following nushell's rule).

**Message types**:
```json
// ── Host → Plugin (requests — have "id", expect response) ──
{"type": "invoke", "id": "req-1", "command": "my-plugin.refresh", "args": ["--force"]}
{"type": "render", "id": "req-2", "segment": "my_segment", "width": 30}
{"type": "render_overlay", "id": "req-3", "pane_id": "p-1", "width": 80, "height": 24}
{"type": "intercept", "id": "req-4", "event": {"type": "pane.focused", ...}}
{"type": "overlay_input", "id": "req-5", "pane_id": "p-1", "key_event": {"key": "y", "modifiers": ["ctrl"]}}

// ── Host → Plugin (notifications — no "id", no response expected) ──
// Events carry optional "stream_id" for flow-control (ACK/DROP) linkage.
{"type": "event", "stream_id": 42, "event": {"type": "pane.focused", "pane_id": "abc-123", ...}}
{"type": "signal", "signal": "interrupt"}
{"type": "shutdown"}

// ── Plugin → Host (responses — echo request "id") ──
{"type": "result", "id": "req-1", "data": {...}}

// ── Plugin → Host (registration — sent once after hello) ──
{"type": "register", "commands": [...], "segments": [...], "themes": [...],
 "api_methods": [...], "command_overrides": [...], "layouts": [...]}

// ── Plugin → Host (display actions) ──
{"type": "set_status", "segment_id": "my_segment", "content": {"text": " main", "fg": "#a6e3a1"}}
{"type": "set_badge", "pane_id": "abc-123", "badge": "!"}
{"type": "clear_badge", "pane_id": "abc-123"}
{"type": "show_overlay", "pane_id": "abc-123"}
{"type": "hide_overlay", "pane_id": "abc-123"}
{"type": "log", "level": "info", "msg": "Refreshed git status"}
{"type": "emit_event", "event_type": "git.branch_changed", "data": {"branch": "main"}}

// ── Plugin → Host (pane/session control — requires permissions) ──
{"type": "create_pane", "id": "req-10", "window_id": "w-1", "command": "bash", "cwd": "/home/user", "env": {}, "name": "worker-1"}
{"type": "split_pane", "id": "req-11", "target_pane_id": "p-1", "direction": "vertical", "size_percent": 30, "command": "tail -f app.log"}
{"type": "create_floating_pane", "id": "req-12", "width": 80, "height": 24, "command": "python3"}
{"type": "close_pane", "id": "req-13", "pane_id": "p-2"}
{"type": "toggle_floating_pane", "id": "req-14", "pane_id": "p-3"}
{"type": "focus_pane", "id": "req-15", "pane_id": "p-1"}
{"type": "resize_pane", "id": "req-16", "pane_id": "p-1", "width": 120, "height": 40}
{"type": "send_keys", "id": "req-17", "pane_id": "p-1", "keys": "ls -la\n"}
{"type": "read_pane_output", "id": "req-18", "pane_id": "p-1", "lines": 50}
{"type": "read_pane_scrollback", "id": "req-19", "pane_id": "p-1", "offset": 0, "lines": 100}
{"type": "set_pane_tag", "pane_id": "p-1", "key": "lang", "value": "rust"}

// ── Plugin → Host (session/window control — requires manage_sessions) ──
{"type": "create_session", "id": "req-20", "name": "dev"}
{"type": "create_window", "id": "req-21", "session_id": "s-1", "name": "logs"}
{"type": "close_window", "id": "req-22", "window_id": "w-2"}
{"type": "kill_session", "id": "req-23", "session_id": "s-1"}
{"type": "focus_window", "id": "req-24", "window_id": "w-2"}

// ── Plugin → Host (layout control — requires manage_panes) ──
{"type": "register_layout", "name": "monitoring", "layout_json": "..."}
{"type": "apply_layout", "id": "req-25", "name": "monitoring"}

// ── Plugin → Host (API extension — requires api_extensions) ──
{"type": "register_api_method", "method_name": "replay.seek", "description": "Seek to a timestamp in replay"}
{"type": "register_command_override", "command_name": "pane.create"}

// ── Plugin → Host (clipboard — requires clipboard) ──
{"type": "get_clipboard", "id": "req-26"}
{"type": "set_clipboard", "id": "req-27", "content": "copied text"}

// ── Plugin → Host (queries — always available) ──
{"type": "get_pane", "id": "req-28", "pane_id": "p-1"}
{"type": "list_panes", "id": "req-29"}
{"type": "get_window", "id": "req-30", "window_id": "w-1"}
{"type": "list_windows", "id": "req-31"}
{"type": "get_session", "id": "req-32", "session_id": "s-1"}
{"type": "list_sessions", "id": "req-33"}
{"type": "get_config", "id": "req-34", "key": "theme.active"}

// ── Flow control (for high-volume event streams like pane.output) ──
{"type": "ack", "stream_id": 42}            // Plugin acknowledges receipt
{"type": "drop", "stream_id": 42}           // Plugin no longer wants this stream

// ── Event subscription (sampled live observation) ──
{"type": "subscribe", "event_type": "pane.output", "pane_id": "p-1", "exhaustive": true}
// Current v0 pane.output is sampled/coalesced and suitable for live
// observation, status updates, and pattern matching. Byte-exact transcripts
// use the daemon-owned pane.record.start / pane.record.stop path instead.
// A future exhaustive plugin stream would need a separate backpressure and
// permission design; do not infer lossless semantics from pane.output today.

// ── Cancellation ──
{"type": "cancel", "id": "req-1"}           // Host cancels an in-flight request
```

**Process plugin parity**: The process plugin protocol mirrors the WIT host interface 1:1. Every WIT function has a corresponding JSON message type. Permission enforcement is identical — the daemon checks `plugin.toml` permissions before executing any action, returning an error response if the permission is not granted.

**Flow control bounds**: Each plugin has a bounded outbound event buffer (default: 256 events). If a plugin stops `ack`-ing but does not exit, the host drops new events for that plugin (logging a warning) rather than growing the buffer unbounded. Future exhaustive plugin streams would need explicit PTY backpressure, stall detection, and permission review. Current v0 pane output remains sampled for plugin/live observation; `pane.record.*` owns byte-exact capture.

**Plugin GC**: Process plugins idle for more than 30 seconds (configurable) receive `shutdown` and are stopped. Plugins can opt out of GC in `plugin.toml`:

```toml
[plugin]
gc = false  # Keep running indefinitely (for long-lived services)
```

### 7.7 Bundled plugins (ship with v1.0)

These are first-party plugins that ship inside the shux binary (compiled-in Wasm or native). They prove the plugin API and provide OOTB functionality.

| Plugin | Purpose | Extension points used |
|--------|---------|----------------------|
| **shux-status-bar** | Default status line: session, windows, pane info, clock | Status segments, event reactors |
| **shux-theme-pack** | Ships 5 themes: default-dark, default-light, prod (red accent), solarized, catppuccin-mocha | Theme packs |
| **shux-diagnostics** | `shux doctor` + TUI diagnostics overlay | Commands, pane overlays |

### 7.8 Plugin development experience

Plugin development must be a joy. Key DX features:

1. **Scaffold**: `shux plugin init my-plugin --kind wasm` generates a complete plugin project with `plugin.toml`, `Cargo.toml` (for Rust), WIT bindings, and a working example.
2. **Dev mode**: `shux plugin dev ./my-plugin/` watches for changes, recompiles, and hot-reloads automatically.
3. **Testing**: `shux plugin test ./my-plugin/` runs the plugin in an isolated daemon with a test harness.
4. **Inspect**: `shux plugin inspect com.example.my-plugin` shows permissions, extension points, health, and latency stats.
5. **Logs**: Plugin logs are tagged with the plugin ID and routable to separate files. `shux logs tail --plugin com.example.my-plugin`.

---

## 8. API design — JSON-RPC primary, gRPC optional

### 8.1 Transport

| Transport | Default | Use case |
|-----------|---------|----------|
| JSON-RPC over Unix domain socket | ON (always) | Primary. Length-prefixed framing (4-byte BE length + JSON payload). Same codec as process plugin protocol. Low latency, no auth needed (fs perms). |
| JSON-RPC over TCP 127.0.0.1 | OFF (opt-in) | For tools that can't use UDS. Requires auth token. Same framing. |
| gRPC over UDS/TCP | OFF (opt-in) | For clients that want typed streaming (protobuf codegen). tonic with UDS connector. Published `.proto` files. gRPC over TCP requires the same auth token as JSON-RPC TCP. |

**Max frame size**: All length-prefixed transports enforce a 16 MB maximum payload. Frames exceeding this limit are rejected immediately with error code `-32001` (`frame_too_large`) and the connection is closed. This prevents memory exhaustion from oversized or malicious payloads.

**Why hand-rolled JSON-RPC (not jsonrpsee)**: jsonrpsee v0.26 lacks native UDS support. Its `TowerService` adapter requires HTTP/WebSocket framing overhead. Hand-rolled gives zero transport impedance — raw JSON-RPC directly over framed UDS. Matches Zellij's proven pattern (framed messages over UDS, just with protobuf). Can be tested with `socat`. Shares the framing codec with the process plugin protocol.

**Implementation**: `tokio::net::UnixListener` + `tokio_util::codec::LengthDelimitedCodec` + `serde_json` + `json-rpc-types` crate (for JSON-RPC 2.0 request/response/error type definitions).

### 8.2 JSON-RPC method naming

Methods follow a `<resource>.<action>` convention:

```text
// Health & version
system.version
system.health

// State
state.snapshot
state.apply

// Sessions
session.list
session.create
session.ensure          ← idempotent create-if-not-exists
session.rename
session.kill
session.attach

// Windows
window.list
window.create
window.ensure
window.rename
window.focus
window.reorder
window.kill

// Panes
pane.list
pane.split
pane.ensure
pane.focus
pane.resize
pane.zoom
pane.swap
pane.kill
pane.send_keys
pane.run_command         ← run command; default waits for completion and returns exit code + captured stdout/stderr.
                            async=true returns command_id immediately; poll status via pane.command_status(command_id),
                            cancel via pane.command_cancel(command_id). Completion emits pane.command_completed event
                            with {command_id, exit_code, stdout, stderr}. Default timeout: 300s (configurable per call).
pane.command_status      ← poll a running async command: returns {state: running|completed|failed, exit_code, runtime_ms}
pane.command_cancel      ← cancel a running async command by command_id (sends SIGTERM, then SIGKILL after 5s)
pane.capture             ← capture scrollback content
pane.set_title
pane.set_cwd
pane.set_env
pane.set_theme
pane.set_theme_override
pane.set_tag
pane.get_tags

// Copy mode
copy.enter
copy.search
copy.select
copy.to_clipboard

// Themes
theme.list
theme.get
theme.set                ← set at session/window/pane scope

// Config
config.get
config.set               ← runtime override
config.validate
config.explain

// Keybindings
keybinding.list
keybinding.set
keybinding.reset

// Plugins
plugin.list
plugin.enable
plugin.disable
plugin.reload            ← hot-reload a specific plugin
plugin.inspect
plugin.install           ← future (v2 marketplace)
plugin.uninstall         ← future

// Events
events.watch             ← streaming subscription
events.history           ← recent events (bounded ring buffer)

// Observability
log.set_level
log.tail
metrics.get
diagnose.run

// Admin
admin.shutdown
admin.gc
```

### 8.3 Request/response format

```json
// Request
{
  "jsonrpc": "2.0",
  "id": "req-abc-123",
  "method": "pane.split",
  "params": {
    "pane_id": "550e8400-...",
    "direction": "vertical",
    "ratio": 0.5,
    "command": ["nvim", "src/main.rs"],
    "client_request_id": "agent-tx-001"
  }
}

// Response
{
  "jsonrpc": "2.0",
  "id": "req-abc-123",
  "result": {
    "pane": {
      "id": "660f9511-...",
      "window_id": "770a0622-...",
      "title": "nvim",
      "cwd": "/home/user/project/src",
      "version": 1
    }
  }
}

// Error
{
  "jsonrpc": "2.0",
  "id": "req-abc-123",
  "error": {
    "code": -32001,
    "message": "version_conflict",
    "data": {
      "resource": "pane",
      "id": "550e8400-...",
      "expected_version": 3,
      "actual_version": 5,
      "hint": "Re-read the pane state and retry with current version"
    }
  }
}
```

### 8.4 Event stream

Events are streamed as JSON-RPC notifications on a held connection. Clients may resume with `from_seq` and must detect gaps (lagged events reported with count).

**`events.watch` request**:
```json
{
  "jsonrpc": "2.0", "id": "watch-1", "method": "events.watch",
  "params": {
    "filters": ["pane.created", "pane.exited", "pane.output"],
    "from_seq": 1040,
    "buffer_size": 1024
  }
}
```

- `filters`: Array of event type prefixes (e.g., `"pane."` matches all pane events). Empty array = all events.
- `from_seq`: Resume from this sequence number. Events between `from_seq` and current are replayed from the ring buffer. If `from_seq` is too old (outside the ring buffer window), the server sends a gap notification: `{"jsonrpc": "2.0", "method": "event.gap", "params": {"from": 1040, "to": 1050, "lost": 10}}`.
- `buffer_size`: Per-client send buffer (default 1024 events). If the client falls behind, oldest events are dropped and a gap notification is sent.

**Event notification format**:
```json
{"jsonrpc": "2.0", "method": "event", "params": {"seq": 1042, "ts": "2026-02-18T10:30:00.123Z", "type": "pane.created", "data": {"pane_id": "...", "window_id": "...", "command": ["bash"]}}}
{"jsonrpc": "2.0", "method": "event", "params": {"seq": 1043, "ts": "2026-02-18T10:30:00.456Z", "type": "pane.focused", "data": {"pane_id": "...", "previous": "..."}}}
```

**`pane.output` binary encoding**: The `data.bytes` field uses base64 encoding for raw PTY output. The `data.sample` boolean indicates whether bytes were dropped/coalesced for that chunk. A false value does not make the whole stream a transcript; byte-exact recording is handled by `pane.record.start` / `pane.record.stop`. Max chunk size: 64 KB per event.

Event types: See Appendix A for complete taxonomy.

### 8.5 Agent-safe patterns

1. **Prefer `pane.run_command` over `pane.send_keys`**: `run_command` executes deterministically (blocking by default, `async=true` for background). `send_keys` is for interactive input.
2. **Use `ensure` operations**: `session.ensure` creates-if-not-exists. Idempotent. Agents can retry safely.
3. **Use `state.apply` for multi-step changes**: Create a window, split it, set themes — all atomically.
4. **Subscribe to events, don't poll**: `events.watch` with filters gives real-time deltas.
5. **Check `version` stamps**: Before mutating, read current version. Include it in mutation request.
6. **Use idempotency keys for retry loops**: `client_request_id` deduplicates operations.

### 8.6 CLI ↔ API mapping

**Invariant: RPC dots become CLI spaces.** Every noun is namespaced
(`session`/`window`/`pane`/`plugin`/`events`/`state`); top-level
shortcut verbs do not exist. Established May 2026 after a codex
dogfood loop established repeated misprediction friction.

```bash
# Sessions
shux session list                            # → session.list (alias: ses ls)
shux session create work                     # → session.create {name: "work", cwd: "$PWD"}
shux session create --ensure work            # → session.ensure {name: "work", cwd: "$PWD"}
shux session kill work                       # → session.kill
shux session attach work                     # → (client-side TUI attach)

# Windows
shux window create -s work -n editor         # → window.create
shux window list -s work                     # → window.list
shux window snapshot -s work                 # → window.snapshot

# Panes
shux pane split -s work -d vertical          # → pane.split
shux pane split -s work -d horizontal -- nvim  # → pane.split {command: ["nvim"]}
shux pane send-keys -s work --text 'j'       # → pane.send_keys
shux pane capture -s work --lines 100        # → pane.capture
shux pane snapshot -s work                   # → pane.snapshot
shux pane wait-for -s work --text 'ready'    # → pane.wait_for

# Events
shux events watch                            # → events.watch (all events, JSON lines)
shux events watch --filter pane.output       # → events.watch (filtered)
shux events history --count 50               # → events.history

# Plugins
shux plugin install ./plugin.py              # → plugin.install (hot reload on)
shux plugin list                             # → plugin.list
shux plugin reload <name>                    # → plugin.reload
shux plugin kill <name>                      # → plugin.kill

# State (atomic batch operations)
shux state apply ./my-project.toml           # → state.apply (from template)

# Raw RPC fallthrough (any registered method)
shux rpc call <method> --params @file        # inline JSON, @file, or - (stdin)

# All non-interactive commands support:
#   --format json|text     (default: text for humans, json for piping)
#   --socket <path>        (override UDS path)
#   --token <token>        (for TCP auth)
```

---

## 9. Keybinding system — graded approach

### 9.1 Tier 1: Bare keys (no prefix, instant)

| Key | Action |
|-----|--------|
| `Alt+h/j/k/l` | Focus pane left/down/up/right |
| `Alt+H/J/K/L` | Resize pane (Shift+Alt+direction) |
| `Alt+n` | Next window |
| `Alt+p` | Previous window |
| `Alt+1..9` | Switch to window by index |
| `Alt+z` | Toggle zoom on current pane |
| `Alt+Enter` | New pane (split in smart direction: splits along the longest edge — if the pane is wider than tall, split vertically; if taller than wide, split horizontally. Tie-breaks to vertical.) |

### 9.2 Tier 2: Prefix (default `Ctrl+Space`)

| Sequence | Action |
|----------|--------|
| `Prefix + c` | New window |
| `Prefix + x` | Close current pane (with confirmation) |
| `Prefix + X` | Close current window (with confirmation) |
| `Prefix + \|` | Split vertical |
| `Prefix + -` | Split horizontal |
| `Prefix + r` | Rename current window |
| `Prefix + R` | Rename current session |
| `Prefix + d` | Detach |
| `Prefix + [` | Enter copy mode |
| `Prefix + :` | Command palette |
| `Prefix + ?` | Keybinding help overlay |
| `Prefix + t` | Set pane theme (interactive picker) |
| `Prefix + .` | Quick command (type a command name) |
| `Prefix + Space` | Toggle last active pane |
| `Prefix + Tab` | Toggle last active window |

### 9.3 Tier 3: Command palette (searchable, discoverable)

Everything is available via `Prefix + :`. Plugin commands appear here automatically.

### 9.4 Customization

All keybindings are remappable in TOML config. Plugin-registered keyboard shortcuts have conflict detection against reserved keys (inspired by PI).

---

## 10. Configuration — TOML, layered, validated

### 10.1 Config discovery (layered precedence)

```text
1. Built-in defaults (compiled into binary)
2. System config: /etc/shux/config.toml (optional, for managed environments)
3. User config: ~/.config/shux/config.toml (XDG_CONFIG_HOME respected)
4. Project config: .shux/config.toml (walking up from CWD)
5. Runtime overrides: set via API (stored in daemon memory)
```

Later layers override earlier layers (per-key merge, not replace).

### 10.2 Full config reference

```toml
# ~/.config/shux/config.toml — shux configuration
# All values shown are defaults.

[daemon]
socket_path = "$XDG_RUNTIME_DIR/shux/shux.sock"
tcp_listen = ""                                    # Empty = disabled
auth_token_path = "~/.config/shux/token"
auto_start = true
auto_exit = true
auto_exit_grace_secs = 5
log_level = "info"
log_format = "pretty"                              # "pretty" or "json"
log_file = ""
grpc_enabled = false

[ui]
prefix = "ctrl+space"
mouse = true
status_bar = true
status_bar_position = "bottom"
scrollback_lines = 5000
pane_border_style = "rounded"                      # "thin", "thick", "double", "rounded", "none"
show_pane_titles = true
auto_title = true

[theme]
name = "default-dark"
paths = ["~/.config/shux/themes"]

[copy]
osc52 = "auto"                                     # "auto", "allow", "deny"
mouse_select_copies = false
vi_keys = true

[plugins]
paths = ["~/.config/shux/plugins"]
allow_process_plugins = false
process_gc_timeout_secs = 30

[shell]
default_command = ""                               # Empty = $SHELL
login_shell = true

# Keybinding overrides. Keys use crossterm notation (e.g., "alt-h", "ctrl-space c").
# Plugin-registered keybindings are validated against reserved keys; conflicts are
# reported at plugin load time and the plugin binding is rejected.
[keybindings]
# "alt-h" = "focus-left"                          # override tier 1
# "ctrl-space c" = "window.create"                # override tier 2
# "ctrl-p" = "command-palette.open"               # custom binding
```

**Config merge semantics**: Later layers override earlier layers using **per-key deep merge**. Scalar values and arrays are replaced entirely (not appended). Nested tables are merged recursively. To delete a key from a parent layer, set it to `false` or an empty value. Walk-up for project config stops at filesystem root or the first directory containing `.git`/`.hg` (whichever is found first). Config files are debounced (100ms) and atomicity is assumed (write to temp file + rename); partial writes are rejected and the previous config is retained.

### 10.3 Session template files

```toml
# ~/.config/shux/templates/web-project.toml

[template]
name = "web-project"
description = "Frontend + Backend + Tests layout"

[session]
name = "{{project_name}}"
cwd = "{{project_dir}}"

[[windows]]
title = "editor"
layout = "single"

[[windows.panes]]
command = ["nvim", "."]

[[windows]]
title = "servers"
layout = "vertical"

[[windows.panes]]
command = ["npm", "run", "dev"]
title = "frontend"

[[windows.panes]]
command = ["cargo", "watch", "-x", "run"]
title = "backend"
theme = "prod"
```

**Template variables**: `{{var_name}}` placeholders use simple Mustache-style substitution (no logic, no loops). Variables are resolved from: (1) CLI `--var key=value` flags, (2) environment variables prefixed with `SHUX_TPL_` (e.g., `SHUX_TPL_PROJECT_NAME`), (3) built-in defaults (`{{cwd}}` = current directory, `{{user}}` = `$USER`). Missing required variables cause `shux apply` to fail with an actionable error listing all unresolved variables.

**Layout values**: The `layout` field in templates maps to named layout algorithms: `single` (one pane), `vertical` (top-bottom split), `horizontal` (left-right split), `even-vertical` (equal vertical splits), `even-horizontal` (equal horizontal splits), `tiled` (balanced grid). These are syntactic sugar for `LayoutNode` trees and are resolved at template application time.

---

## 11. Observability — production-grade for local dev

### 11.1 Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `shux_input_decode_duration_ms` | Histogram | Time to decode input events |
| `shux_render_duration_ms` | Histogram | Time per render cycle |
| `shux_render_diff_cells` | Histogram | Number of cells changed per render |
| `shux_pty_read_bytes_total` | Counter | Total bytes read from PTYs |
| `shux_event_bus_lag_ms` | Gauge | Event processing lag |
| `shux_api_request_duration_ms` | Histogram | API call latency |
| `shux_plugin_call_duration_ms` | Histogram | Plugin function call latency (per plugin) |
| `shux_sessions_total` | Gauge | Active session count |
| `shux_panes_total` | Gauge | Active pane count |

### 11.2 Doctor bundle

`shux doctor` produces a JSON file containing: version, config, capabilities, plugin status, recent errors, metrics, terminal environment. `--redact strict` removes paths and env values.

---

## 12. Terminal compatibility

### 12.1 Capability detection strategy

At client attach time, build a `ClientCaps` struct:

1. **Fast heuristics**: Check `TERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, `COLORTERM` environment variables.
2. **Escape sequence probes** (with 200ms timeout): DA2 query, XTVERSION, Kitty keyboard query (`CSI ? u`), DECRQM for Mode 2026 (synchronized output).
3. **Cache per-client**: Different attached clients may have different capabilities.

### 12.2 Enhanced features (opportunistic)

| Feature | Detection | Fallback |
|---------|-----------|----------|
| Truecolor (24-bit) | `$COLORTERM=truecolor` | 256-color approximation |
| Kitty keyboard protocol | `CSI ? u` response | Legacy input decoding |
| OSC 52 clipboard | Config + terminal detection | External clipboard tool |
| Synchronized output | DECRQM Mode 2026 | No synchronization |
| Image passthrough | Terminal identification | DCS passthrough for focused pane |

---

## 13. Security

### 13.1 Mitigations

| Threat | Mitigation |
|--------|-----------|
| Cross-user socket access | UDS with `0700` permissions |
| Malicious TCP access | Token-based auth; TCP off by default |
| Plugin RPC over-reach (v0.19+) | Default-deny permission model on every plugin RPC frame: per-method sensitivity tiers (`Public`/`ContentRead`/`OwnedMutation`/`Grantable`/`PluginsForbidden`), ownership-based auto-grant for entities the plugin created, per-install UUID identity (so name re-use doesn't inherit grants), CLI grant/revoke (`shux plugin grant ...`). NDJSON audit log per plugin. See `docs/designs/permissions/README.md`. |
| Plugin filesystem access | WASI sandbox; `ResourceLimiter`; declared permissions (v1+ WASM milestone — process plugins ship with permission model above first) |
| Plugin CPU abuse | Epoch interruption (~10% overhead); 100ms kill threshold |
| Plugin memory abuse | `ResourceLimiter` trait (e.g., 16 MB cap per plugin) |
| Plugin network access | Disabled by default; requires explicit grant |
| Process plugin isolation | Disabled by default; platform-dependent when enabled |
| Plugin subprocess escape (`exec`) | Scrubbed environment (allowlist: PATH, HOME, TERM, LANG), CWD restriction, 30s timeout. On Linux: rlimits applied (RLIMIT_NPROC, RLIMIT_FSIZE). Does not use shell — invokes command directly (no shell injection). |
| Oversized payloads | 16 MB max frame size on all transports; reject and close on violation |
| Plugin API extension squatting | Registered API methods must use `<plugin-id>.` prefix (e.g., `com.example.my-plugin.my-method`). Built-in method names cannot be overridden via `register-api-method`; use `register-command-override` with explicit permission instead. |
| Grant-file TOCTOU / symlink swap | Atomic temp+rename writes; symlink target rejected on read and write. |

---

## 14. Non-functional requirements

### 14.1 Performance budgets (P0)

| Metric | Target | Hard limit |
|--------|--------|-----------|
| Keypress → visible update | p50 ≤ 8ms, p99 ≤ 25ms | p99 ≤ 50ms |
| Split pane operation | p50 ≤ 25ms, p99 ≤ 80ms | p99 ≤ 150ms |
| Attach (< 10 panes) | ≤ 150ms | ≤ 300ms |
| High-output throughput | ≥ 10K lines/s across 4 panes without UI lockup | |
| Daemon idle memory (RSS, 10 panes, 5K scrollback) | ≤ 80 MB (goal) | ≤ 150 MB |
| Plugin call overhead (warm path) | p99 ≤ 5ms per call | Kill plugin at 100ms |
| Wasm instantiation (cold, pre-compiled `.cwasm`) | p50 ≤ 50μs, p99 ≤ 200μs | ≤ 1ms |
| Wasm function call (warm) | p99 ≤ 100μs | ≤ 1ms |

### 14.2 Reliability

- **Crash-safe design**: Startup IS recovery — the daemon must function correctly after an unclean exit with no special recovery mode. However, the daemon also supports **best-effort graceful shutdown** (`SIGTERM` → `CancellationToken` → subsystem drain → exit) to cleanly notify plugins and flush logs. The graceful path is an optimization, not a correctness requirement.
- **Session-persist plugin (v1.x)**: Serializes session graph every 1 second as a TOML layout file. On restart, offers to restore (Zellij pattern: "Press ENTER to run..." for safety).
- **Plugin isolation**: Crashing plugins are disabled and reported, never stalling the daemon.
- **Client reaping**: Misbehaving clients (slow readers, abandoned connections) are reaped with timeouts.

---

## 15. Technology choices (validated)

### 15.1 Language & toolchain

- **Rust stable** (pinned via `rust-toolchain.toml`)
- Latest stable edition
- Clippy on CI with deny warnings

### 15.2 Key crate families (with validated versions)

| Domain | Crate | Version | Notes |
|--------|-------|---------|-------|
| Async runtime | `tokio` | 1.x | Standard choice |
| JSON-RPC | Hand-rolled + `json-rpc-types` | - | Over `tokio::net::UnixListener` + `tokio_util::codec::LengthDelimitedCodec`. jsonrpsee lacks UDS support. |
| gRPC | `tonic` + `prost` | 0.x | Optional transport. UDS via custom connector (`serve_with_incoming`). |
| CLI | `clap` (derive) | 4.x | Subcommands, completions |
| Client terminal I/O | `crossterm` | 0.29 | Kitty keyboard, synchronized output, OSC 52 clipboard. No OSC 8 (raw emission). |
| VT parsing (PTY output) | `vte` | 0.15 | With `ansi` feature for typed handler callbacks. Maintained by Alacritty project. |
| Virtual terminal grid | Custom | - | VecDeque-based (Alacritty pattern). NOT `alacritty_terminal` (too coupled). NOT `ratatui` (wrong abstraction for multiplexer core). |
| UI chrome | Optionally `ratatui` | 0.30 | For status bar and borders only. ratatui's diff-based `Buffer` is useful as a utility, not its `Terminal` abstraction. |
| Wasm host | `wasmtime` | 41+ | Component Model, WASI Preview 2, epoch interruption, `ResourceLimiter`. |
| Config | `toml` + `serde` | - | With schema validation |
| Tracing | `tracing` + `tracing-subscriber` | - | Structured observability |
| PTY | `pty-process` | 0.5.3 | AsyncRead/AsyncWrite, tokio integration, resize support. Alternative: custom with `nix`/`rustix`. |
| State snapshots | `arc-swap` | - | Lock-free reads, atomic updates |
| Event bus | `tokio::sync::broadcast` | - | Custom wrapper with sequence numbers and gap detection |
| Graceful shutdown | `tokio-util` | - | `CancellationToken`, `TaskTracker` |
| Daemonization | `daemonize` or manual `nix` | - | Fork BEFORE tokio runtime init |

### 15.3 Build

- `cargo build --release` produces a single binary artifact
- Cross-compilation for macOS (aarch64 + x86_64) and Linux (glibc + musl)
- GitHub Actions CI with test matrix

---

## 16. Testing strategy — prove it works

### 16.1 The testing pyramid

```text
     ┌─────────────────────────────┐
     │  Layer 6: Dogfood Tests     │  shux testing itself
     │  (M2+)                      │  inside its own panes
     ├─────────────────────────────┤
     │  Layer 5: Agent Scenarios   │  Scripts drive shux API
     │  (M2+)                      │  end-to-end
     ├─────────────────────────────┤
     │  Layer 4: Visual Regression │  iTerm2 driver screenshots
     │  (M1+)                      │  golden image comparison
     ├─────────────────────────────┤
     │  Layer 3: API Contract      │  Start daemon, exercise
     │  (M1+)                      │  every API endpoint
     ├─────────────────────────────┤
     │  Layer 2: PTY Integration   │  Spawn real PTYs, send
     │  (M0+)                      │  input, verify output
     ├─────────────────────────────┤
     │  Layer 1: Headless Unit     │  ratatui TestBackend,
     │  (M0+)                      │  property tests, fuzzing
     └─────────────────────────────┘
```

### 16.2 Layer details

**L1 (Headless unit)**: SessionGraph CRUD, LayoutEngine property tests, input decoder, config parser, theme resolver, ANSI parser (fuzz extensively), JSON-RPC serialization. Every commit, <30s.

**L2 (PTY integration)**: Real PTY spawn/read/write, command execution, scrollback, resize, UTF-8, high-throughput stress (10K lines/s). Every PR, 1-2 min.

**L3 (API contract)**: Real daemon (ephemeral socket), every API method happy path + errors, idempotent ops, version conflicts, event stream (including resume/gap detection), batch ops, auth (including gRPC TCP token auth parity), plugin load/unload, permission-denied matrix (every permission tested with grant and deny), process-plugin protocol parity with WIT, frame-size limit rejection, and interceptor timeout/fail-closed behavior. Every PR, 2-5 min.

**L4 (Visual regression)**: iTerm2 driver, keystrokes, screenshots, golden image comparison. Initial launch, splits, per-pane theming, zoom, command palette, copy mode. Pre-release, 5-10 min.

**L5 (Agent scenarios)**: Python scripts as AI agent driving shux API end-to-end. Setup workspace, monitor processes, batch operations. Every PR, 2-3 min.

**L6 (Dogfood)**: Run `cargo test` inside shux panes. Verify shux remains responsive. M2+.

### 16.3 Fuzzing

- ANSI parser: `cargo-fuzz` with arbitrary bytes. Smoke on PRs, long campaigns nightly.
- JSON-RPC parser: Fuzz request deserialization.
- Config parser: Fuzz TOML parsing.
- Layout engine: Fuzz split/resize/swap sequences, verify invariants.

---

## 17. Milestone plan — ~12-16 weeks

### M0: Architecture spike (weeks 1-3)

**Goal**: Prove the core architecture works end-to-end.

**Deliverables**:
- Daemon skeleton with tokio event loop (fork-then-runtime pattern)
- PTY manager: spawn process via `pty-process`, async read/write
- Virtual terminal: `vte` parser + custom VecDeque grid
- Minimal TUI client: single pane, renders output via crossterm, handles input
- JSON-RPC server on UDS (hand-rolled, length-prefixed): `system.version`, `system.health`, `session.list`
- Basic input decoder (legacy mode, Kitty keyboard when available)
- `shux` binary with `new`, `attach`, `ls` subcommands

**Test layers**: L1, L2

**Done when**: `shux session create test` → starts daemon, creates session, attaches TUI. Typing works. Detach/reattach works. `shux --format json rpc call system.version` works.

### M1: Daily-driver core (weeks 4-7)

**Goal**: Usable as a basic daily-driver multiplexer.

**Deliverables**:
- Sessions, windows, panes: full CRUD via CLI + API
- Splits (H/V), directional focus, resize (keyboard + mouse), zoom, swap
- Copy mode: scroll, search, select, clipboard (OSC 52)
- Graded keybindings (Tier 1 + Tier 2), command palette, help overlay
- TOML config with live reload and validation
- Mouse support, pane titles, beautiful defaults (dark theme)
- Basic hardcoded status bar (pre-plugin)
- Synchronized output (Mode 2026)

**Test layers**: L1, L2, L3, L4 (first golden images)

**Done when**: A developer can use shux for a full day. All keybindings work. Visual regression tests pass.

**Dogfooding begins**.

### M2: API completeness + plugin system (weeks 8-10)

**Goal**: Full API surface. Plugin system working with bundled plugins.

**Deliverables**:
- Complete JSON-RPC API (all methods from section 8.2)
- Event stream with filters and sequence numbers
- Idempotent `ensure` operations, batch `state.apply`, optimistic concurrency
- Plugin host: Wasm (wasmtime 41+, Component Model, WIT) + process plugins
- Plugin lifecycle, permissions, hot reload, GC
- Event interception and command override support
- **shux-status-bar** replaces hardcoded status bar
- **shux-theme-pack** with 5 themes
- **shux-diagnostics** (doctor + overlay)
- Per-pane theming, session templates
- gRPC API (optional)

**Test layers**: L1, L2, L3, L4, L5, L6

**Done when**: Every API method has a contract test. Agent scenarios pass. Bundled plugins work. Plugin hot reload works. `shux doctor` works.

### M3: Polish, performance, docs (weeks 11-12+)

**Goal**: Release-quality v1.0.

**Deliverables**:
- Performance optimization against budgets
- Shell completions (bash, zsh, fish)
- README, getting started guide, plugin authoring guide
- ANSI parser fuzzing campaign
- Binary releases for macOS + Linux
- Homebrew formula, cargo install

**Done when**: All performance budgets met. Zero known crashers. Plugin authoring guide includes working examples.

---

## 18. Success metrics

| Metric | Target | How measured |
|--------|--------|-------------|
| Daily-driver adoption | Author uses shux exclusively for ≥ 2 weeks | Self-report |
| Performance | All p99 budgets met | Benchmark suite |
| API coverage | 100% of operations testable via CLI | API contract tests |
| Plugin API sufficiency | All 3 bundled plugins use only public plugin API | Code review |
| Agent story | All 3 agent scenario tests pass | CI |
| Visual quality | Zero visual regressions vs golden images | CI (macOS) |
| Crash resilience | Zero crashes in 1-week dogfood period | Doctor + crash reports |
| Plugin DX | Working "hello world" plugin in < 15 minutes | User testing |

---

## 19. Future roadmap (post v1.0)

| Priority | Feature | Implementation |
|----------|---------|----------------|
| P1 | Floating panes | First-party plugin (overlay layer) |
| P1 | Session persistence | First-party plugin (Zellij-style layout serialization) |
| P1 | MCP server | First-party plugin (expose shux as MCP tools for Claude Code et al) |
| P1 | Session replay | First-party plugin (event recording + rewind UX) |
| P1 | Python + Node SDKs | Generated from API schema |
| P1 | URL picker | Plugin |
| P1 | Notification on exit | Plugin |
| P2 | Scripting layer | Embed Lua/Rhai/Starlark (architecture prepared from v1) |
| P2 | Multiplayer | Multiple clients, read-only viewers (Zellij has this) |
| P2 | Web client | Browser-based sessions (Zellij has this) |
| P2 | Plugin marketplace | Registry protocol + install (plugin.toml metadata ready) |
| P2 | Windows native | ConPTY + named pipes |
| P2 | Stacked panes | Zellij-style tabbed panes within splits |
| P2 | Self-modification | Agents can write plugin code, trigger hot-reload (PI pattern) |

---

## 20. Supported platforms & terminals

### 20.1 OS support

| OS | v1.0 | Notes |
|----|------|-------|
| macOS 13+ (aarch64, x86_64) | ✓ | Primary development platform |
| Linux (glibc, x86_64/aarch64) | ✓ | |
| Linux (musl, x86_64/aarch64) | ✓ | Alpine, static builds |
| Windows | Designed-for, not implemented | Architecture allows ConPTY later |

### 20.2 Terminal compatibility matrix

| Terminal | Stars | Priority | Enhanced features |
|----------|-------|----------|-------------------|
| Ghostty | 44k | Primary | Truecolor, modern keyboard, synchronized output |
| iTerm2 | - | Primary | Truecolor, OSC 52, inline images, AppleScript (testing) |
| WezTerm | 24.3k | Primary | Truecolor, OSC 8 hyperlinks |
| Kitty | - | Primary | Kitty keyboard protocol, graphics protocol |
| Warp | - | Primary | Truecolor (test compatibility with Warp's own multiplexing) |
| Alacritty | - | Supported | Truecolor, basic terminfo |
| Terminal.app | - | Supported | 256-color, basic capabilities |

---

## 21. Appendix A — Event taxonomy (complete)

```text
# Session lifecycle
session.created        {session_id, name}
session.renamed        {session_id, old_name, new_name}
session.killed         {session_id, name}
session.attached       {session_id, client_id}
session.detached       {session_id, client_id}

# Window lifecycle
window.created         {window_id, session_id, title}
window.activated       {window_id, session_id, previous_window_id}
window.renamed         {window_id, old_title, new_title}
window.reordered       {window_id, session_id, old_index, new_index}
window.killed          {window_id, session_id}

# Pane lifecycle
pane.created           {pane_id, window_id, command}
pane.focused           {pane_id, window_id, previous_pane_id}
pane.resized           {pane_id, cols, rows}
pane.zoomed            {pane_id, zoomed: bool}
pane.title_changed     {pane_id, old_title, new_title}
pane.cwd_changed       {pane_id, old_cwd, new_cwd}
pane.exited            {pane_id, exit_status, command}
pane.respawned         {pane_id, command}
pane.command_completed {pane_id, command_id, exit_code, stdout, stderr}  # fired when pane.run_command(async=true) finishes
pane.output            {pane_id, bytes, sample}  # opt-in sampled live observation; use pane.record.* for transcripts
pane.input             {pane_id, data}           # interceptable; fired on line-submit (Enter), not per-keystroke
pane.bell              {pane_id}                 # fired when pane receives BEL character (\x07)
pane.tag_changed       {pane_id, key, old_value, new_value}

# Client
client.connected       {client_id, terminal_size, capabilities}
client.disconnected    {client_id, reason}
client.resized         {client_id, old_width, old_height, new_width, new_height}

# Theme
theme.changed          {scope, scope_id, old_theme, new_theme}

# Config
config.reloaded        {source, changes: [{key, old, new}]}

# Plugin
plugin.enabled         {plugin_id, version}
plugin.disabled        {plugin_id, reason}
plugin.reloaded        {plugin_id, version}
plugin.error           {plugin_id, error, context}

# Inter-plugin (namespaced — shipped in v0.16+)
# Plugins publish via `event.publish`; daemon brands them
# `plugin.<plugin_id>.<event_type>` so subscribers can filter by
# plugin identity. The `plugin_id` is captured server-side from the
# calling plugin's manifest name and cannot be spoofed via params.
plugin.event           {plugin_id, event_type, data}  # e.g., "git.branch_changed"

# Keybinding
keybinding.changed     {key, old_action, new_action}

# System
error                  {code, message, context}
```

---

## 22. Appendix B — Glossary

| Term | Definition |
|------|-----------|
| **Daemon** | The long-running shux server process that owns all state and PTYs |
| **Client** | Any process connecting to the daemon (TUI, CLI, SDK, agent) |
| **Session** | A named workspace containing one or more windows |
| **Window** | A tab-like container within a session, containing a layout of panes |
| **Pane** | A terminal viewport connected to a PTY process |
| **Layout tree** | Binary split tree defining how panes are arranged in a window |
| **Overlay** | Plugin-provided UI layer rendered above the layout (floating panes, popups) |
| **Theme** | A named set of color tokens applied at session/window/pane scope |
| **Client caps** | Negotiated terminal capabilities for a specific attached client |
| **Event** | A typed, sequenced, timestamped notification emitted on the event bus |
| **Plugin** | An extension loaded by the plugin host (Wasm or process) |
| **Virtual terminal** | Per-pane terminal emulation grid (VecDeque + vte parser) |
| **Render compositor** | System that composes virtual terminals + chrome into per-client output |

---

## 23. Appendix C — References (verified February 2026)

### Core tools & dependencies
- Wasmtime releases: https://github.com/bytecodealliance/wasmtime/releases (v41.0.3)
- WIT specification: https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md
- Component Model docs: https://component-model.bytecodealliance.org/design/wit.html
- WASI roadmap: https://wasi.dev/roadmap
- crossterm: https://github.com/crossterm-rs/crossterm (v0.29)
- ratatui: https://ratatui.rs/ (v0.30)
- vte: https://github.com/alacritty/vte (v0.15)
- pty-process: https://lib.rs/crates/pty-process (v0.5.3)
- tokio: https://tokio.rs/
- json-rpc-types: https://crates.io/crates/json-rpc-types
- arc-swap: https://docs.rs/arc-swap/
- tonic: https://github.com/hyperium/tonic (UDS example in repo)
- jsonrpsee: https://github.com/paritytech/jsonrpsee (v0.26, no native UDS)

### Competitors
- tmux: https://github.com/tmux/tmux (3.6a)
- Zellij: https://github.com/zellij-org/zellij (0.43.1)
- TUIOS: https://github.com/Gaurav-Gosain/tuios (0.6.0, 2.4k stars)
- cy: https://github.com/cfoust/cy (1.11.1)
- Ghostty: https://github.com/ghostty-org/ghostty (44k stars)
- WezTerm: https://github.com/wezterm/wezterm (last stable Feb 2024)

### Agent orchestration ecosystem
- Agent Deck: https://github.com/asheshgoplani/agent-deck (901 stars)
- Agent of Empires: https://github.com/njbrake/agent-of-empires (743 stars)
- Agentboard: https://github.com/gbasin/agentboard (301 stars)
- AWS CLI Agent Orchestrator: https://github.com/awslabs/cli-agent-orchestrator (243 stars)
- NTM: https://github.com/Dicklesworthstone/ntm (148 stars)
- Claude Code Agent Teams: https://code.claude.com/docs/en/agent-teams

### Inspiration
- PI (badlogic): https://github.com/badlogic/pi-mono (13.5k stars) — extension architecture, hot reload, tool override, event interception
- Nushell plugin protocol: https://www.nushell.sh/contributor-book/plugin_protocol_reference.html
- LSP specification: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/
- Neovim RPC: https://neovim.io/doc/user/api.html

### Architecture references
- Zellij architecture (DeepWiki): https://deepwiki.com/zellij-org/zellij/2.1-client-server-model
- Zellij session resurrection: https://zellij.dev/documentation/session-resurrection.html
- Zellij performance blog: https://poor.dev/blog/performance/
- Terminal multiplexer in Rust (blog): https://implaustin.hashnode.dev/how-to-write-a-terminal-multiplexer-with-rust-async-and-actors-part-2
- Anatomy of a terminal emulator: https://poor.dev/blog/terminal-anatomy/
- Terminal latency analysis: https://danluu.com/term-latency/
- Alacritty scrollback: https://jwilm.io/blog/alacritty-lands-scrollback/
- Kitty keyboard protocol: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
- Kitty graphics protocol: https://sw.kovidgoyal.net/kitty/graphics-protocol/
- iterm2-driver skill: https://github.com/indrasvat/claude-code-skills/blob/main/skills/iterm2-driver/SKILL.md

---

## 24. Appendix D — Validated Corrections From v3

1. **JSON-RPC transport resolved**: jsonrpsee lacks native UDS (confirmed); hand-rolled JSON-RPC over `tokio::net::UnixListener` + `LengthDelimitedCodec` is the validated approach, matching Zellij's pattern.
2. **WIT snippet validated**: The v3 WIT parses correctly. `result<_, error-type>` is valid WIT syntax. Added more host functions (`get-pane`, `list-panes`, `get-config`, `set-badge`, `set-pane-tag`, `emit-event`, `read-file`) and plugin functions (`intercept-event`, `render-overlay`).
3. **Wasmtime version updated**: v41.0.3 (Feb 2026). WASI Preview 2 stable. Epoch interruption and `ResourceLimiter` confirmed production-ready.
4. **Competitor landscape updated**: tmux 3.6a (confirmed), Zellij 0.43.1 (web client shipped, switching to wasmi), TUIOS 0.6.0 (2.4k stars — more serious than v3 implied), cy 1.11.1 (daily commits).
5. **Agent orchestration section added**: Documented the explosion of tmux-wrapping tools (Agent Deck, Agent of Empires, Agentboard, etc.) — all of which exist because tmux lacks a typed API.
6. **PI patterns incorporated**: Registration-activation separation, tool override by name, event interception/blocking, inter-plugin event bus, plugin GC — all inspired by PI's extension architecture.
7. **Process plugin protocol enriched**: Added flow control (Ack/Drop), cancellation, plugin GC, and the notification vs request distinction (from JSON-RPC/nushell/LSP).
8. **Crate versions validated**: crossterm 0.29 (Kitty keyboard, sync output, OSC 52), vte 0.15, ratatui 0.30 (workspace reorganization), pty-process 0.5.3.
9. **Architecture clarified**: ArcSwap for lock-free state reads, broadcast channel for events, single-writer/many-readers pattern, fork-before-tokio for daemonization.
10. **Rendering architecture clarified**: `vte` + custom VecDeque grid for per-pane virtual terminals. `ratatui` optional for chrome only. NOT `alacritty_terminal` (too coupled). New `RenderCompositor` abstraction for per-client output.
11. **Memory budget refined**: Default scrollback reduced to 5,000 lines (from 10,000) to meet 60 MB idle target. Compact cell representation.
12. **Plugin DX section added**: Scaffold, dev mode, testing, inspect, logs.
13. **User preferences incorporated**: No tmux migration tooling, JSON-only process plugin protocol, scripting layer designed-for v2+, registry metadata fields in plugin.toml for v2 marketplace.

## 25. Appendix E — Corrections From v4 Review

1. **Permission naming unified**: `run_subprocess` renamed to `exec` everywhere (manifest, WIT docs, tier table, security table) to match the WIT host function name. Subprocess sandboxing details added (env scrubbing, CWD restriction, rlimits).
2. **Interception vs "cannot block core" clarified**: Invariant #3 now specifies per-interceptor deadline (100ms kill), fail-closed behavior on interceptor failure, and marks exhaustive plugin streams as future design separate from current sampled pane output.
3. **Daemon pinning added**: Long-lived plugins with `gc = false` hold daemon leases, preventing auto-exit while service plugins (MCP, SSH Tunnels) are active.
4. **`state.apply` delta schema specified**: Operation types, positional back-references (`$N.field`), all-or-nothing rollback, `client_request_id` deduplication.
5. **`events.watch` contract specified**: Filter grammar, `from_seq` resume, gap notifications, buffer policy, reconnect semantics.
6. **`pane.output` binary encoding specified**: Base64 encoding, 64 KB max chunk, chunk-local sample flag. Byte-exact transcripts use `pane.record.*`.
7. **`pane.run_command` async lifecycle specified**: `command_id`, `pane.command_status/cancel`, completion event, timeout.
8. **`stream_id` added to flow control events**: Process plugin notifications now carry `stream_id` for ACK/DROP linkage.
9. **gRPC TCP auth required**: Same token auth as JSON-RPC TCP, documented in transport table.
10. **Max frame size added**: 16 MB limit on all length-prefixed transports.
11. **Plugin API namespace rule**: Registered API methods must use plugin-scoped names; collisions with built-in methods rejected.
12. **Crash-safe vs crash-only clarified**: Primary model is crash-safe (startup IS recovery); graceful shutdown is best-effort optimization.
13. **Performance budgets refined**: Wasm instantiation target adjusted to realistic p50/p99 ranges; memory budget adjusted for allocator overhead.
14. **Testing strategy expanded**: Added explicit test requirements for stream resume/gaps, permission-denied matrix, process-plugin protocol parity, frame-limit rejection, interceptor fail-closed behavior.
15. **Smart direction defined**: `Alt+Enter` splits along longest edge (wider→vertical, taller→horizontal).
16. **Keybinding config schema added**: `[keybindings]` section with crossterm notation, reserved key validation, plugin conflict detection.
17. **Config merge semantics specified**: Per-key deep merge, walk-up stops at VCS root, debounce, atomic write requirement.
18. **Template engine specified**: Mustache-style `{{var}}` substitution, resolution order (CLI → env → defaults), layout value mapping.
19. **Interception scope clarified**: v1.0 scopes interception to events in `intercept_events` permission; API-level operation interception is v2.
20. **API version format unified**: `shux.plugin.v1` → `shux:plugin@1.0.0` in manifest, matching WIT package declaration.
