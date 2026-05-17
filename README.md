<p align="center">
  <img src="assets/logo.svg" alt="shux" width="400">
</p>

<p align="center">
  <strong>The terminal multiplexer built for AI coding agents</strong><br>
  <em>Typed API. Deterministic state. Zero wrappers needed.</em><br>
  <a href="https://github.com/indrasvat/shux/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/indrasvat/shux/ci.yml?branch=main&style=flat&labelColor=1a1a1a&label=CI" alt="CI"></a>
  <a href="https://app.codecov.io/gh/indrasvat/shux"><img src="https://img.shields.io/codecov/c/github/indrasvat/shux?style=flat&labelColor=1a1a1a" alt="coverage"></a>
  <a href="https://github.com/indrasvat/shux/releases/latest"><img src="https://img.shields.io/github/v/release/indrasvat/shux?style=flat&labelColor=1a1a1a" alt="release"></a>
  <img src="https://img.shields.io/badge/lang-Rust-DEA584?style=flat&labelColor=1a1a1a" alt="Rust">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat&labelColor=1a1a1a" alt="MIT">
</p>

---

## Why shux

Every AI agent orchestration tool today wraps tmux. They do it because no
multiplexer offers a typed API for programmatic control. shux is what tmux
would be if designed today: humans and agents are equal citizens, every
operation is available through both keyboard and JSON-RPC, state is always
queryable, deterministic, and streamable.

```bash
# As a human
shux                                    # attach to last session, or create "default"

# As an agent — every CLI verb mirrors an RPC method 1:1.
# RPC dots become CLI spaces. No top-level shortcuts to memorize.
shux session create work                # → session.create RPC, cwd = caller $PWD
shux session create work --title work   # pin the initial pane border title
shux session list                       # → session.list
shux pane send-keys -s work --text 'j'  # → pane.send_keys
shux rpc call <method> --params @file   # raw fallthrough for any method
```

## Install

The fastest path — pre-built binary for macOS and Linux (x86_64 / aarch64),
verified by SHA-256 and dropped into `~/.local/bin`:

```bash
curl -sSfL https://raw.githubusercontent.com/indrasvat/shux/main/install.sh | bash
```

Pin a version or change the install dir:

```bash
curl -sSfL https://raw.githubusercontent.com/indrasvat/shux/main/install.sh \
  | bash -s -- --version v0.1.0 --dir ~/.bin
```

Or build from source:

```bash
git clone https://github.com/indrasvat/shux.git
cd shux
make install   # → ~/.local/bin/shux
```

Requires Rust 1.93+ and a Unix-like OS (macOS, Linux). For dev setup, see
[`docs/development.md`](docs/development.md).

## Quickstart

```bash
shux                              # attach last session (TTY-only)
shux session create work          # create + attach in the caller's cwd
shux session create work --title work  # also pin the initial pane title
shux pane split -s work           # split the active pane
shux pane snapshot -s work        # PNG of the current frame
shux config init                  # scaffold ~/.config/shux/config.toml
```

Every command mirrors an RPC method: `shux session create` is the same
RPC as `session.create`. Drop to the raw form with
`shux rpc call session.create --params @spec.json` when you'd rather
write the payload in a file. CLI-created sessions start in the current
directory of the `shux` invocation unless you pass `--cwd`. Use `--title`
when the initial pane's border label should be pinned instead of following
app-emitted OSC titles or command/cwd auto-title derivation.

Inside the TUI, the prefix key is `Ctrl+Space` by default:

| Key | Action |
|---|---|
| `Ctrl+Space \|`/`-` | split vertical / horizontal |
| `Ctrl+Space h`/`j`/`k`/`l` | focus left / down / up / right |
| `Ctrl+Space z` | toggle zoom |
| `Ctrl+Space d` | detach |
| click any pane | focus it |
| drag a border | resize |

## Extend shux with a process plugin

A plugin is any executable that speaks shux's line-delimited
JSON-RPC dialect on stdin/stdout. It subscribes to bus events and can
call the same RPC methods you use from outside (`window.rename`,
`pane.send_keys`, `state.apply`, …). Any language — bash, python,
node — same trust model as a shell function.

```bash
shux plugin install ./my-plugin.sh   # spawn, handshake, register
shux plugin list                      # what's running
shux plugin kill <name>               # graceful shutdown (2s) → SIGKILL
```

The smallest correct shape is
[`examples/plugins/hello/plugin.sh`](examples/plugins/hello/plugin.sh)
(~30 lines of bash). Full protocol is documented in
[`skills/shux/references/plugins.md`](skills/shux/references/plugins.md)
and the task design doc
[`docs/tasks/044a-process-plugins-v0.md`](docs/tasks/044a-process-plugins-v0.md).

## Documentation

Read in this order:

1. [**Quickstart for humans**](docs/users.md) — keybindings, status bar,
   customization, dotfile integration
2. [**Quickstart for agents**](docs/agents.md) — typed JSON-RPC surface,
   `ensure` semantics, event streaming, scripting patterns
3. [**Configuration**](docs/configuration.md) — `~/.config/shux/config.toml`
   schema, hot reload, status-bar segments, starship integration
4. [**Architecture**](docs/architecture.md) — daemon model, the seven crates,
   why each one exists, the patterns that hold it all together
5. [**Development**](docs/development.md) — dev setup, make targets, the L1–L4
   testing strategy
6. [**Roadmap**](docs/roadmap.md) — what's done, what's next, milestone plan
7. [**Full PRD**](docs/PRD.md) — design philosophy, competitive analysis,
   plugin WIT interfaces, performance budgets

## License

MIT
