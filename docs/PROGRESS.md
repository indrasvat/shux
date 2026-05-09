# shux — Implementation Progress

> **STRICT RULE:** This file MUST be updated at the end of every coding session.

## Current Phase

**M0: Architecture Spike** — **Complete**. M1: tasks 013–017 + 060 done. shux is now a working interactive multiplexer end-to-end (multi-pane render + attach client + Tier-1 keybindings + status bar). 576 tests pass.

## Status

### Milestone Targets

- [ ] **M0: Architecture Spike** (tasks 001–012)
  - [ ] Daemon skeleton with fork-before-tokio daemonization
  - [ ] PTY manager with async I/O
  - [ ] Virtual terminal grid (vte + VecDeque)
  - [ ] Minimal TUI client (single pane)
  - [ ] JSON-RPC server on UDS (system.version, system.health, session.list)
  - [ ] Basic input decoder (legacy + Kitty)
  - [ ] `shux` binary with `new`, `attach`, `ls`
  - [ ] L1 + L2 tests passing

- [ ] **M1: Daily-Driver Core** (tasks 013–034)
  - [ ] Full session/window/pane CRUD (API + CLI)
  - [ ] Splits, directional focus, resize, zoom, swap
  - [ ] Copy mode with clipboard
  - [ ] Graded keybindings (Tier 1 + 2), command palette, help overlay
  - [ ] TOML config with live reload
  - [ ] Theme engine with per-pane theming
  - [ ] Mouse support, pane titles, status bar
  - [ ] Session templates
  - [ ] L1–L4 tests passing
  - [ ] Dogfooding begins

- [ ] **M2: API + Plugin System** (tasks 035–052)
  - [ ] Complete JSON-RPC API surface
  - [ ] Event stream with filters and sequence numbers
  - [ ] Plugin host (Wasm + process plugins)
  - [ ] Event interception, command override, overlays
  - [ ] Bundled plugins (status-bar, theme-pack, diagnostics)
  - [ ] gRPC API (optional)
  - [ ] L1–L6 tests passing

- [ ] **M3: Polish, Performance, Docs** (tasks 053–059)
  - [ ] All P0 performance budgets met
  - [ ] Shell completions, image passthrough
  - [ ] Fuzzing campaigns (ANSI, JSON-RPC, config, layout)
  - [ ] Documentation (README, guides, API reference)
  - [ ] Binary releases (macOS + Linux)
  - [ ] v1.0 release

---

## Session Log

**2026-05-09 — Spike followups: inline starship_config + `shux config init`**
- Single-file UX: collapsed the spike's two-file layout (config.toml + statusbar.toml) into ONE. New `SegmentDef::starship_config: Option<String>` accepts an inline TOML literal multi-line string (`'''...'''`). At runner startup the daemon materialises it to `$TMPDIR/shux-segment-<idx>.toml` and exports `STARSHIP_CONFIG=<that path>` for the spawn. On runner exit (config reload or daemon shutdown) the tempfile is removed.
- Critical TOML detail: the OUTER field MUST be triple-single-quoted (TOML literal) so escapes pass through verbatim. Triple-double-quote decodes `\"` → `"` mid-flight and corrupts the inner TOML — starship's parser rejects the materialised file.
- New CLI: `shux config init` writes a single starter `~/.config/shux/config.toml` with an inline starship segment that uses Catppuccin Macchiato colors. Also `shux config show` (prints the canonical defaults to stdout) and `shux config path` (effective path). The init prints a bashrc snippet that guards `eval "$(starship init bash)"` on `[[ -z $SHUX ]]`, so the user's shell PS1 stays starship-rich outside shux but goes bare (`❯ `) inside, removing the "two starships at once" duplication.
- Visual proof — `.claude/automations/test_017_statusbar_custom.py`: writes a config with custom load+IP modules in inline starship_config, attaches, screenshots. Result: `◆ sbcustom  [1/1] 1   00:52:39   load 3.95   ip 10.0.0.31   00:52:38` rendered live in the bar with full Catppuccin colors. The user's default starship continues to render at the top of the pane (their `~/.config/starship.toml`), unaffected. Distinct configs, distinct outputs, single shux config file.
- 7/7 PASS in `test_017_statusbar_segments.py` after fixes (added S6 = "inline config drives the bar, not user PS1 config" using a `${custom.sentinel}` marker).
- Learning: starship custom modules require `when = true` (boolean) — not `when = 'true'` (string). The string form invokes `true` as a shell command which usually exits 0 too, but quoting in the inner-TOML can break it. Boolean is unambiguous.
- Learning: starship custom modules use `${custom.<name>}` reference syntax (with curly braces and a dot), NOT `$custom_<name>`. The latter silently produces no output and gives no error.
- Learning: starship's default `command_timeout` is 500ms — `top -l 1` blows past that. Use `command_timeout = 2000` in the inline config, or use instant alternatives like `sysctl -n vm.loadavg` for CPU load.
- Learning: `std::env::temp_dir()` returns `$TMPDIR` (`/var/folders/...`) on macOS, not `/tmp`. Tests that grep `/tmp/shux-segment-*` find nothing; use `$TMPDIR` or pass an explicit path.
- 590 tests pass total (was 586). Lint clean.

**2026-05-09 — Spike: script-driven status-bar segments (starship + fallback)**
- Goal: validate that we can ship "drop-in starship in the status bar" without porting starship's formatter as a Rust SDK (starship is a `[[bin]]`-only crate; no library API). Result: it works, OOTB, with a real safety net.
- Schema: extended `[statusbar]` with a `[[statusbar.segment]]` array. Each entry has `zone` (left/center/right), `command: Vec<String>`, optional `env: HashMap<String,String>`, `interval_ms` (clamped to ≥100ms; default 2000), and `fallback: Option<String>`. Lets users wire `command = ["starship", "prompt"]` with `STARSHIP_CONFIG = "..."` to get a separate prompt config for the bar without touching their PS1.
- Runner: new `crates/shux/src/statusbar_runner.rs`. `spawn_segment_runners` runs once per daemon, spawns a child task per segment, restarts the whole group on `ConfigHandle::change_notify()` (same hot-reload primitive that drives border-style swaps). Each child task runs the command on its interval (1s timeout per spawn so a hung script can't starve the bar), captures stdout, stashes into `SegmentCache (Arc<RwLock<HashMap<usize, Vec<u8>>>>)`. Failure → fallback bytes → still rendered.
- ANSI → segments: `ansi_to_segments(bytes)` feeds the captured output through a 6-row × 512-col `VirtualTerminal`, scans the first non-blank row, groups runs of cells by (fg/bg/bold) into `StatusSegment`s. Multi-row VT specifically because starship's default prompt is two-line — a 1-row VT would scroll on `\n` and lose the meaningful first line.
- Wiring: `attach::run_render_loop` calls `populate_bar(&mut bar, &config, &segments)` after the existing `build_status_bar`. Built-in segments still anchor the bar (so a missing/empty config never produces a blank bar); script segments append into their declared zone.
- Visual + perf verification (`.claude/automations/test_017_statusbar_segments.py`): 6/6 PASS, 0 leaked tabs.
  - S1 OOTB: built-in bar shows when no segments configured.
  - S2 starship segment renders in the bar at the bottom of screen with full ANSI colors (verified via Quartz screenshot — capsules/git-branch/Rust+Python versions/clock all visible alongside the built-in segments).
  - S3 missing-binary (`this-binary-does-not-exist-shux`) → daemon spawns repeatedly without crashing, fallback text `[no-bin] FALLBACK-OK` appears in the bar. Daemon PID still alive after the failed-spawn loop.
  - S4 hot-add: empty config → write a segment via filesystem → `HOT-ADDED-$RANDOM` lights up live, no restart.
  - S5 perf: 1Hz `starship prompt` segment, 5s sample of daemon `%CPU` via `ps`. **avg 0.1%** (samples 0.1, 0.1, 0.1, 0.1, 0.1). Way under the 5% threshold.
- Unit tests (4 new in `statusbar_runner::tests`): SGR red → red segment; mixed RED/GREEN groups by style; empty input; trailing newline stripping.
- 586 tests pass total (was 582).
- Open caveats from the spike (next-iteration TODOs, not blockers):
  - Script segments today *append* into a zone alongside the built-in (e.g. clock + starship both end up in `right`). Probably want a per-segment "replace this zone instead of append" knob, OR drop the built-in segments for a zone when any user segment targets it.
  - Each spawn fork-execs starship. At 1Hz it's 0.1% CPU; at 5Hz × 5 segments it'd be ~5%. M2 plugin host is the long-lived answer; for now `interval_ms ≥ 1000` recommended.
  - VT width is 512 cols. Wider configs would need either a wider VT or a layout-aware sizing pass.
  - No `shux config init` yet — users still hand-write the TOML.
- Learning: `tokio::sync::Notify::notify_waiters()` only wakes tasks that are *currently* `.notified().await`-ing. The runner's loop creates the listener BEFORE awaiting, but if a notify lands between the `current()` read and `notified()`, it's dropped. The fact that the spike works in practice is because `notify_waiters` is called by the watcher AFTER the runner is already in `select!`. For a tighter race story: switch to `tokio::sync::watch::channel` (every receiver gets every change, no permit semantics).
- Learning: starship outputs multi-line by default. Render the captured bytes into an N-row VT (not 1-row) and scan the first non-blank row, otherwise the `\n` at the end of starship's status line scrolls the meaningful content off and you get just the chevron `❯`.
- Learning: when a bash subshell inside a pane has its own starship init from `~/.bashrc`, expect TWO starship prompts on screen — one inside the pane (user's PS1) and one in the multiplexer's status bar (from the `[[statusbar.segment]]` runner). They render independently. Tests that just grep "indrasvat-shux" can mistake one for the other.

**2026-05-08 — M1 follow-up suite: CLI passthrough, mouse, TOML config + hot reload**
- **CLI passthrough**: `shux new -s X -- python3 -c "..."` (or `-- vim foo.rs`, etc.) exec's the trailing argv directly in the pane instead of dropping into a shell. Wired through clap (`#[arg(last = true)]` on `New::argv`), session.create / session.ensure RPC `command` param (accepts string or array), and `PtyConfig::with_command`. `spawn_pane_pty` signature now takes `command: Vec<String>`.
- **Mouse support**: Forwarded crossterm `Event::Mouse` as `AttachClientFrame::Mouse { kind, button, col, row }`. Daemon: `pane_at(col, row)` for click-to-focus; `border_at()` detects clicks on a pane separator and arms a `DragState` so subsequent `Drag` events translate cursor deltas into `ResizePane` calls. Scroll variants reserved for task 021 (copy mode).
- **TOML config + hot reload**: New `shux-core::config` module — `Config`/`ConfigHandle` with lock-free `ArcSwap` snapshots and a `Notify` for change events. Loaded from `$XDG_CONFIG_HOME/shux/config.toml` or `$HOME/.config/shux/config.toml`. Sections: `[appearance][keys][shell][statusbar]`. `run_hot_reload()` watches the parent dir via `notify` (parent because editors atomic-rename), debounces 150ms, re-parses, atomically swaps the live snapshot, and fires the change Notify. The attach render loop awaits both the data pulse and the config Notify; changes land on the very next frame. `RenderCompositor::set_border_style()` for the live appearance swap.
- **iterm2-driver migration**: Refactored `test_017_attach_multipane.py` and `test_017_real_apps.py` to use the shared helpers in `_shux_iterm.py` (janitor at start, own window via `iterm2.Window.async_create()` with refresh, position-based Quartz screenshot correlation, multi-level `try/finally` cleanup). Verified zero leaked tabs.
- **Comprehensive visual verification suite** (`.claude/automations/test_017_full_verify.py`): 9/9 PASS, 0 FAIL.
  - V1 splits don't overdraw pane content (LEFT-MARK + RIGHT-MARK both visible)
  - V2 color isolation (red text at col 71, green at col 1, no bleed)
  - V3 prefix bindings fire (3-pane layout = 106 │ chars; zoom collapses to 0; unzoom returns)
  - V4 CLI passthrough — `shux new -- python3 -c "..."` exec'd directly
  - V5 mouse click-to-focus (synthetic click at col 6 → CLICK-MARK appears at col 8 in left pane)
  - V6 config hot reload — rounded → thick → ascii observed live (no restart)
  - V7 broken config doesn't crash daemon
- **Tests**: 582 pass (was 577) — 5 new config tests, including a real hot-reload test that writes a file and waits for the watcher to land the new value within 2s.
- **Build artifacts**: `notify = "8"` added to workspace deps; `tempfile` to shux-core dev-deps.
- Learning: `notify` crate watcher should bind to the **parent directory**, not the file directly — many editors write to a tempfile and atomic-rename, which the file watch misses.
- Learning: `iterm2.async_set_fullscreen(True)` causes `screencapture -l <window-id>` to fail because macOS native fullscreen creates a new Space and changes the window ID. Switching to `screencapture -x -D 1` (whole main display) is the simple fix; position-based Quartz correlation is preferred for non-fullscreen captures.
- Learning: For `pane capture`, request `--lines = $rows_of_pane` rather than a small number, otherwise output that lives near row 0 (cursor at top of grid) gets missed and the response looks empty.
- Learning: `bash -l -i` + `TERM_PROGRAM=shux` are necessary together to load user dotfiles correctly. `-l` alone misses `~/.bashrc`; without `TERM_PROGRAM=shux` user rc files keep branching on the parent emulator's value (e.g. skip starship under Warp).

**2026-05-08 — Followup: user dotfiles (starship) load inside shux panes**
- Root cause: `crates/shux-pty/src/handle.rs::resolve_command` spawned the shell as `bash -l` (login only). User's `~/.bash_profile` sources `~/.bashrc` only when `$- == *i*` (interactive); `-l` alone leaves `$-` without `i`, so `~/.bashrc` never ran and starship never initialized.
- Secondary cause: even with bashrc running, `TERM_PROGRAM` was inherited from the parent emulator (e.g. `WarpTerminal`). User rc files commonly branch on `TERM_PROGRAM` to skip starship under Warp; that branch fired wrong inside shux panes.
- Fix: spawn `<shell> -l -i` (login + interactive). Override `TERM_PROGRAM=shux` and `TERM_PROGRAM_VERSION=<pkg ver>` so user rc files see shux as the host emulator. Also set `SHUX=1` (mirrors tmux's `TMUX` env var, lets users guard config) and `COLORTERM=truecolor`.
- Verified end-to-end via `.claude/automations/test_017_starship.py`: starship prompt with username, path, git branch (with dirty/untracked indicators), Rust + Python version, clock all render correctly inside a shux pane.
- iterm2-driver best-practices applied to all task-017 automation scripts:
  - Janitor at start (`cleanup_stale_windows`, prefix=`shux-auto-`) closes orphan windows from crashed prior runs.
  - Per-script isolated window via `iterm2.Window.async_create()` with stale-object refresh + readiness probe (the #1 iterm2 automation pitfall).
  - Position-based Quartz screenshot correlation (no focus required, no whole-display capture).
  - Multi-level try/finally cleanup so the script's window always closes, even on exception.
  - `\n` instead of `\r` for shell command submit — bypasses readline / ble.sh keymap, avoiding multiline-mode traps in user-customized shells.
- Shared helpers landed at `.claude/automations/_shux_iterm.py` so future tests don't reinvent the patterns.
- Learning: `bash -l -i` for spawning users' configured shells, mirrors what iTerm2 does. `bash -l` alone is the silent killer of `~/.bashrc` integrations (starship, atuin, ble.sh).
- Learning: `TERM_PROGRAM` inheritance from the parent emulator silently mis-routes user rc-file branches; multiplexers should claim their own value (tmux sets `TERM_PROGRAM=tmux`, shux now sets `TERM_PROGRAM=shux`).
- Learning: `\r` (CR) gets remapped by readline replacements like ble.sh into "insert-newline" within a multiline edit; `\n` (LF) bypasses the readline keymap entirely and is more reliable for automation.

**2026-05-08 — Task 017: Multi-Pane Rendering + Attach Client — Done**
- shux is now a working interactive multiplexer. The `shux attach` / `shux` / `shux new` (no `--detached`) commands launch a real TUI; the daemon owns rendering, the attach client is a thin keystrokes-up / ANSI-down pipe.
- shux-ui: new `borders.rs` (BorderStyle: thin/thick/double/rounded/ascii/none + compute_borders with corner/T/cross resolution), `statusbar.rs` (3-zone left/center/right), extended `compositor.rs` with `render_multi_pane()` (layout-aware, diff-based, zoom mode, status bar, focused border, inset pane viewport so outline never overdraws pane content), client `attach.rs` (handshake → run_loop with crossterm event polling thread → forwards keys as Input frames, dumps Render frames to stdout)
- shux-rpc: new `attach.rs` defining `AttachHello`/`AttachReady`/`AttachServerFrame`/`AttachClientFrame` length-prefix-framed JSON protocol with base64-encoded ANSI binary payloads. Reuses existing codec
- shux (binary): new `attach.rs` daemon-side session handler — owns one RenderCompositor per attached client, watches PaneIoState, ships ANSI bytes as Render frames at 200ms cadence + on render_pulse notify. Dispatches Action frames to GraphHandle. Pinger task detects dead peers. Hello handshake bounded by 5s timeout
- main.rs: PaneIoState gains `resizers` (mpsc<PtySize> per pane) and `render_pulse` (tokio Notify); per-pane PTY task gains a third `select!` branch for resize → TIOCSWINSZ + VT resize. handle.kill() called on PTY task exit to reap zombie shells
- Two rounds of brutal codex+gemini council reviews surfaced and fixed: mutex-held-across-await deadlocks, every-pane-gets-client-size (not its rect), `current_size_for_session` infinite shrink loop, `notify_waiters` lost wakeups (now `notify_one`), hardcoded 120x40 viewport for spatial actions, prefix swallowing unbound keys, `key_to_prefix_action` ignoring modifiers (Ctrl+C → NewWindow!), focus while zoomed routing to hidden pane (both directional + relative), no PTY winsize update on layout changes, `Some(d) = recv()` silently disabling channel branches, prefix-prefix not forwarding literal prefix to PTY, send().await blocking the whole attach on backpressure, no hello timeout (slowloris), borders overdrawing on tiny terminals, cursor hidden at right edge, multi-row status bar duplicating
- L4 visual tests via iterm2-driver (`test_017_attach_multipane.py`): 13/16 pass, 0 failures. Verified end-to-end: shux attach starts TUI, status bar shows session name + clock, borders draw with rounded corners, vertical/horizontal splits work via Ctrl+Space + |/-, zoom (Ctrl+Space z) collapses splits and unzoom restores, two different commands run side-by-side in two panes, send-keys via API forwards to attached client, detach via Ctrl+Space d returns to shell. Screenshots in `.claude/screenshots/017_*.png`.
- 576 tests pass (was 567 before — added 17 unit tests in borders/statusbar/attach + 7 integration tests for multi-pane compositor)
- Learning: `tokio::sync::Notify` — `notify_waiters()` only wakes tasks currently awaiting `.notified()`; if the renderer is mid-CPU it loses the wakeup. Use `notify_one()` which queues a permit consumed by the next `.notified().await`.
- Learning: `Some(x) = recv()` in `tokio::select!` is a refutable pattern that silently disables the branch when None comes through; you cannot detect channel close that way. Use `res = recv() => match res { Some(x) => ..., None => break }`.
- Learning: Multi-pane multiplexer must size each pane's PTY to its layout rect, NOT the full client size. apps polling TIOCGWINSZ otherwise lay themselves out for the whole screen. The compositor crops the oversized VT and TUIs render wrong.
- Learning: Inferring client size from a pane's VT grid creates a self-feeding shrink loop: pane is half-width → grid says 40 cols → resize compositor to 40 → pane is now 18 cols, etc. Track client size as authoritative state, never derive it from the very thing it sizes.
- Learning: Layout actions (split, zoom, kill) change pane rects; the daemon must re-fan winsize to all PTYs in the active window after every such action so vim/htop/less inside the panes redraw correctly.
- Learning: Holding the global PaneIoState mutex across `.await` on bounded channel sends can deadlock the entire session if any single PTY task gets slow. Pattern: `let tx = { state.lock().writers.get(&p).cloned() }; tx.send(...).await`.
- Learning: For interactive input forwarding, `try_send` is right; `send().await` lets one stuck pane freeze the whole client (user can't even detach). Drop the keystroke instead.

**2026-02-19 — Task 016: Pane I/O (send_keys, run_command, capture) — Done**
- Created `crates/shux-pty/src/command.rs`: `CommandEngine` with marker technique for detecting command completion — `start_command()` generates PTY command with `SHUX_MARKER{marker}EXIT{$?}SHUX_END` pattern, `process_output()` scans per-pane output buffers for markers (handles split-across-chunks), `check_timeouts()`, `cancel_command()`, `get_status()`, `shell_escape_args()` — 13 unit tests
- Created `crates/shux-pty/src/capture.rs`: `strip_ansi()` removing CSI, OSC, DCS, 8-bit CSI, character set designation sequences — 5 unit tests
- Added `capture_text(lines)` to `VirtualTerminal` (shux-vt): iterates last N visible rows, extracts cell chars (skipping wide continuations), trims trailing whitespace and empty lines — 2 unit tests
- Wired PTY/VT subsystems into daemon (`crates/shux/src/main.rs`): `PaneIoState` (shared writers map + VT map + CommandEngine), `run_pane_pty_task()` (per-pane async task with select! for concurrent read/write), `spawn_pane_pty()` (spawn shell + VT + read/write task). Updated all `register_*_methods()` to spawn/cleanup PTY on pane create/kill
- Registered 5 new pane I/O RPC methods: `pane.send_keys` (text or base64), `pane.run_command` (sync with marker detection + oneshot, or async), `pane.command_status`, `pane.command_cancel` (Ctrl-C + engine cancel), `pane.capture` (VT capture + strip_ansi)
- Added 3 CLI subcommands: `pane send-keys` (-t text/--data base64), `pane run` (command + args, --timeout, --async), `pane capture` (--lines N)
- Added style helpers: `print_send_keys()`, `print_run_command()` with state-colored output
- Created `crates/shux/tests/pane_io_integration.rs`: 9 integration tests with real PTY processes — send_keys text/base64, nonexistent pane error, capture after echo, run_command sync (echo/false), async+poll, cancel, capture with default lines
- Fixed marker echo bug: shell's PTY input echo contains the literal marker command text, which falsely matches the marker detector before the actual output. Split the echo string (`"SHUX_MAR""KER..."`) so input echo never contains the full pattern
- Added `runtime_ms: u64` to `PaneCommandCompleted` event variant
- 546 tests passing (510 existing + 20 command/capture unit + 7 pane_io integration + 9 event), all make targets pass
- Learning: PTY input echo contains the literal typed command — marker detection must ensure the echo text can't match the marker pattern. Splitting the shell string (`"SHUX_MAR""KER..."`) breaks the echo while shell concatenation produces the correct output.
- Learning: Channel-based PTY write architecture (mpsc sender per pane, tokio task owns PtyHandle with `select!` for read/write) avoids ownership conflicts between `PtyManager::write(&mut self)` and the read loop.

**2026-02-19 — Task 060: Rich CLI Output — Beautiful List Commands — Done**
- Rewrote `crates/shux/src/style.rs` (~1078 lines): added `TerminalContext` (auto-detect TTY, colors, unicode, width), `OutputFormat` (Text/Json/Plain), `BoxRenderer` (Unicode box-drawing frames ╭─╮│╰─╯ with ASCII fallback), `ColumnLayout` (column alignment engine), `SessionInfo`/`WindowInfo`/`PaneInfo` data structs
- Added `render_session_list()`, `render_window_list()`, `render_pane_list()`: box-framed tabular output with `short_id()` (8-char), active markers (filled diamond `◆` cyan, open diamond `◇` dim, arrow `◀ active`/`◀ focus [zoomed]`), summary footers ("3 sessions · 5 windows total"), context headers ("Windows ── session: alpha")
- Added `render_empty_state()`: box-framed empty state with hint text ("(no sessions)" + "Create one: shux new -s my-project")
- Changed all confirmation messages to `✓` prefix with short IDs: `print_success("Created", ...)`, `print_error` now uses `✗` prefix
- Updated `crates/shux/src/cli.rs`: added `Plain` variant to `OutputFormat`, `to_style_format()` converter, `format_created_at()` helper, rewrote `handle_ls`/`handle_window_list`/`handle_pane_list` to use batch renderers
- Auto-detection: piped stdout → Plain format (tab-separated, no box, no color), `NO_COLOR` → box preserved but no ANSI codes, `TERM=dumb` → Plain
- Updated `cli_integration.rs` test assertion for empty session list (piped output is empty in Plain format)
- Created `.claude/automations/test_060_rich_cli_output.py`: 44 visual tests across 13 parts (A–M) covering box frames, column alignment, active markers, short IDs, empty state, zoom state, confirmations, errors, plain format, piped auto-detect, NO_COLOR, multi-session stress, JSON cross-check — ~30 screenshots
- Zero new crate dependencies — all hand-rolled (BoxRenderer ~120 lines, ColumnLayout ~90 lines)
- 510 tests passing, all make targets pass (lint + test)

**2026-02-19 — Task 014: Window CRUD (API + CLI) — Done**
- Added window mutation methods to `SessionGraph` (graph.rs): `create_window`, `destroy_window`, `rename_window`, `focus_window`, `reorder_window` with new `GraphCommand` variants and `GraphError` variants (`WindowNameConflict`, `EmptyWindowName`, `WindowIndexOutOfRange`, `LastWindow`)
- Registered 7 window RPC methods in binary crate (main.rs): `window.list`, `window.create`, `window.kill`, `window.rename`, `window.focus`, `window.reorder`, `window.ensure` — all backed by GraphHandle closures with consistent error mapping via `graph_error_to_rpc()`
- Added `WindowCommand` enum (6 sub-subcommands) to CLI with `Window` variant (alias "win"): List, New, Kill, Rename, Focus, Reorder — each with session name → UUID resolution via `resolve_session_id()` and window spec → UUID resolution via `resolve_window_id()`
- Added 6 style helpers: `print_window_entry`, `print_window_created`, `print_window_killed`, `print_window_renamed`, `print_window_focused`, `print_window_reordered`
- Improved `rpc_display()` in CLI to show human-readable error messages (extracting detail/name/id from RPC data fields) instead of raw "RPC error -32NNN: code_name"
- Added 14 window integration tests (m0_integration.rs): create, auto-name, list, list-missing-session, kill, kill-last-fails, rename, focus, reorder, ensure, new-becomes-active, 3 CLI tests
- Created `.claude/automations/test_014_window_crud.py`: L4 visual test with 25 tests (Parts A–H: setup, creation, auto-naming, focus, rename, reorder, kill, JSON output), 21 screenshots — all passing
- 489 tests passing (458 existing + 38 graph unit + 14 integration + 9 CLI parse - some overlap), all make targets pass
- **Spike fix: stale daemon version handshake** — `ensure_daemon_running_at()` now calls `system.version` after connecting and compares against `env!("CARGO_PKG_VERSION")`. On mismatch, kills old daemon via SIGTERM (PID file), waits for exit, spawns fresh daemon. Prevents `method_not_found` errors after rebuilds.
- Added `build.rs` to both `shux` and `shux-rpc` crates to capture `git rev-parse --short HEAD` at compile time as `SHUX_GIT_SHA` env var. Version handshake now compares both `CARGO_PKG_VERSION` and `SHUX_GIT_SHA` — catches stale daemons even within the same version (e.g., after code changes without version bump).
- Updated `system.version` RPC to include `git_sha` field. `shux version` now displays `shux 0.1.0 (abc1234)`.
- Created `.claude/automations/test_014_version_handshake.py`: 13-test E2E verification of version handshake — builds v1, bumps to 0.1.99, rebuilds, verifies auto-restart (PID changes), verifies git_sha in response, verifies same-version doesn't restart.
- Learning: Improved `rpc_display()` that extracts human-readable messages from RPC error data fields (detail, name+resource, id+resource) makes CLI errors much more user-friendly

**2026-02-19 — Task 013: Session CRUD (API + CLI) — Done**
- Added `NameConflict` error code (-32007) to `shux-rpc` error types with convenience constructor
- Added session name validation to `SessionGraph`: non-empty, max 128 chars, alphanumeric + hyphens + underscores + dots. New `GraphError` variants: `EmptySessionName`, `SessionNameTooLong`, `InvalidSessionName`
- Created `graph_error_to_rpc()` helper mapping `GraphError` → `RpcError` with proper error codes: `SessionNotFound` → `NotFound`, `SessionNameExists` → `NameConflict`, validation errors → `InvalidParams`
- Created `session_to_json()` helper building consistent JSON responses with `window_count`, `active_window_id`, `window_id`, `pane_id` fields
- Enhanced `session.list`: sorted by `created_at`, includes `window_count` and `active_window_id`
- Enhanced `session.create`: returns `window_id` and `pane_id`, auto-generates `session-N` names when no name provided
- Enhanced `session.kill`: accepts `{name: ".."}` OR `{id: "uuid.."}` — tries UUID parse first, falls back to name lookup
- Added `session.rename` RPC method: accepts name or id, resolves to session_id, validates new_name, returns updated session
- Added `Rename` CLI subcommand (`shux rename -s <old> -n <new>`) with `handle_rename()` and `print_session_renamed()` style helper
- Added `FromStr` implementation to `define_id!` macro for UUID parsing in model.rs
- Updated `register_session_methods()` in both test files (`m0_integration.rs`, `cli_integration.rs`) with all 5 methods and proper error mapping
- Created `.claude/automations/test_013_session_crud.py`: L4 visual test with 20 tests (Parts A–F: creation, listing, ensure, rename, kill, error handling), 17 screenshots
- 458 tests passing (437 existing + 5 graph validation unit + 14 integration + 2 CLI parse), all make targets pass
- Learning: `graph_error_to_rpc()` centralizes error mapping — keeps RPC handlers clean and ensures consistent error codes across all session methods
- Learning: Auto-generated session names use `session-N` pattern where N is the count of existing sessions (simple, predictable, avoids conflicts)

**2026-02-19 — Task 012: M0 Integration and Quality Gate — Done**
- Wired RPC Server + SessionGraph into daemon (`crates/shux/src/main.rs`): replaced bare `UnixListener::bind` stub with real `run_rpc_server()` that creates SessionGraph + graph loop + RPC Server
- Added `register_session_methods()`: registers `session.list`, `session.create`, `session.kill`, `session.ensure` backed by GraphHandle closures — lives in binary crate since shux-rpc intentionally doesn't depend on shux-core
- Removed `session.list` stub from `shux_rpc::server::register_builtin_methods()` — session methods now registered at binary level
- Updated `crates/shux/tests/cli_integration.rs`: `start_test_server()` now creates SessionGraph + graph loop, all 17 existing tests continue to pass with real data
- Created `crates/shux/tests/m0_integration.rs`: 17 new M0 integration tests — 10 RPC tests (system.version, system.health, create/list/kill/ensure session, detach-reattach, multiple sessions, invalid method, concurrent connections), 2 PTY tests (spawn echo, exit status), 5 CLI binary tests (version json, ls, new detached, kill, ls json)
- Created `scripts/bench-baseline.sh`: performance baseline script measuring binary size, test count, make target verification; outputs to `docs/m0-baseline.txt`
- Added `bench-baseline` Makefile target
- Created `.claude/automations/test_012_m0_integration.py`: L4 visual test exercising CLI smoke tests (build, new detached, ls, api version, kill, list after kill) with screenshots
- 437 tests passing (420 existing + 17 new M0 integration), all make targets pass (build, test, lint, check)
- **M0 Architecture Spike complete:** all 13 tasks (000–012) done, daemon + SessionGraph + RPC + CLI + PTY + VT + compositor + input + event bus all wired and integration-tested
- Learning: Edition 2024 disallows `unwrap_or(&vec![])` — the temporary `vec![]` is freed while still borrowed. Use `.cloned().unwrap_or_default()` instead.
- Learning: Session RPC methods must be registered in the binary crate (not shux-rpc) because they need GraphHandle from shux-core, and shux-rpc intentionally has no dependency on shux-core. The `register_session_methods()` function is duplicated in main.rs and test files (acceptable since binary crates aren't importable).

**2026-02-19 — Task 011: CLI Foundation (clap) — Done**
- Created `crates/shux/src/cli.rs`: Cli struct with clap derive, Command enum (New/Attach/Ls/Kill/Api/Version/__daemon), OutputFormat (Text/Json), RpcClientError, rpc_call() async JSON-RPC client with length-prefix framing, handler functions (handle_ls, handle_new, handle_kill, handle_api, handle_version), custom clap Styles (cyan headers, green commands, yellow placeholders, red errors)
- Created `crates/shux/src/style.rs`: consistent CLI color palette (accent=cyan, success=green, warning=yellow, error=red, muted=dim), respects NO_COLOR convention and IsTerminal check, crossterm Stylize-based Styled helper, print helpers (print_version, print_session_entry, print_no_sessions, print_session_created, print_session_killed, print_error), banner() with figlet "shux" ASCII art and cyan→blue→indigo gradient (256-color codes 51→45→39→33→27)
- Updated `crates/shux/src/main.rs`: real CLI dispatch with CommandFactory+FromArgMatches (for dynamic banner injection), run_daemon() for __daemon subcommand, run_client() with tracing setup + styled error output, dispatch() routing all subcommands, instant version via try_connect() (no daemon auto-start)
- Updated `crates/shux/src/client.rs`: added ensure_daemon_running_at(socket_path) for explicit socket path override, try_connect() for quick probe without auto-start
- Created `crates/shux/tests/cli_integration.rs`: 17 integration tests — 5 in-process RPC tests (version, health, session.list, unknown method, concurrent), 5 CLI binary tests against real RPC server using tokio::process::Command (async), 7 smoke tests (help, version flag, invalid subcommand, kill requires session, list alias, version without daemon, version json without daemon)
- Created `.claude/automations/test_011_cli_styling.py`: L4 visual test with 7 tests (build, help banner, help headers, help commands, version styled, subcommand help, short help) — all passing, 4 screenshots confirming gradient colors and styled output
- Added crossterm, serde, serde_json, uuid to shux crate deps; bytes, futures to dev-deps
- 420 tests passing (37 new: 16 unit CLI parsing + 4 style + 17 integration)
- Learning: tokio::process::Command (async) must be used instead of std::process::Command (blocking) in #[tokio::test] to avoid deadlocking the single-threaded runtime when the test also runs a server task
- Learning: clap's before_help requires CommandFactory+FromArgMatches pattern for dynamic content (banner with terminal detection); the Styles const can use AnsiColor for consistent branded help output

**2026-02-19 — Task 010: Minimal TUI Client — Done**
- Created `crates/shux-ui/src/terminal.rs`: TerminalGuard (RAII raw mode + alt screen + mouse + Kitty keyboard), install_panic_hook (restores terminal before panic), shutdown_signal (SIGTERM/SIGINT)
- Created `crates/shux-ui/src/client.rs`: ClientRequest/DaemonMessage serde types, ClientConfig (prefix key default Ctrl+Space), ExitReason enum, encode_key_event (Ctrl/Alt/arrows/F-keys/nav), parse_key_from_bytes (prefix key detection), parse_resize_event, run_client skeleton (TODOs for daemon wiring in tasks 011/012)
- Created `crates/shux-ui/examples/terminal_demo.rs`: standalone demo exercising TerminalGuard + VirtualTerminal + RenderCompositor + key encoding, with prefix key detach (Ctrl+Space d)
- Created `.claude/automations/test_010_tui_client.py`: L4 visual test with 9 tests (build, alt screen, banner, key echo, enter, arrows, Ctrl+C handling, detach, terminal restore) — all passing
- Updated `crates/shux-ui/Cargo.toml`: added tokio, serde, serde_json, anyhow deps; tempfile dev-dep
- Updated `crates/shux-ui/src/lib.rs`: added client + terminal modules with re-exports
- Updated `docs/tasks/010-minimal-tui-client.md`: added L4 visual testing section
- 41 new tests (2 terminal, 39 client), 383 total passing
- Learning: `parse_key_from_bytes` must handle Enter (0x0d) and Tab (0x09) before the Ctrl range (1..=26), since \r=Ctrl+M and \t=Ctrl+I overlap
- Learning: `enable_raw_mode()` is global (not per-thread), so `spawn_blocking` for crossterm event polling avoids blocking the async runtime

**2026-02-18 — Task 009: Render Compositor (Single Pane) — Done**
- Created `crates/shux-ui/src/buffer.rs`: RenderCell, RenderAttrs, FrameBuffer (double-buffered), DirtyCell, From<&shux_vt::Cell> conversion
- Created `crates/shux-ui/src/render.rs`: RenderBackend<W: Write> with style tracking, render_diff (synchronized output Mode 2026), render_full, clear/hide/show/set_cursor
- Created `crates/shux-ui/src/compositor.rs`: RenderCompositor<W: Write> orchestrating compose->diff->render, CompositorConfig (border, status_bar_height), RenderStats, border rendering with Unicode box-drawing chars
- Created `crates/shux-ui/src/vt_convert.rs`: vt_color_to_crossterm mapping (Default->None, Indexed->AnsiValue, Rgb->Rgb)
- Updated `crates/shux-ui/Cargo.toml`: added shux-vt dependency
- Updated `crates/shux-ui/src/lib.rs`: added buffer, compositor, render, vt_convert modules with re-exports
- 44 new tests (17 buffer, 13 compositor, 11 render, 3 vt_convert), 342 total passing
- Performance: 80x24 full render completes well under 8ms budget (Vec<u8> sink)
- Learning: When RenderCompositor borrows `&mut W`, tests that need multiple render passes should use `Cursor<Vec<u8>>` (owned by compositor) instead of `&mut Vec<u8>` to avoid borrow conflicts
- Learning: crossterm 0.29 `SetAttribute(Attribute::Reset)` resets fg/bg too, so attribute changes must re-emit color sequences afterward

**2026-02-18 — Tasks 005, 006, 007, 008: VT Grid, Input Decoder, Event Bus, JSON-RPC**
- Completed: all four tasks implemented in parallel
- Task 005: Virtual terminal grid (shux-vt) — cell, grid, cursor, vte parser, VirtualTerminal API
- Task 006: Input decoder (shux-ui) — key types, modifiers, crossterm event translation
- Task 007: Event bus (shux-core) — typed event taxonomy, broadcast pub/sub, sequence numbers, history
- Task 008: JSON-RPC server (shux-rpc) — error codes, codec, router, UDS/TCP server, builtin methods

**2026-02-18 — Tasks 002, 003, 004: Core Data Model, Layout Engine, PTY Manager**
- Created `crates/shux-core/src/model.rs`: SessionId, WindowId, PaneId (UUID newtypes via macro), Session, Window, Pane, RestartPolicy with serde kebab-case, Version stamps, Tags
- Created `crates/shux-core/src/graph.rs`: SessionGraph (single-writer with ArcSwap), SessionGraphSnapshot (immutable reads), GraphCommand (13 mutation variants with oneshot reply), GraphHandle (async convenience methods), run_graph_loop
- Created `crates/shux-core/src/layout.rs`: LayoutNode (Split/Leaf binary tree), Direction, Rect, NavDirection, WindowLayout with zoom save/restore, smart_split (wider→vertical, taller→horizontal), directional_focus (center-distance heuristic), resize_pane with ratio clamping [0.05, 0.95], 1-cell separator
- Created `crates/shux-pty/src/handle.rs`: PtyHandle wrapping pty_process::Pty + tokio::process::Child, PtyConfig, PtySize, PtyError (pty_process::Error for Open/Spawn/Resize, std::io::Error for Read/Write), CWD tracking via /proc/pid/cwd (Linux) or initial_cwd fallback (macOS)
- Created `crates/shux-pty/src/manager.rs`: PtyManager, PtyEvent (Output/Exited/Restarted), run_pty_read_loop with CancellationToken, should_restart, respawn_pty
- Created `crates/shux-pty/tests/integration.rs`: 7 integration tests (spawn_echo, exit_status, failing_command, write_and_read, resize, initial_cwd, pty_event_output)
- Updated workspace Cargo.toml: added `async` feature to pty-process
- 101 tests passing (36 model+graph, 28 layout, 10 pty unit, 7 pty integration, 20 pre-existing)
- Learning: pty-process 0.5 API differs from docs — `open()` returns `(Pty, Pts)`, Command uses consuming builder pattern, `spawn(pts)` takes Pts arg
- Learning: tokio::process::Child `kill()` is async; use `start_kill()` for synchronous kill

**2026-02-18 — Task 001: Daemon Skeleton and Process Lifecycle**
- Created `crates/shux-core/src/daemon.rs`: DaemonState, DaemonCommand, ShutdownTokens, run_daemon_state_loop with auto-exit grace timer
- Created `crates/shux/src/daemon.rs`: runtime_dir, PID file, socket path, double-fork daemonize(), signal handler (SIGTERM/SIGINT/SIGHUP)
- Created `crates/shux/src/client.rs`: ensure_daemon_running() with UDS probe + exponential backoff + re-exec auto-start
- Wired up main.rs with __daemon internal subcommand (fork-before-tokio) and client entrypoint
- 20 tests passing: DaemonState lifecycle, grace timer with tokio::time::pause(), shutdown tokens, PID file round-trip, runtime dir
- Added nix (user feature), tokio-util, thiserror dependencies
- Learning: Rust edition 2024 makes `std::env::set_var`/`remove_var` unsafe (process-global mutable state)
- Learning: nix 0.29 requires explicit `user` feature flag for `getuid()`
- Learning: Use `tokio::time::pause()` + `advance()` for deterministic timer tests instead of real 5+ second sleeps

**2026-02-18 — Task 000: Repository Scaffold and Tooling**
- Created Cargo workspace with 7 crates (shux binary + 6 library crates)
- All crates compile, clippy passes, rustfmt passes, nextest runs (0 tests)
- Created Makefile with self-documenting help, colored output, all required targets
- Created lefthook.yml with pre-commit (fmt+clippy) and pre-push (progress-check+test+deny)
- Created CLAUDE.md agent instructions and AGENTS.md redirect
- Created .github/workflows/ci.yml (check + test on ubuntu/macos + deny)
- Created deny.toml, clippy.toml, .cargo/config.toml, rust-toolchain.toml
- Created scripts/setup-dev.sh and scripts/check-progress.sh
- Created .claude/settings.json with Stop hook (progress gate), PreToolUse hooks (push gate, commit reminder)
- Created .claude/automations/ directory for iterm2-driver visual tests
- Learning: `cargo nextest` exits 4 with 0 tests unless `--no-tests=pass` is passed
- Learning: `edition = "2024"` requires Rust 1.85+; stable channel is 1.93.1 as of Feb 2026

---

## Task List

| ID | Task | Phase | Status | Depends On |
|----|------|-------|--------|-----------|
| 000 | Repository scaffold and tooling | Bootstrap | **Done** | — |
| 001 | Daemon skeleton and process lifecycle | M0 | **Done** | 000 |
| 002 | Core data model and SessionGraph | M0 | **Done** | 000 |
| 003 | Layout engine (binary split tree) | M0 | **Done** | 002 |
| 004 | PTY manager | M0 | **Done** | 001 |
| 005 | Virtual terminal grid | M0 | **Done** | 000 |
| 006 | Input decoder | M0 | **Done** | 000 |
| 007 | Event bus | M0 | **Done** | 002 |
| 008 | JSON-RPC server foundation | M0 | **Done** | 001, 002 |
| 009 | Render compositor (single pane) | M0 | **Done** | 005, 006 |
| 010 | Minimal TUI client | M0 | **Done** | 004, 008, 009 |
| 011 | CLI foundation (clap) | M0 | **Done** | 001, 008 |
| 012 | M0 integration and quality gate | M0 | **Done** | 001–011 |
| 013 | Session CRUD (API + CLI) | M1 | **Done** | 012 |
| 014 | Window CRUD (API + CLI) | M1 | **Done** | 013 |
| 015 | Pane operations (split, focus, resize, zoom, swap, kill) | M1 | **Done** | 014, 003 |
| 016 | Pane I/O (send_keys, run_command, capture) | M1 | **Done** | 015, 004 |
| 017 | Multi-pane rendering | M1 | **Done** | 015, 009 |
| 018 | Tier 1 keybindings (bare keys) | M1 | Pending | 017 |
| 019 | Prefix key system (Tier 2) | M1 | Pending | 018 |
| 020 | Mouse support | M1 | Pending | 017 |
| 021 | Copy mode | M1 | Pending | 019 |
| 022 | TOML config system | M1 | Pending | 012 |
| 023 | Live config reload | M1 | Pending | 022 |
| 024 | Theme engine and token system | M1 | Pending | 022 |
| 025 | Per-pane theming | M1 | Pending | 024, 017 |
| 026 | Status bar (hardcoded, pre-plugin) | M1 | Pending | 025 |
| 027 | Pane titles (manual + auto) | M1 | Pending | 015 |
| 028 | Capability negotiation (ClientCaps) | M1 | Pending | 010 |
| 029 | Synchronized output (Mode 2026) | M1 | Pending | 028 |
| 030 | Session templates | M1 | Pending | 022, 015 |
| 031 | Keybinding configuration and conflict detection | M1 | Pending | 019, 022 |
| 032 | Command palette | M1 | Pending | 019, 031 |
| 033 | Help overlay (keybinding cheat sheet) | M1 | Pending | 032 |
| 034 | M1 integration and quality gate | M1 | Pending | 013–033 |
| 035 | Complete JSON-RPC API surface | M2 | Pending | 034 |
| 036 | Event stream (events.watch) | M2 | Pending | 035, 007 |
| 037 | Optimistic concurrency and ensure operations | M2 | Pending | 035 |
| 038 | Plugin host: wasmtime integration | M2 | Pending | 034 |
| 039 | Plugin permissions and sandbox | M2 | Pending | 038 |
| 040 | Plugin WIT host functions | M2 | Pending | 039 |
| 041 | Plugin lifecycle and hot reload | M2 | Pending | 040 |
| 042 | Event interception chain | M2 | Pending | 041, 036 |
| 043 | Command override system | M2 | Pending | 041 |
| 044 | Process plugin protocol | M2 | Pending | 041 |
| 045 | Plugin API extensions | M2 | Pending | 041, 035 |
| 046 | Overlay system (z-ordered stack) | M2 | Pending | 041 |
| 047 | Inter-plugin event bus | M2 | Pending | 041, 036 |
| 048 | Bundled plugin: shux-status-bar | M2 | Pending | 046, 047 |
| 049 | Bundled plugin: shux-theme-pack | M2 | Pending | 041 |
| 050 | Bundled plugin: shux-diagnostics | M2 | Pending | 046, 045 |
| 051 | gRPC API (optional transport) | M2 | Pending | 035 |
| 052 | M2 integration and quality gate | M2 | Pending | 035–051 |
| 053 | Performance optimization campaign | M3 | Pending | 052 |
| 054 | Shell completions (bash, zsh, fish) | M3 | Pending | 052 |
| 055 | Image passthrough (DCS, Kitty, Sixel, iTerm2) | M3 | Pending | 052 |
| 056 | Fuzzing campaign (ANSI, JSON-RPC, config, layout) | M3 | Pending | 052 |
| 057 | Documentation (README, guides, API reference) | M3 | Pending | 052 |
| 058 | Binary releases and distribution | M3 | Pending | 052 |
| 059 | M3 final quality gate and v1.0 release | M3 | Pending | 053–058 |
| 060 | Rich CLI output — beautiful list commands | M1 | **Done** | 011, 015 |
