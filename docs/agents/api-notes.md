# Important API Notes

## Crate Versions (Validated Feb 2026)

- `crossterm` 0.29 — Kitty keyboard, synchronized output, OSC 52
- `vte` 0.15 — with `ansi` feature for typed handler callbacks
- `ratatui` 0.30 — workspace reorganization, used for chrome only
- `wasmtime` 41+ — WASI Preview 2, Component Model, epoch interruption
- `pty-process` 0.5.3 — AsyncRead/AsyncWrite, tokio integration
- `arc-swap` 1.x — lock-free state snapshots
- `clap` 4.x — derive macro, subcommands, completions

## Architecture Patterns

- `SessionGraph` owns all state. ArcSwap for lock-free reads.
- Single-writer mutation channel (tokio::sync::mpsc → state-owner task)
- Event bus: tokio::sync::broadcast + sequence numbers (AtomicU64) + gap detection
- Plugin host: wasmtime Engine + Linker shared; per-plugin Store (dropped on hot reload)
