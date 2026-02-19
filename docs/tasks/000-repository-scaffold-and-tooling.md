# 000 — Repository Scaffold and Tooling

**Status:** In Progress
**Depends On:** —
**Parallelizable With:** —

---

## Problem

Set up the shux repository with all foundational tooling, configuration, and documentation so that all subsequent tasks can build on a consistent, well-configured base. This task establishes the Rust project structure, build system, code quality tooling, git hooks, CI pipeline, and agent instructions (CLAUDE.md/AGENTS.md).

The GH repo already exists at `git@github.com:indrasvat/shux.git` on the `create-shux` branch. The `docs/PRD.md` and `docs/use_cases/shux_plugin_use_cases.md` are already committed.

## PRD Reference

- §15.1 Language & toolchain (Rust stable, pinned via rust-toolchain.toml)
- §15.2 Key crate families (tokio, crossterm, vte, wasmtime, clap, etc.)
- §15.3 Build (cargo build --release, cross-compilation)
- §16 Testing strategy (cargo-nextest, cargo-fuzz)
- §14.1 Performance budgets (benchmarking framework)

---

## Files to Create

- `rust-toolchain.toml` — Pin Rust stable channel
- `Cargo.toml` — Workspace root with initial crate structure
- `crates/shux/Cargo.toml` — Main binary crate
- `crates/shux/src/main.rs` — Entrypoint stub
- `crates/shux-core/Cargo.toml` — Core library crate (daemon, data model, event bus)
- `crates/shux-core/src/lib.rs` — Library stub
- `crates/shux-pty/Cargo.toml` — PTY manager crate
- `crates/shux-pty/src/lib.rs` — Library stub
- `crates/shux-vt/Cargo.toml` — Virtual terminal grid crate
- `crates/shux-vt/src/lib.rs` — Library stub
- `crates/shux-rpc/Cargo.toml` — JSON-RPC server crate
- `crates/shux-rpc/src/lib.rs` — Library stub
- `crates/shux-plugin/Cargo.toml` — Plugin host crate
- `crates/shux-plugin/src/lib.rs` — Library stub
- `crates/shux-ui/Cargo.toml` — TUI client crate
- `crates/shux-ui/src/lib.rs` — Library stub
- `Makefile` — Exhaustive build/test/lint/release targets
- `lefthook.yml` — Pre-commit and pre-push hooks
- `clippy.toml` — Clippy configuration
- `.cargo/config.toml` — Cargo build configuration
- `CLAUDE.md` — Agent instructions (source of truth)
- `AGENTS.md` — Redirect to CLAUDE.md
- `.claude/settings.json` — Claude Code settings (hooks, permissions)
- `.github/workflows/ci.yml` — GitHub Actions CI
- `scripts/setup-dev.sh` — Developer environment setup script
- `deny.toml` — cargo-deny configuration (license/advisory audit)

## Files to Modify

- `.gitignore` — Add Rust/cargo-specific entries
- `docs/PROGRESS.md` — Mark task 000 as in-progress/done

---

## Execution Steps

### Step 1: Pin Rust toolchain

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy", "rust-src"]
targets = ["wasm32-wasip2"]
```

**Why pin stable (not a specific version):** The PRD says "Rust stable" (§15.1). We pin `stable` and let CI validate. The `wasm32-wasip2` target is needed for compiling Wasm plugins in M2.

### Step 2: Create Cargo workspace

Create `Cargo.toml` (workspace root):

```toml
[workspace]
resolver = "2"
members = [
    "crates/shux",
    "crates/shux-core",
    "crates/shux-pty",
    "crates/shux-vt",
    "crates/shux-rpc",
    "crates/shux-plugin",
    "crates/shux-ui",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "MIT"
repository = "https://github.com/indrasvat/shux"
authors = ["indrasvat"]

[workspace.dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Terminal
crossterm = "0.29"
ratatui = "0.30"

# VT parsing
vte = { version = "0.15", features = ["ansi"] }

# PTY
pty-process = "0.5"

# Plugin (Wasm)
wasmtime = "41"
wasmtime-wasi = "41"

# State
arc-swap = "1"

# CLI
clap = { version = "4", features = ["derive", "env"] }

# Tracing
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# JSON-RPC types
json-rpc-types = "1"

# Error handling
thiserror = "2"
anyhow = "1"

# UUID
uuid = { version = "1", features = ["v4", "serde"] }

# Testing
tempfile = "3"
assert_cmd = "2"
predicates = "3"

# gRPC (optional, for M2)
tonic = "0.12"
prost = "0.13"

# Daemonization
nix = { version = "0.29", features = ["process", "signal", "fs"] }

# Graceful shutdown
tokio-util-crate = { package = "tokio-util", version = "0.7", features = ["rt"] }
```

### Step 3: Create crate stubs

Create each crate with a minimal Cargo.toml and lib.rs/main.rs.

**`crates/shux/Cargo.toml`:**
```toml
[package]
name = "shux"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
description = "A modern, batteries-included terminal multiplexer"
default-run = "shux"

[[bin]]
name = "shux"
path = "src/main.rs"

[dependencies]
shux-core = { path = "../shux-core" }
shux-pty = { path = "../shux-pty" }
shux-vt = { path = "../shux-vt" }
shux-rpc = { path = "../shux-rpc" }
shux-plugin = { path = "../shux-plugin" }
shux-ui = { path = "../shux-ui" }
clap.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
anyhow.workspace = true
```

**`crates/shux/src/main.rs`:**
```rust
fn main() {
    println!("shux v{}", env!("CARGO_PKG_VERSION"));
}
```

**Each library crate (`shux-core`, `shux-pty`, `shux-vt`, `shux-rpc`, `shux-plugin`, `shux-ui`):**

Cargo.toml pattern:
```toml
[package]
name = "shux-<name>"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
# Added per-task as needed
```

lib.rs:
```rust
//! shux <name> — <one-line description>
```

### Step 4: Create Makefile

```makefile
.PHONY: build test lint check ci clean install install-tools install-hooks fmt bench doc release

# Default target
all: check

# ── Build ────────────────────────────────────────
build:
	cargo build

release:
	cargo build --release

# ── Test ─────────────────────────────────────────
test:
	cargo nextest run --workspace

test-verbose:
	cargo nextest run --workspace --no-capture

test-lib:
	cargo nextest run --workspace --lib

test-doc:
	cargo test --workspace --doc

test-coverage:
	cargo llvm-cov nextest --workspace --lcov --output-path lcov.info

# ── Lint ─────────────────────────────────────────
lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check

fmt:
	cargo fmt --all

# ── Check (what pre-commit runs) ─────────────────
check: lint test

# ── CI (fail-fast) ───────────────────────────────
ci: lint test-lib test-doc

# ── Bench ────────────────────────────────────────
bench:
	cargo bench --workspace

# ── Doc ──────────────────────────────────────────
doc:
	cargo doc --workspace --no-deps --document-private-items

# ── Install ──────────────────────────────────────
install: release
	install -d ~/.local/bin
	install -m 755 target/release/shux ~/.local/bin/shux

install-tools:
	cargo install cargo-nextest --locked
	cargo install cargo-llvm-cov --locked
	cargo install cargo-deny --locked
	cargo install cargo-fuzz --locked
	cargo install lefthook --locked || npm i -g lefthook

install-hooks:
	lefthook install

# ── Clean ────────────────────────────────────────
clean:
	cargo clean
	rm -f lcov.info

# ── Deny (license/advisory audit) ────────────────
deny:
	cargo deny check

# ── Fuzz ─────────────────────────────────────────
fuzz:
	@echo "Run individual fuzz targets with: cargo fuzz run <target>"
	@echo "Available targets (after M3 task 056):"
	@echo "  cargo fuzz run fuzz_vt_parser"
	@echo "  cargo fuzz run fuzz_json_rpc"
	@echo "  cargo fuzz run fuzz_config"
	@echo "  cargo fuzz run fuzz_layout"
```

### Step 5: Create lefthook.yml

```yaml
# lefthook.yml — git hooks for shux
# Install: lefthook install (or: make install-hooks)

pre-commit:
  parallel: true
  commands:
    fmt-check:
      glob: "*.rs"
      run: cargo fmt --all -- --check
    clippy:
      glob: "*.rs"
      run: cargo clippy --workspace --all-targets -- -D warnings

pre-push:
  commands:
    test:
      run: cargo nextest run --workspace
    deny:
      run: cargo deny check 2>/dev/null || true
```

### Step 6: Create clippy.toml

```toml
# clippy.toml — Clippy configuration for shux
msrv = "1.85"
```

### Step 7: Create .cargo/config.toml

```toml
[build]
# Use mold linker on Linux for faster linking (if available)
# Uncomment below for your platform:
# [target.x86_64-unknown-linux-gnu]
# linker = "clang"
# rustflags = ["-C", "link-arg=-fuse-ld=mold"]

# [target.aarch64-unknown-linux-gnu]
# linker = "clang"
# rustflags = ["-C", "link-arg=-fuse-ld=mold"]

[alias]
xt = "nextest run"
```

### Step 8: Create CLAUDE.md

```markdown
# CLAUDE.md — shux AI Agent Instructions

> **This file is the source of truth for all AI coding agents working on shux.**
> AGENTS.md points here. Do not duplicate instructions elsewhere.

## Project Overview

**shux** is a modern, batteries-included terminal multiplexer built in Rust.
Tiny core, powerful plugin system, first-class support for both humans and AI agents.

- **PRD:** `docs/PRD.md` — full product requirements, architecture, UI specs
- **Use Cases:** `docs/use_cases/shux_plugin_use_cases.md` — plugin architecture validation
- **Progress:** `docs/PROGRESS.md` — implementation tracker (MUST be kept current)
- **Tasks:** `docs/tasks/NNN-descriptive-name.md` — individual task specifications

## Build & Test Commands

```bash
make build           # Build all crates (debug)
make release         # Build optimized binary → target/release/shux
make test            # Run tests with cargo-nextest (all workspace crates)
make test-verbose    # Run tests with output visible
make test-lib        # Run library tests only
make lint            # Run clippy + rustfmt check
make check           # lint + test (what pre-commit runs)
make ci              # CI-only target (lint + test-lib + test-doc, fail-fast)
make install         # Install to ~/.local/bin/shux
make install-tools   # Install dev dependencies (nextest, llvm-cov, deny, fuzz, lefthook)
make install-hooks   # Install lefthook git hooks
make bench           # Run benchmarks
make doc             # Build documentation
make clean           # Remove build artifacts
make deny            # Run license/advisory audit
```

## Architecture

```
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
- **Client/server**: Single binary, daemon auto-starts on first use
- **Single writer, many readers**: Mutations via mpsc → single state-owner task; reads via ArcSwap snapshots
- **CLI == API**: Every `shux` subcommand is a thin JSON-RPC call
- **Events as integration surface**: typed, sequenced, via tokio::sync::broadcast

## Code Conventions

- **Format:** `rustfmt` (enforced by CI and pre-commit hook). No debates.
- **Linting:** `clippy` with `-D warnings`. Must pass before commit.
- **Errors:** Use `thiserror` for library errors, `anyhow` for application errors. Wrap with context.
- **No `panic!`** outside test code. Use `Result` everywhere. `unwrap()` only with a comment explaining why it's safe.
- **No `unsafe`** unless absolutely necessary, documented, and justified.
- **Async:** All I/O operations use `tokio`. No blocking in async contexts. Use `tokio::task::spawn_blocking` for CPU-heavy work.
- **Testing:** `#[cfg(test)]` modules in each file. Integration tests in `tests/`. Property tests with `proptest` where applicable.
- **Imports:** `use` statements grouped: std → external crates → workspace crates → local modules. Enforced by `rustfmt`.

## Git Workflow

- **Branch naming:** `feat/`, `fix/`, `refactor/`, `docs/`, `chore/`
- **Commits:** Conventional commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`)
- **PRs:** One feature/fix per PR. Reference task number if applicable.
- **Hooks:** lefthook runs fmt+clippy on pre-commit, full test suite on pre-push.

## Key Decisions

| Decision | Rationale | Date |
|---|---|---|
| Cargo workspace with separate crates | Clean dependency boundaries, parallel compilation, independent testing | 2026-02-18 |
| `rust-toolchain.toml` pins stable | PRD requires stable Rust; pin ensures reproducible builds | 2026-02-18 |
| Hand-rolled JSON-RPC (not jsonrpsee) | jsonrpsee lacks native UDS; hand-rolled matches Zellij's pattern | 2026-02-18 |
| cargo-nextest over `cargo test` | Better output, parallelism, JUnit XML for CI, retry support | 2026-02-18 |
| VecDeque grid (not alacritty_terminal) | alacritty_terminal is too coupled; PRD §15.2 specifies custom grid | 2026-02-18 |
| Fork-before-tokio daemonization | Fork in multi-threaded process is UB; PRD §4.5 specifies this | 2026-02-18 |

## Important API Notes

### Crate Versions (Validated Feb 2026)
- `crossterm` 0.29 — Kitty keyboard, synchronized output, OSC 52
- `vte` 0.15 — with `ansi` feature for typed handler callbacks
- `ratatui` 0.30 — workspace reorganization, used for chrome only
- `wasmtime` 41+ — WASI Preview 2, Component Model, epoch interruption
- `pty-process` 0.5.3 — AsyncRead/AsyncWrite, tokio integration
- `arc-swap` 1.x — lock-free state snapshots
- `clap` 4.x — derive macro, subcommands, completions

### Architecture Patterns
- `SessionGraph` owns all state. ArcSwap for lock-free reads.
- Single-writer mutation channel (tokio::sync::mpsc → state-owner task)
- Event bus: tokio::sync::broadcast + sequence numbers (AtomicU64) + gap detection
- Plugin host: wasmtime Engine + Linker shared; per-plugin Store (dropped on hot reload)

## Learnings

> **STRICT RULE:** This section MUST be updated at the end of every coding session.
> Each entry should be a concrete, actionable insight. Delete entries that become obsolete.

*(No entries yet — will be populated during implementation)*
```

### Step 9: Create AGENTS.md

```markdown
# AGENTS.md — shux

All agent instructions live in **CLAUDE.md**. This file exists solely as a redirect.

See [CLAUDE.md](CLAUDE.md) for: project overview, build commands, architecture, code conventions, git workflow, key decisions, API notes, and learnings.
```

### Step 10: Create .claude/settings.json

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash(git commit.*)",
        "hooks": [
          {
            "type": "command",
            "command": "echo '⚠️  GATE: Verify all tests pass (make check) before committing.'"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo '📝 REMINDER: Update CLAUDE.md Learnings section AND docs/PROGRESS.md session log before stopping.'"
          }
        ]
      }
    ]
  }
}
```

### Step 11: Create GitHub Actions CI

```yaml
name: CI

on:
  push:
    branches: [main, "feat/**", "fix/**", "refactor/**"]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: -Dwarnings

jobs:
  check:
    name: Check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2
      - name: Format check
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings

  test:
    name: Test
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: taiki-e/install-action@nextest
      - name: Run tests
        run: cargo nextest run --workspace
      - name: Doc tests
        run: cargo test --workspace --doc

  deny:
    name: Deny
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
```

### Step 12: Create deny.toml

```toml
# deny.toml — cargo-deny configuration
# Run: cargo deny check

[advisories]
vulnerability = "deny"
unmaintained = "warn"
yanked = "warn"
notice = "warn"

[licenses]
unlicensed = "deny"
allow = [
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "Zlib",
    "BSL-1.0",
    "OpenSSL",
]

[bans]
multiple-versions = "warn"
wildcards = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

### Step 13: Create setup-dev.sh

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "=== shux dev environment setup ==="

# Check Rust
if ! command -v cargo &>/dev/null; then
    echo "❌ Rust not found. Install from https://rustup.rs/"
    exit 1
fi

echo "✓ Rust $(rustc --version | cut -d' ' -f2)"

# Install dev tools
echo "Installing dev tools..."
cargo install cargo-nextest --locked 2>/dev/null || echo "  cargo-nextest already installed"
cargo install cargo-llvm-cov --locked 2>/dev/null || echo "  cargo-llvm-cov already installed"
cargo install cargo-deny --locked 2>/dev/null || echo "  cargo-deny already installed"

# Install lefthook
if ! command -v lefthook &>/dev/null; then
    echo "Installing lefthook..."
    cargo install lefthook --locked 2>/dev/null || npm i -g lefthook
fi
echo "✓ lefthook $(lefthook version 2>/dev/null || echo 'installed')"

# Install git hooks
echo "Installing git hooks..."
lefthook install

# Build to verify
echo "Building..."
cargo build --workspace

echo ""
echo "=== Setup complete! ==="
echo "Run 'make check' to verify everything works."
```

### Step 14: Update .gitignore

Append the following to the existing `.gitignore`:

```
# Coverage
lcov.info
tarpaulin-report.html

# Fuzz
fuzz/artifacts/
fuzz/corpus/

# Lefthook
.lefthook-local/
```

---

## Verification

### Functional

```bash
# Workspace builds without errors
cargo build --workspace 2>&1 | tail -1
# Expected: "Finished ..."

# All crate stubs compile
cargo check --workspace

# Clippy passes
cargo clippy --workspace --all-targets -- -D warnings

# Formatting passes
cargo fmt --all -- --check

# Main binary runs
cargo run -p shux
# Expected: "shux v0.1.0"

# Nextest runs (no tests yet, but should succeed)
cargo nextest run --workspace
# Expected: 0 tests, 0 failures

# Make targets work
make build
make lint
make check
make ci
```

### Tooling

```bash
# lefthook is installed and configured
lefthook run pre-commit

# Cargo deny runs
cargo deny check || echo "deny not required to pass yet — advisory db may need initialization"

# CLAUDE.md exists and is referenced by AGENTS.md
head -1 CLAUDE.md
# Expected: "# CLAUDE.md — shux AI Agent Instructions"

head -1 AGENTS.md
# Expected: "# AGENTS.md — shux"
```

### CI Pipeline

```bash
# Validate CI config syntax (if act is installed)
# act -l
# Otherwise, push to trigger CI on GitHub
```

---

## Completion Criteria

- [ ] `rust-toolchain.toml` pins stable with clippy, rustfmt, rust-src, wasm32-wasip2
- [ ] Cargo workspace compiles with all 7 crates (shux + 6 library crates)
- [ ] `cargo run -p shux` prints version
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo nextest run --workspace` runs successfully (0 tests OK)
- [ ] `Makefile` has all targets: build, release, test, lint, check, ci, bench, doc, install, install-tools, install-hooks, clean, deny, fuzz, fmt
- [ ] `lefthook.yml` has pre-commit (fmt+clippy) and pre-push (test+deny) hooks
- [ ] `lefthook install` succeeds
- [ ] `CLAUDE.md` contains: Project Overview, Build & Test Commands, Architecture, Code Conventions, Git Workflow, Key Decisions, Important API Notes, Learnings
- [ ] `AGENTS.md` redirects to CLAUDE.md
- [ ] `.claude/settings.json` has PreToolUse commit gate and Stop learnings reminder
- [ ] `.github/workflows/ci.yml` has check, test (ubuntu+macos), and deny jobs
- [ ] `deny.toml` configured with allowed licenses
- [ ] `docs/PROGRESS.md` task 000 marked complete
- [ ] `scripts/setup-dev.sh` is executable and works
- [ ] `make ci` (lint + test-lib + test-doc) runs successfully
- [ ] `.claude/automations/` directory exists for iterm2-driver visual test scripts
- [ ] `.claude/automations/screenshots/` directory exists (gitignored) for screenshot output
- [ ] `CLAUDE.md` documents L4 visual testing with iterm2-driver (`uv run .claude/automations/<test>.py`)

---

## Commit Message

```
chore: scaffold repository with Rust workspace, Makefile, lefthook, CI

- Cargo workspace with 7 crates (shux binary + 6 library crates)
- Makefile with build/test/lint/check/ci/bench/doc/install/clean targets
- lefthook pre-commit (fmt+clippy) and pre-push (test) hooks
- GitHub Actions CI (check + test on ubuntu/macos + cargo-deny)
- CLAUDE.md agent instructions, AGENTS.md redirect
- rust-toolchain.toml pinning stable + wasm32-wasip2 target
- cargo-deny configuration for license/advisory audit
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md` and `docs/PRD.md` §15 (Technology choices)
2. **During:** Create files in order (Steps 1–14). Run verification after each step.
3. **After:** Run full verification suite. Update `docs/PROGRESS.md` (mark 000 done, add session log entry). Update `CLAUDE.md` Learnings if anything was discovered.
