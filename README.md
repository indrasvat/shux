<p align="center">
  <img src="assets/logo.svg" alt="shux" width="400">
</p>

<p align="center">
  <strong>The terminal multiplexer built for AI coding agents</strong><br>
  <em>Typed API. Deterministic state. Zero wrappers needed.</em><br>
  <img src="https://img.shields.io/badge/lang-Rust-DEA584?style=flat&labelColor=1a1a1a" alt="Rust">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat&labelColor=1a1a1a" alt="MIT">
</p>

<p align="center">
  <a href="#why-shux">Why shux</a> •
  <a href="#features">Features</a> •
  <a href="#installation">Installation</a> •
  <a href="#usage">Usage</a> •
  <a href="#architecture">Architecture</a> •
  <a href="docs/PRD.md">PRD</a> •
  <a href="#roadmap">Roadmap</a>
</p>

---

## Why shux

Every AI agent orchestration tool — [Agent Deck](https://github.com/nicholasgasior/agent-deck), [Agent of Empires](https://github.com/smalik/agent-of-empires), [Agentboard](https://github.com/agentboard/agentboard), [NTM](https://github.com/atinylittleshell/ntm), Claude Code's Agent Teams — wraps tmux. They do this because no multiplexer provides a first-class typed API for programmatic control.

shux is what tmux would be if designed today, in the age of Claude Code, Codex CLI, and Gemini CLI. Humans and agents are equal citizens: every operation is available through both keyboard and JSON-RPC API. State is always queryable, always deterministic, always streamable.

The core is deliberately small. Everything else — status bar, themes, floating panes, session persistence, MCP integration — is a plugin.

## Features

### Agent-first

- **JSON-RPC API** — Every CLI command is a thin API call over Unix domain sockets; no string parsing, no screen scraping
- **Idempotent `ensure` operations** — Create-if-missing semantics for sessions, windows, and panes; safe for retries and concurrent agents
- **Deterministic state snapshots** — Query the full session graph at any point; always consistent, never stale
- **Event streaming** — Subscribe to typed, sequenced events; react to pane output, session changes, command completion
- **MCP-ready architecture** — Designed to be exposed as MCP tools for AI agents (plugin)

### For humans too

- **Zero-config beauty** — Polished defaults, smart terminal detection, no `.shux.toml` archaeology needed
- **Graded keybindings** — Common actions on bare keys, less common behind a prefix, all discoverable via command palette
- **Per-pane theming** — Color-code prod vs dev, highlight agent panes, theme by project
- **Plugin system** — WASI Preview 2 (Wasmtime) + process plugins, hot reload, capability-based permissions
- **Observability** — Structured tracing, diagnostics, doctor bundle

## Installation

### From Source

```bash
git clone https://github.com/indrasvat/shux.git
cd shux
make build
```

Optimized binary:

```bash
make release      # → target/release/shux
make install      # → ~/.local/bin/shux
```

### Requirements

- Rust 1.93+ (stable)
- A Unix-like OS (macOS, Linux)

### Dev Setup

```bash
make install-tools   # Install nextest, llvm-cov, deny, fuzz, lefthook
make hooks           # Install git hooks
make check           # lint + test (what pre-commit runs)
```

## Usage

```bash
shux                       # Attach to last session, or create "default"
shux new -s work           # Create session named "work"
shux ls                    # List sessions
shux attach -s work        # Attach to "work"
shux pane split -s work    # Split the active pane
```

The daemon auto-starts on first use and auto-exits when the last session is destroyed.

### CLI == API

Every subcommand is a thin JSON-RPC call. Agents and scripts get the same capabilities as the keyboard:

```bash
# Sessions
shux ls                                  # List sessions (alias: list)
shux new -s dev                          # Create session
shux new -s dev --ensure                 # Idempotent — create only if missing
shux kill -s dev                         # Kill session
shux rename -s dev -n staging            # Rename session

# Windows
shux window list -s dev
shux window new -s dev -n logs
shux window focus -s dev -w logs

# Panes
shux pane list -s dev
shux pane split -s dev -d vertical
shux pane zoom -s dev                    # Toggle zoom on active pane

# Raw API access (for debugging)
shux api system.version
shux api session.list '{}'
```

### Exit Codes

| Code | Meaning |
|------|---------|
| `0`  | Success |
| `1`  | Error   |

## Architecture

```text
crates/shux/           CLI entrypoint (clap, daemon auto-start)
    ↓
crates/shux-core/      Core engine (SessionGraph, LayoutEngine, EventBus, config, theme)
    ↓
crates/shux-pty/       PTY manager (pty-process, async I/O, lifecycle)
crates/shux-vt/        Virtual terminal grid (vte parser, VecDeque grid, scrollback)
crates/shux-rpc/       JSON-RPC server (UDS + TCP, length-prefixed framing)
crates/shux-plugin/    Plugin host (wasmtime, WIT, process plugins, permissions)
crates/shux-ui/        TUI client (crossterm, ratatui for chrome, render compositor)
```

**Key patterns:**

- **Single writer, many readers** — Mutations via mpsc → single state-owner task; reads via ArcSwap snapshots
- **Events as integration surface** — Typed, sequenced, via `tokio::sync::broadcast`
- **Plugin sandbox** — WASI Preview 2 + Component Model; per-plugin Store; epoch interruption; hot reload via Store drop/recreate
- **Version handshake** — CLI detects daemon version mismatch, restarts automatically after rebuilds

> **[Full PRD](docs/PRD.md)** — Design philosophy, competitive analysis, detailed architecture, plugin WIT interfaces, and milestone plan.

## Make Targets

```bash
make build              # Build all crates (debug)
make release            # Build optimized binary
make test               # Run tests with cargo-nextest
make lint               # clippy + fmt-check
make check              # lint + test
make ci                 # CI pipeline (lint + test-lib + test-doc)
make bench              # Run benchmarks
make doc                # Build documentation
make deny               # License/advisory audit
```

## Roadmap

Potential future enhancements:

- **Session persistence** — Save/restore layouts and running commands across restarts
- **Floating panes** — Overlay panes for transient tasks
- **Session replay** — Time-travel through terminal history
- **MCP integration** — Expose shux operations as MCP tools for AI agents
- **Status bar plugins** — Git status, system metrics, custom segments
- **Web client** — Attach to sessions from a browser
- **gRPC streaming** — Typed event subscriptions for high-throughput integrations
- **Plugin marketplace** — Discover and install community plugins

---

## License

MIT
