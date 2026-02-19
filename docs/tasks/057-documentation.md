# 057 — Documentation (README, Guides, API Reference)

**Status:** Pending
**Depends On:** 052
**Parallelizable With:** 053, 054, 055, 056

---

## Problem

shux needs comprehensive documentation for three audiences: new users who want to try it (README + getting started), plugin authors who want to extend it (plugin guide), and power users/agents who want to automate it (API reference + config reference). The PRD specifies these as M3 deliverables. Good documentation is the difference between "interesting project" and "tool I actually adopt." Every document must be accurate against the implemented codebase, not just the PRD's design — if something changed during implementation, the docs must reflect reality.

## PRD Reference

- **SS 17** M3 deliverables: "README, getting started guide, plugin authoring guide"
- **SS 7.8** Plugin development experience: scaffold, dev mode, testing, inspect, logs
- **SS 8.2** API methods: Complete method list for API reference
- **SS 10.2** Config reference: Full config schema
- **SS 18** Success metrics: "Plugin DX: Working 'hello world' plugin in < 15 minutes — User testing"

---

## Files to Create

- `docs/getting-started.md` — First 5 minutes with shux
- `docs/plugin-guide.md` — Plugin authoring guide with working examples
- `docs/api-reference.md` — Complete API reference (all methods, params, responses, errors)
- `docs/config-reference.md` — Complete configuration reference
- `man/shux.1` — man page (troff format)

## Files to Modify

- `README.md` — Complete rewrite: installation, quick start, features, comparison
- `docs/PROGRESS.md` — Mark task 057 complete

---

## Execution Steps

### Step 1: Rewrite README.md

The README is the project's front door. It must:

```markdown
# shux

> A modern, batteries-included terminal multiplexer. Tiny core, powerful plugins, first-class AI agent support.

<!-- badges: CI, version, license, stars -->

## Why shux?

[2-paragraph pitch: what makes shux different from tmux/Zellij]

## Features

- **Just works** — Beautiful dark theme, discoverable keybindings, zero config needed
- **Per-pane theming** — Color-code prod vs dev, highlight agent panes
- **Plugin system** — Wasm sandbox, hot reload, typed interfaces (WIT)
- **AI-agent first** — Typed JSON-RPC API, event streaming, idempotent operations
- **Fast** — p99 keypress latency ≤ 25ms, p99 split ≤ 80ms
- **Shell completions** — bash, zsh, fish with dynamic completion

## Quick Start

[5-line install + first command + screenshot]

## Installation

### Homebrew (macOS)
### Cargo
### Binary releases
### Build from source

## Usage

### Basic commands
### Keybindings (Tier 1 + Tier 2 table)
### Per-pane theming
### Session templates

## For AI Agents

[Short example: Python script using JSON-RPC API]

## Comparison

| Feature | shux | tmux | Zellij |
[Honest comparison table]

## Documentation

- [Getting Started](docs/getting-started.md)
- [Plugin Guide](docs/plugin-guide.md)
- [API Reference](docs/api-reference.md)
- [Config Reference](docs/config-reference.md)

## Contributing

## License
```

### Step 2: Getting Started Guide

Create `docs/getting-started.md`:

```markdown
# Getting Started with shux

This guide covers your first 5 minutes with shux.

## Install

[3 installation methods]

## First session

$ shux

[Explain what happens: daemon starts, session created, you're attached]

## Navigate

### Panes
- Alt+h/j/k/l — move focus
- Alt+Enter — smart split
- Alt+z — zoom/unzoom
- Ctrl+Space | — vertical split
- Ctrl+Space - — horizontal split

### Windows
- Alt+n/p — next/prev window
- Alt+1-9 — switch by number
- Ctrl+Space c — new window

### Sessions
- shux ls — list sessions
- shux attach -s <name> — attach
- Ctrl+Space d — detach

## Customize

### Themes
$ shux theme ls
$ shux theme set catppuccin-mocha
$ shux theme set -p <pane> prod    # Per-pane!

### Config
~/.config/shux/config.toml

[Minimal config example]

## Next steps

- [Plugin Guide](plugin-guide.md) — extend shux
- [API Reference](api-reference.md) — automate with agents
- [Config Reference](config-reference.md) — full configuration
```

### Step 3: Plugin Authoring Guide

Create `docs/plugin-guide.md` — this is the most important document for ecosystem growth:

```markdown
# Plugin Authoring Guide

Build plugins for shux in any language. This guide walks through
creating, testing, and publishing a plugin.

## Overview

shux plugins come in two kinds:
- **Wasm plugins** (recommended) — Portable, sandboxed, fast
- **Process plugins** — Any language, stdio-based (experimental)

## Quick Start: Hello World Plugin

### Scaffold

$ shux plugin init hello-world --kind wasm
$ cd hello-world/

### Structure

hello-world/
├── plugin.toml      # Manifest
├── Cargo.toml       # Rust build
├── src/lib.rs       # Plugin code
└── README.md

### plugin.toml

[Complete annotated example]

### Implementation

[Step-by-step Rust code with explanation]

### Build

$ cargo build --target wasm32-wasip2 --release

### Test

$ shux plugin dev ./hello-world/

### Install

$ cp target/wasm32-wasip2/release/hello_world.wasm ~/.config/shux/plugins/hello-world/plugin.wasm

## Permissions

[Table of all permissions with examples and security implications]

### Permission examples

[3-4 examples: theme plugin (no permissions), status plugin (events),
orchestrator plugin (manage_panes + send_keys), audit plugin (intercept_events)]

## Extension Points

### Commands
[How to register and handle commands]

### Status bar segments
[How to provide status bar content]

### Pane overlays
[How to show interactive overlays]

### Theme packs
[How to provide themes]

### Event reactors
[How to subscribe to and react to events]

### API extensions
[How to register new API methods]

### Event interceptors
[How to intercept and modify events]

## Testing Your Plugin

### Unit tests
$ cargo test

### Integration testing
$ shux plugin test ./my-plugin/

### Dev mode (hot reload)
$ shux plugin dev ./my-plugin/
[Edit code → auto-recompile → auto-reload → see changes instantly]

## Process Plugins

### Protocol overview
[Length-prefixed JSON over stdio]

### Hello handshake
[Example exchange]

### Python example
[Complete working Python process plugin]

### Message reference
[All message types with examples]

## Debugging

### Logs
$ shux logs tail --plugin com.example.my-plugin

### Inspect
$ shux plugin inspect com.example.my-plugin

### Common errors
[Troubleshooting table]

## Publishing

### plugin.toml metadata
[What to fill in for discoverability]

### Versioning
[Semver rules for plugin API compatibility]
```

### Step 4: API Reference

Create `docs/api-reference.md`:

```markdown
# API Reference

Complete reference for shux's JSON-RPC API. Every operation available
via CLI is also available via API.

## Transport

### Unix Domain Socket (default)
[Connection details, framing format]

### TCP (opt-in)
[Connection details, auth token]

### gRPC (opt-in)
[Proto file locations, connection details]

## Request/Response Format

[JSON-RPC 2.0 format with examples]

## Error Codes

| Code | Name | Description |
|-|-|-|
| -32600 | invalid_request | Malformed JSON-RPC |
| -32601 | method_not_found | Unknown method |
| -32602 | invalid_params | Invalid parameters |
| -32001 | version_conflict | Optimistic concurrency violation |
| -32002 | not_found | Resource not found |
| -32003 | already_exists | Resource already exists |
| ... | ... | ... |

## Methods

### System

#### system.version
[Params, response, errors, example]

#### system.health
[Params, response, errors, example]

### Sessions

#### session.list
#### session.create
#### session.ensure
#### session.rename
#### session.kill
#### session.attach

### Windows

[All window methods]

### Panes

[All pane methods — split, focus, resize, zoom, swap, kill,
send_keys, run_command, capture, set_title, set_theme, set_tag]

### State

#### state.snapshot
#### state.apply
[Batch operations with back-references example]

### Events

#### events.watch
[Streaming subscription, filters, resume, gap detection]

#### events.history

### Themes

[All theme methods]

### Config

[All config methods]

### Plugins

[All plugin methods]

### Observability

#### diagnose.run
#### metrics.get
#### log.set_level
#### log.tail

## Event Types

[Complete event taxonomy from Appendix A]

## Agent Patterns

### Read → Plan → Apply → Verify loop
### Using ensure operations
### Batch operations with back-references
### Event-driven monitoring
[Working code examples for each pattern]
```

### Step 5: Config Reference

Create `docs/config-reference.md`:

```markdown
# Configuration Reference

## Config File Locations

| Priority | Location | Description |
|-|-|-|
| 1 | Built-in defaults | Compiled into binary |
| 2 | /etc/shux/config.toml | System-wide |
| 3 | ~/.config/shux/config.toml | User config |
| 4 | .shux/config.toml | Project config |
| 5 | Runtime API | `config.set` method |

## Complete Schema

### [daemon]
[Every field with type, default, description, example]

### [ui]
[Every field: prefix, mouse, status_bar, scrollback_lines, etc.]

### [theme]
[name, paths]

### [copy]
[osc52, mouse_select_copies, vi_keys]

### [plugins]
[paths, allow_process_plugins, process_gc_timeout_secs]

### [shell]
[default_command, login_shell]

### [keybindings]
[Override syntax, reserved keys, examples]

## Theme Files

### Token Schema
[Complete token set with types and examples]

### Example Theme
[Full theme TOML file]

## Session Templates

### Template Syntax
[Fields, variables, layout values]

### Example Template
[Working template with variables]

## Validation

$ shux config validate
$ shux config explain
```

### Step 6: Man Page

Create `man/shux.1`:

```troff
.TH SHUX 1 "2026" "shux 1.0.0" "User Commands"
.SH NAME
shux \- modern terminal multiplexer
.SH SYNOPSIS
.B shux
[\fIcommand\fR] [\fIoptions\fR]
.SH DESCRIPTION
.B shux
is a modern, batteries-included terminal multiplexer with a plugin
system, typed API, and first-class AI agent support.
.PP
When invoked without arguments, shux attaches to the last session
or creates a new one named "default".
.SH COMMANDS
.TP
.B new \-s \fIname\fR
Create a new session
.TP
.B attach \-s \fIname\fR
Attach to an existing session
.TP
.B ls
List sessions
.TP
.B split \-d \fIv|h\fR
Split the current pane
.TP
.B theme ls
List available themes
.TP
.B theme set \fIname\fR
Set the active theme
.TP
.B plugin ls
List plugins
.TP
.B doctor
Run diagnostics
.TP
.B completions \fIshell\fR
Generate shell completions (bash, zsh, fish)
.TP
.B api \fImethod\fR [\-\-format json|text]
Call a JSON-RPC API method directly
.SH KEYBINDINGS
.SS Tier 1 (bare keys)
.TP
.B Alt+h/j/k/l
Focus pane left/down/up/right
.TP
.B Alt+n/p
Next/previous window
.TP
.B Alt+z
Toggle zoom
.TP
.B Alt+Enter
Smart split
.SS Tier 2 (prefix: Ctrl+Space)
.TP
.B c
New window
.TP
.B x
Close pane
.TP
.B |
Vertical split
.TP
.B \-
Horizontal split
.TP
.B :
Command palette
.TP
.B ?
Help overlay
.TP
.B d
Detach
.SH FILES
.TP
.I ~/.config/shux/config.toml
User configuration
.TP
.I ~/.config/shux/themes/
Theme files
.TP
.I ~/.config/shux/plugins/
Plugin directory
.TP
.I $XDG_RUNTIME_DIR/shux/shux.sock
Unix domain socket
.SH ENVIRONMENT
.TP
.B SHUX_SOCKET
Override socket path
.TP
.B SHUX_CONFIG
Override config file path
.SH SEE ALSO
.BR tmux (1),
.BR zellij (1)
.SH AUTHORS
indrasvat
.SH BUGS
https://github.com/indrasvat/shux/issues
```

### Step 7: Verify Documentation Accuracy

After writing all docs, verify against the actual codebase:

```bash
# Verify all CLI commands mentioned in docs actually exist
cargo run -p shux -- --help

# Verify all API methods mentioned in api-reference.md exist
grep -oP '#### (\w+\.\w+)' docs/api-reference.md | sort | uniq

# Verify all config fields mentioned in config-reference.md exist
grep -oP '^\w+' crates/shux-core/src/config.rs | sort | uniq

# Verify all keybindings mentioned in README are implemented
grep 'Alt+' README.md
```

### Step 8: Add Documentation Tests

Ensure code examples in docs compile:

```rust
// In lib.rs or a dedicated docs test file:
#[cfg(test)]
mod doc_tests {
    #[test]
    fn readme_quick_start_compiles() {
        // Verify the README's quick start example code is valid
        // (This is a placeholder — actual doc tests use cargo test --doc)
    }
}
```

---

## Verification

### Functional

```bash
# Verify all documentation files exist
ls README.md docs/getting-started.md docs/plugin-guide.md docs/api-reference.md docs/config-reference.md man/shux.1

# Verify man page renders
man -l man/shux.1

# Verify links in README point to existing files
grep -oP '\(docs/[^)]+\)' README.md | tr -d '()' | while read f; do
    test -f "$f" || echo "BROKEN LINK: $f"
done

# Verify no broken internal links in docs
for doc in docs/*.md; do
    grep -oP '\]\([^)]+\)' "$doc" | tr -d '()' | while read link; do
        if [[ "$link" != http* ]]; then
            test -f "docs/$link" -o -f "$link" || echo "BROKEN: $doc -> $link"
        fi
    done
done
```

### Tests

```bash
# Run doc tests
cargo test --workspace --doc

# Verify README code blocks are syntactically valid
# (Manual review)
```

---

## Completion Criteria

- [ ] README.md: installation (Homebrew, cargo, binary), quick start, features, keybindings, comparison table, documentation links
- [ ] docs/getting-started.md: first 5 minutes covering install, first session, navigation, customization
- [ ] docs/plugin-guide.md: scaffold, hello world step-by-step, permissions explained, all extension points, testing, process plugin example, debugging
- [ ] docs/api-reference.md: every method documented with params, response, errors, and working example
- [ ] docs/config-reference.md: every config field with type, default, description, example
- [ ] man/shux.1: renders correctly with `man -l`, covers commands, keybindings, files, environment
- [ ] All code examples in docs are accurate against implemented codebase
- [ ] No broken internal links between documents
- [ ] Plugin guide enables "hello world in < 15 minutes" (PRD SS18 metric)
- [ ] API reference covers all methods from SS8.2
- [ ] Config reference covers all fields from SS10.2

---

## Commit Message

```
docs: add README, getting started, plugin guide, API reference, config reference, man page

- README rewrite with installation, quick start, features, comparison
- Getting started guide for first 5 minutes with shux
- Plugin authoring guide with scaffold, hello world, permissions,
  extension points, testing, process plugin, and debugging
- Complete API reference for all JSON-RPC methods
- Complete config reference for all TOML fields
- man page (shux.1) with commands, keybindings, files
```

---

## Session Protocol

1. **Before starting:** Read the implemented codebase to understand what actually works (not just what the PRD specifies). Run `shux --help` and every subcommand's `--help` to get the real CLI interface. Test every API method to verify the response format.
2. **During:** Write docs in order: README (Step 1), getting started (Step 2), plugin guide (Step 3), API reference (Step 4), config reference (Step 5), man page (Step 6). After each document, have a second pass for accuracy.
3. **Accuracy over completeness:** It is better to have fewer documented features that are accurate than more features that are wrong. Every code example must work. Every config field must be real.
4. **Plugin guide is the crown jewel:** The PRD's success metric for plugin DX is "working hello world in < 15 minutes." Time yourself following the guide. If it takes longer, simplify.
5. **After:** Verify all links work. Run doc tests. Have someone unfamiliar with shux follow the getting started guide. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing).
