# Development

## Setup

```bash
git clone https://github.com/indrasvat/shux.git
cd shux
make install-tools   # nextest, llvm-cov, deny, fuzz, lefthook
make hooks           # install lefthook git hooks
make build           # debug build
```

Requires Rust 1.93+ (stable). Pinned via `rust-toolchain.toml`.

## Make targets

| Target | What it does |
|---|---|
| `make build` | Build all crates (debug) |
| `make release` | Build optimized binary → `target/release/shux` |
| `make test` | Run tests with cargo-nextest |
| `make test-verbose` | Tests with output visible |
| `make lint` | clippy + fmt-check |
| `make check` | lint + test (what pre-commit runs) |
| `make ci` | CI pipeline (lint + test-lib + test-doc) |
| `make fmt` | Format all code |
| `make fmt-check` | Check formatting (no changes) |
| `make deny` | License/advisory audit |
| `make check-progress` | Verify PROGRESS.md is current |
| `make install` | Install to `~/.local/bin/shux` |
| `make hooks` | Install lefthook git hooks |
| `make doc` | Build documentation |
| `make bench` | Run benchmarks |
| `make clean` | Remove build artifacts |

**Always use `make <target>`** instead of running raw `cargo`,
`lefthook`, or scripts. If a task needs a command without a Makefile
target, add one before using it.

## Testing strategy (L1–L4)

| Layer | What | Where |
|---|---|---|
| **L1** unit | Per-module unit tests | `#[cfg(test)] mod tests` in each file |
| **L2** integration | Cross-module integration | `crates/<crate>/tests/*.rs` |
| **L3** end-to-end | CLI binary against a real daemon | `crates/shux/tests/*_integration.rs` |
| **L4** visual | iterm2-driver scripts that drive the actual TUI | `.claude/automations/test_*.py` |

Run all four:

```bash
make test                              # L1 + L2 + L3
uv run .claude/automations/test_*.py   # L4 (per script)
```

L4 scripts live in `.claude/automations/` and use the shared
`_shux_iterm.py` helpers (janitor, own window, position-based Quartz
screenshots, multi-level cleanup). Screenshots go to
`.claude/screenshots/` (gitignored).

## Conventional commits

```
feat(scope):     new feature
fix(scope):      bug fix
refactor(scope): non-behavioral change
test(scope):     test changes only
docs(scope):     documentation only
chore(scope):    tooling, deps, CI
```

Reference task numbers when applicable (`feat(017): ...`).

## Branch hygiene

- Default branch is `main`. Branch protection blocks force-push and
  deletion; CI checks (`Check`, `Deny`, both `Test`s) must pass before merge.
- Feature work happens on `feat/<slug>` / `fix/<slug>` / etc.
- Squash-merge into `main`. Tag releases on `main` only.

## Pre-commit / pre-push hooks

`make hooks` installs lefthook. Two stages:

- **pre-commit**: `make lint` (clippy + fmt-check) and `make check-progress`.
- **pre-push**: `make ci` (lint + test + test-doc).

Failures block the commit/push. The progress check ensures
`docs/PROGRESS.md` is updated when source changes.

## Toolchain skew (gotcha)

CI uses `dtolnay/rust-toolchain@stable` which tracks the latest stable
release. `rust-toolchain.toml` pins **1.93**, so local runs older. New
clippy lints introduced after 1.93 (e.g. `unnecessary_sort_by` in 1.95)
won't fire locally but DO fail CI.

If you hit a CI clippy failure that didn't reproduce locally, install the
latest stable and re-run:

```bash
rustup install stable
cargo +stable clippy --workspace --all-targets -- -D warnings
```

## Session protocol

Each task in `docs/tasks/` has a `Status:` field:

```
**Status:** Pending | In Progress | Done
```

When starting a task: set status to `In Progress` in both the task file AND
`docs/PROGRESS.md`. When finishing: flip to `Done`, add a session-log entry
in `docs/PROGRESS.md`, and update `CLAUDE.md` Learnings if anything was
discovered.

The `make check-progress` target enforces this; pre-push hook blocks
inconsistent state.

## Logging

```bash
RUST_LOG=debug shux ls      # verbose logs to stderr
shux -v ls                  # convenience flag
```

Daemon logs currently go to /dev/null because of double-fork. M3 will
land file-based daemon logs (`$RUNTIME_DIR/daemon.log`).

## Useful one-liners

```bash
# Wipe daemon state and start fresh
pkill -f 'shux.*__daemon' && rm -rf /tmp/shux-$UID

# See the raw RPC for any command
RUST_LOG=trace shux ls 2>&1 | grep rpc

# Run a specific test
cargo nextest run -p shux test_session_create

# Run a single L4 visual
uv run .claude/automations/test_017_full_verify.py
```

## Code conventions

See `CLAUDE.md` for the full set. Highlights:

- **rustfmt** is non-optional. Hooks enforce it.
- **clippy with `-D warnings`** must pass before commit.
- **`thiserror`** for library errors, **`anyhow`** for application errors.
- **No `panic!`** outside test code. **`unwrap()`** only with a comment
  explaining safety.
- **No `unsafe`** unless documented and justified.
- **All I/O via `tokio`**; no blocking in async contexts. Use
  `tokio::task::spawn_blocking` for CPU-heavy work.
- **CLI output styling** must use `crates/shux/src/style.rs` helpers.
  Never raw `println!` for styled output.

## Where to look first when contributing

| Want to change... | Start here |
|---|---|
| The TUI rendering | `crates/shux-ui/src/compositor.rs` |
| Pane layout / split / focus | `crates/shux-core/src/layout.rs` |
| What a CLI subcommand does | `crates/shux/src/cli.rs` |
| RPC method handlers | `crates/shux/src/main.rs::register_*_methods` |
| Status bar / config | `crates/shux/src/statusbar_runner.rs` + `crates/shux-core/src/config.rs` |
| The attach protocol | `crates/shux-rpc/src/attach.rs` (types) + `crates/shux/src/attach.rs` (daemon) + `crates/shux-ui/src/attach.rs` (client) |
| Tests | `crates/<crate>/tests/` for L2/L3, `.claude/automations/` for L4 |
