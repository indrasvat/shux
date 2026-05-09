# Architecture

Cargo workspace, 7 crates, ~30K lines (Rust edition 2024, stable 1.93+).

```
crates/shux/           CLI entrypoint (clap, daemon auto-start, attach client wiring)
    ↓
crates/shux-core/      Core engine — SessionGraph, LayoutEngine, EventBus, config
    ↓
crates/shux-pty/       PTY manager (pty-process, async I/O, lifecycle, command engine)
crates/shux-vt/        Virtual terminal grid (vte parser, VecDeque grid, scrollback)
crates/shux-rpc/       JSON-RPC server (UDS + TCP, length-prefixed, attach protocol)
crates/shux-plugin/    Plugin host stub (wasmtime, WIT — fleshed out in M2)
crates/shux-ui/        TUI client (crossterm, render compositor, attach loop)
```

## Single binary, daemon model

`shux` is one binary. The first time you run it, it forks a daemon
(double-fork, then `setsid`) and connects to its UDS socket. Subsequent
invocations talk to that same daemon. Sessions persist across attach/detach.

When the last session is destroyed, the daemon auto-exits.

```
$ shux ls
   ↓ tries to connect
$ shux ls    [daemon: not running]
   ↓ spawns __daemon (double-fork)
$ shux ls    [daemon: running] ← talks JSON-RPC
```

The daemon binds two UDS sockets:

- `shux.sock` — JSON-RPC. Request-response. Every `shux <subcmd>` lands here.
- `attach.sock` — Streaming. The TUI attach client lives here.

## Single writer, many readers

All state mutations go through one mpsc channel into a `SessionGraph`
state-owner task. Every reader gets a lock-free `ArcSwap<SessionGraphSnapshot>`
to load. No locks on the read path.

```rust
let graph = GraphHandle { tx: cmd_tx, snapshot: arc_swap.clone() };

// Read (lock-free):
let snap = graph.snapshot();
for (id, sess) in &snap.sessions { ... }

// Write (single-writer):
graph.create_session("dev", cwd).await?;
```

This keeps render-tick latency low (no contention) while still being safe
for concurrent agents and attach clients.

## CLI == API

Every `shux` subcommand is a thin JSON-RPC call:

```rust
// pseudocode of shux ls
let stream = connect_or_spawn_daemon().await?;
let result = rpc(&mut stream, "session.list", json!({})).await?;
print_pretty(result);
```

The CLI never reads daemon state directly. Agents talk to the same RPC
surface the keyboard does. This is what gives shux its "no wrappers"
property.

## Daemon-renders-everything attach

`shux attach` opens `attach.sock` and goes raw mode. From there it's a
thin pipe:

- **Daemon → client**: a per-attach `RenderCompositor` walks all VTs in
  the active window, composes a frame, ships ANSI bytes wrapped in a
  Render frame at ~30fps.
- **Client → daemon**: keystrokes encoded by crossterm, mouse events,
  resize notifications.

This is the same architecture tmux uses. Multiple clients can attach
independently; they all see the same session state.

## Events as integration surface

`tokio::sync::broadcast` + atomic sequence numbers. Every state change
emits a typed event:

```rust
enum Event {
    SessionCreated { id, name, .. },
    PaneOutput { pane, bytes, seq },
    PaneExited { pane, exit_code },
    WindowFocused { window },
    ConfigChanged { generation },
    // ...
}
```

The M2 plugin host will expose this via `events.watch` so plugins can react
to the world without polling.

## TOML config + hot reload

Config lives at `$XDG_CONFIG_HOME/shux/config.toml`. The daemon watches
the parent directory via `notify`, debounces, re-parses, and atomically
swaps the live `Arc<Config>`. The render loop awaits a `tokio::sync::Notify`
to redraw on changes.

Why parent dir? Because editors atomic-rename — file-only watches miss the
real "save."

## fork-before-tokio daemonization

Daemonization (double-fork + setsid + redirect stdio to /dev/null) MUST
happen before `tokio::runtime::Runtime::new()`. Forking a multi-threaded
process is undefined behavior — only the calling thread survives in the
child, and any locks held by other threads are now unlockable.

## Crate responsibilities

### shux-core

- `SessionGraph`: the authoritative state. Single-writer task + ArcSwap snapshots.
- `LayoutEngine`: binary split tree per window; smart_split, directional focus, zoom save/restore.
- `EventBus`: typed events, broadcast, sequence numbers, gap detection.
- `Config`: TOML schema, hot reload via `notify`.

### shux-pty

- `PtyHandle`: per-pane PTY wrapper. async read/write/resize/wait.
- `PtyManager`: spawns child processes, wires output to event bus.
- `CommandEngine`: marker-based completion detection for `pane.run`.

### shux-vt

- `VirtualTerminal`: per-pane terminal emulator (vte parser).
- `Grid`: VecDeque-backed cell grid with scrollback (max 5000 lines).
- Used both inside the daemon (for capture/render) and inside the
  status-bar runner (for parsing ANSI from `starship prompt` etc.).

### shux-rpc

- JSON-RPC 2.0 over length-prefixed framing (4-byte BE + payload).
- UDS server (always on) + optional TCP loopback (auth-token-gated).
- Attach protocol: `AttachHello → AttachReady`, then streaming
  `AttachServerFrame` (Render/Bell/Notice/SessionEnded/Ping) and
  `AttachClientFrame` (Input/Resize/Action/Mouse/Detach/Pong).

### shux-ui

- `RenderCompositor`: composes multi-pane frames, diff-based output.
- `borders` + `statusbar`: data-driven UI primitives.
- `attach`: client-side streaming loop (raw mode, key encoding, etc.).

### shux-plugin

- Stub today. M2 fleshes it out: WASI Preview 2 + Component Model via
  `wasmtime`, capability-based permissions, hot reload, plus process
  plugins as a fallback.

### shux

- The CLI binary. clap definitions, dispatch, daemon auto-start, attach
  client invocation. Holds the per-pane PTY tasks (`run_pane_pty_task`)
  and registers all the JSON-RPC method handlers.

## Why these patterns

| Decision | Rationale |
|---|---|
| Cargo workspace, separate crates | Clean dependency boundaries, parallel compilation, independent testing |
| `rust-toolchain.toml` pins stable | Reproducible builds; PRD requires stable Rust |
| Hand-rolled JSON-RPC (not jsonrpsee) | jsonrpsee lacks native UDS; matches Zellij's pattern |
| cargo-nextest over `cargo test` | Better output, parallelism, JUnit XML for CI, retry support |
| VecDeque grid (not alacritty_terminal) | alacritty_terminal too coupled; PRD §15.2 specifies custom grid |
| Fork-before-tokio daemonization | Fork in a multi-threaded process is undefined behavior |
| ArcSwap for snapshots | Lock-free reads on the hot path; one-shot atomic publish on writes |

For the full design rationale — competitive analysis, plugin WIT
interfaces, performance budgets — see [`PRD.md`](PRD.md).
