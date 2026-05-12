# Task 044a: Process plugin protocol v0 — the Pi-style unlock

**Status:** Done (phase 0)
**Milestone:** M2
**Depends on:** 035 (RPC surface), 036 (events)
**Supersedes (for v0):** the WASM-first sequencing in 038–040. WASM is
deferred to v1.1. Tasks 041 (lifecycle/hot-reload), 042 (event
interception), 043 (command override), 044 (process protocol), 045
(API extensions) are folded into a process-first DX-first stack.

## Why this exists

The PRD (§7 "Plugin system — the crown jewel", §3.1 P4 persona,
§Appendix references to badlogic/pi-mono) is unambiguous: extension
is the moat, and the unlock is *agent-authored* plugins on a
hot-reload loop. WASM gives us sandboxed third-party distribution
but it does not give us DX. Process plugins do — same trust model
as a shell function, any language, sub-second feedback. We're
pre-1.0 with zero external users, so we can ship the no-compromise
DX now and add the sandboxed-distribution layer later.

## Vision (target end state)

```bash
# Agent authors a plugin in 30 seconds:
shux plugin scaffold lint-on-commit --runtime bun
$EDITOR ~/.config/shux/plugins/lint-on-commit/index.ts
# (drops a 20-line plugin that subscribes to pane.input,
#  re-formats commit messages before they hit git)
shux plugin install ~/.config/shux/plugins/lint-on-commit
# ✓ installed lint-on-commit v0.1 (bun, subscribes: pane.input)

# Hot-reload on save:
$EDITOR ~/.config/shux/plugins/lint-on-commit/index.ts
# (save → notify fires → shux reloads in <200ms)

# Inspect:
shux plugin list      # name · version · runtime · status · uptime
shux plugin debug lint-on-commit   # tails plugin stdout + RPC frames

# Uninstall:
shux plugin kill lint-on-commit
```

## Phasing

This task is large. We slice it into three PRs so each lands a real
demo, not just plumbing.

### Phase 0 — protocol + spawn + manual install (this PR)

Minimum surface that proves the protocol end-to-end:

- `crates/shux-plugin/`: line-delimited JSON IPC, `Plugin` handle,
  `PluginManager`, init/handshake/teardown, event forwarding from
  daemon → plugin, RPC forwarding from plugin → daemon.
- Daemon: `register_plugin_methods` registers `plugin.install`,
  `plugin.list`, `plugin.kill`. Plugins are spawned children whose
  stdin/stdout speak shux's JSON-RPC dialect.
- CLI: `shux plugin {install, list, kill}`.
- One example plugin under `examples/plugins/hello/` — a POSIX
  shell plugin (~30 lines) that handshakes, subscribes to
  `session.created`, and writes a `pane.send_keys` greeting into
  the new session's first pane. Proves the bidirectional surface
  with zero runtime dependencies.
- Integration test: install hello plugin → create a session →
  assert the greeting lands in the pane's captured output.

**Out of scope for phase 0:** hot reload, scaffold, override-by-name,
event interception, the bundled trio, the website gallery tab. All
queued for phase 1 / phase 2 below.

### Phase 1 — DX layer

- `notify`-backed file watcher on `~/.config/shux/plugins/` →
  on file change, send `plugin.reload` to that plugin (kill +
  respawn, re-handshake). Target latency: <200ms.
- `shux plugin scaffold <name> [--runtime bun|python|sh]` —
  drops a starter directory with manifest + handler + a
  self-test script.
- `shux plugin debug <name>` — tails plugin stdout/stderr +
  every RPC frame in/out, like `journalctl -f` for one plugin.
- `plugin.install_inline` RPC — accepts a code blob in the
  request, writes to a managed tmpdir, registers. Lets a coding
  agent write→register in one tool call.
- Override-by-name: plugins declare `provides: ["pane.send_keys"]`
  in their manifest; on call, the daemon routes through the
  plugin first with a "call original" continuation. Pi's pattern.
- Event interception chain: plugins declare `intercepts:
  ["pane.input"]`; the daemon serializes pre-publish hooks
  through the chain, allowing modify/block.
- Install-consent prompt: first install of a non-bundled plugin
  prints capabilities + asks `[Y/n]` once, persists trust in
  `~/.config/shux/plugins/trusted.toml`. No sandbox; trust is
  human-supervised at install time.

### Phase 2 — bundled trio + website showcase

Three bundled plugins prove the surface and ship the demo story:

1. **`shux-mcp`** (Node/Bun) — exposes `pane.*`, `window.*`,
   `session.*` as MCP tools over stdio. Any MCP-aware client
   (Claude Desktop, Cursor, Codex) drives shux natively.
2. **`shux-conductor`** (Bun) — reads `.shux/conductor.toml` from
   the active cwd, spawns the declared workspace, subscribes to
   `pane.exit` events to re-spawn / re-focus on configured
   triggers. Declarative reactive workspaces.
3. **`shux-diagnostics`** (Bun) — subscribes to `pane.output`
   data plane, parses cargo/rustc/eslint/tsc/pytest output into
   a quickfix list, exposes `diagnostics.list` /
   `diagnostics.jump` RPCs.

**Website showcase** (`pages/index.html` gallery section):

- New gallery tab at `data-i=0` (push others down by one again):
  **"Plugins: agents extending shux on the fly"**. Tab artwork is
  a 3-panel composite:
  - **Left panel**: `shux plugin scaffold lint-on-commit` →
    `shux plugin install ./lint-on-commit` → `shux plugin list`
    output, captured as a real `pane.snapshot`. Sells the install
    DX in one frame.
  - **Middle panel**: the resulting plugin code (~25 lines of TS)
    rendered through `bat` in a shux pane — proves how *small* a
    useful plugin is.
  - **Right panel**: the plugin actually intervening in a `git
    commit` flow inside a separate pane — the commit message gets
    auto-reformatted before it hits git. Proves it *works*.
  All three frames composed with `magick montage`, same pattern
  as `multi-agent.png` (PR #22).
- Companion ASCII timeline strip below the tab caption: "scaffold
  → edit → save → hot-reload → run" with `<200ms` between steps.
- New section under "in the wild" that lists the bundled trio as
  cards (icon + 1-line pitch + `shux plugin install <name>` copy
  button), styled in the same terracotta/moss palette as the
  existing gallery cards. Each card links to a per-plugin
  page (`/plugins/mcp.html`, `/plugins/conductor.html`,
  `/plugins/diagnostics.html`) with a longer walkthrough.
- "Build your own" CTA: an aesthetic single-pane snippet showing
  a coding agent typing `shux plugin scaffold my-extension` into
  a Claude/Codex/opencode session, with the resulting `index.ts`
  appearing in the adjacent pane via `pane.send_keys`. Sells the
  agent-authoring story in one image.
- Eyebrow copy update: "Nine tabs" (one for plugins). Or pull the
  plugin showcase out of the gallery into its own marquee
  section between gallery and quickstart, depending on whether
  it reads more as "demo" or "platform pitch" — decide visually.

## Protocol (phase 0)

JSON-RPC 2.0 over line-delimited UTF-8 on the child's stdin/stdout.
stderr is captured by the daemon and tagged with the plugin name
for debug output. One JSON object per line, no nested newlines in
strings (JSON encoder handles this).

### Handshake

```
daemon → plugin (stdin):  {"jsonrpc":"2.0","method":"plugin.init",
                           "params":{"shux_version":"0.13.0",
                                     "rpc_schema_uri":"..."},
                           "id":1}
plugin → daemon (stdout): {"jsonrpc":"2.0","result":{
                             "name":"hello",
                             "version":"0.1.0",
                             "subscribes":["session.created"],
                             "provides":[],
                             "capabilities":[]
                           },"id":1}
```

### Plugin → daemon (calls into shux)

The plugin sends any registered RPC method:

```
plugin → daemon: {"jsonrpc":"2.0","method":"pane.send_keys",
                  "params":{"session":"s0","text":"hi"},"id":42}
daemon → plugin: {"jsonrpc":"2.0","result":{...},"id":42}
```

### Daemon → plugin (events)

For each event matching the plugin's `subscribes` filters, the
daemon writes a notification (no `id`):

```
daemon → plugin: {"jsonrpc":"2.0","method":"event",
                  "params":{"type":"session.created",
                            "data":{"session_id":"...","name":"s0"},
                            "seq":7}}
```

### Shutdown

`plugin.kill` RPC → daemon writes a `plugin.shutdown` notification
to the plugin's stdin → 2-second grace timer → SIGKILL if still
alive. The plugin can drain in-flight RPCs during the grace window.

## Permissions (phase 0)

None enforced. All RPC methods are callable. This matches the v0
trust model: a process plugin runs as the user and can already
read/write anything the user can. Install-consent (phase 1) makes
the user explicitly opt in once per plugin. Sandboxed distribution
ships in v1.1 via WASM.

## Acceptance (phase 0)

- `shux plugin install ./examples/plugins/hello` registers and
  spawns the plugin; `plugin.list` shows it as `running`.
- Creating a session fires `session.created`; the hello plugin
  receives the event and calls `pane.send_keys` with a greeting;
  `pane.capture` on the new session's first pane returns text
  containing the greeting.
- `shux plugin kill hello` terminates the child within 2s and
  removes it from `plugin.list`.
- A new integration test (`tests/plugin_integration.rs` or
  similar) exercises the loop end-to-end.
- All existing tests pass; no new clippy warnings.
- Existing `make ci` target passes.

## Out of scope (entirely)

- WASM. Comes back in v1.1 / phase ∞.
- The marketplace, signing, capability tokens. Same.
- Plugin-to-plugin communication (task 047) — phase 2 once the
  event bus is plumbed through to plugins.
- A graphical plugin manager. Forever a CLI affair.
