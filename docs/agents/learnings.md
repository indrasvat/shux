# Learnings

> **STRICT RULE:** This section MUST be updated at the end of every coding session.
> Each entry should be a concrete, actionable insight. Delete entries that become obsolete.

- **2026-08-07 (issue #130 — a slow suite is a BLIND suite, not just a slow one):**
  `make test` took 461s serially and hid three real defects, one of them in
  production code. The plugin host leaked its children on every failed handshake
  (`Child::kill` signals one process; a plugin is a process tree) — and the leak
  guard could not see it because the stray `sleep 30` expired during the
  remaining 400 seconds of serial execution. Cutting the suite to 22s is what
  made the guard start failing. **When a guard starts firing after a speedup,
  the speedup did not break anything — it removed the delay that was hiding the
  break.** Reproduce the finding serially and in isolation before blaming
  parallelism; all three of these reproduced with `-j 1`.

- **2026-08-07 (nextest test-groups fail SILENTLY, in two different ways):**
  A group whose filterset matches nothing is not an error — nextest prints an
  empty group and runs everything unbounded, and nothing goes red. `test(=...)`
  matches the test NAME only, so `test(=shux::bin/shux::tests::foo)` — which
  reads exactly like the identifier `cargo nextest list` PRINTS — matches every
  test in the workspace, not the one it names. Separately, a test belongs to at
  most ONE group and the first matching override wins, so an earlier group
  silently swallows binaries a later one names and both still look healthy.
  Both shipped here. `scripts/check-test-groups.sh` now asserts each group's
  EXACT member count; a non-empty check would have caught neither. Scope
  filtersets by `binary_id()` first, then by name.

- **2026-08-07 (killpg is only safe while the child is UNREAPED):** An unreaped
  child pins its pid; `Child::wait()` reaps it and hands the pid straight back
  to the kernel's free pool. Keeping the (now-empty) `Child` value in scope does
  NOT pin anything — the pid is freed by `waitpid`, not by the Rust value's
  lifetime. Since shux makes every plugin a process-group leader (as does every
  job-control shell), a `killpg` on a recycled pid can SIGKILL an unrelated
  group. Wait out a shutdown grace by polling the group with signal 0
  (`killpg(pgid, None)`), never by `wait()`ing, and reap only after signalling.

- **2026-08-07 (optimisation moves timing-RATIO thresholds, not just runtimes):**
  `region_scroll_cost_is_linear_in_pane_height` compares a 1024-row scroll
  against a 128-row one and asserts the ratio is under 12x. The threshold was
  calibrated at `opt-level = 0` (8.1x fixed, 19.9x broken). At `opt-level = 1`
  the honest ratio rises to ~11-12.8x — the small arm fits in cache and
  optimises well, the large arm is memory-bandwidth-bound and cannot. A ratio
  cancels out machine SPEED but not cache behaviour and not contention. Measured
  A/B: 1/20 solo failures at opt-level 1, 0/20 at opt-level 0. If you change a
  build profile, re-run every test whose assertion is a measurement.

- **2026-08-07 (`max-threads = 1` does NOT give a test exclusivity):** A
  test-group's `max-threads` serializes its members against EACH OTHER and
  nothing else. For a group with one member it is a complete no-op — the test
  still competes with every test outside the group. Measured: the timing-ratio
  test read 8.1x at one thread per core and 30.8x at four, against a 12x
  threshold, purely from contention, while nominally "serialized".
  `threads-required = "num-test-threads"` is the knob that reserves the whole
  run's budget and actually makes a test run alone.

- **2026-08-07 (measure a WARM cache when comparing RUSTFLAGS):** `RUSTFLAGS` is
  part of cargo's fingerprint, so the first build after any flag change rebuilds
  the entire graph. Comparing that against a warm default build reported
  `rust-lld` as 35x SLOWER than the default linker. Warm on both sides, the real
  answer was "no measurable difference" (2.50s/2.33s vs 2.33s/2.41s) — because
  `debug = "line-tables-only"` had already removed most of what a linker spends
  its time on. Fixing the profile made the linker swap redundant; the
  long-commented-out mold block was deleted rather than enabled.

- **2026-08-07 (a unique `ps` needle must not change the process's LIFETIME):**
  Encoding uniqueness into a sleep duration (`sleep 29456`, from
  `format!("29{:03}", pid % 1000)`) makes every leaked marker outlive the run by
  eight hours, and `pid % 1000` collides across runs anyway — so a stale process
  from a Ctrl-C'd run gets attributed to the code under test. Put the marker in
  `argv[0]` instead (`sh -c 'exec -a <marker> sleep 30'`) and use the full pid.

- **2026-07-09 (task 077 lens P2 — `pane.glance` atomicity vs. unsynchronized
  multi-write frame producers):** A per-pane RPC can be PROVABLY atomic (one
  lock, one clone, render+text both derived from that frozen clone — verified
  by three independent concurrent calls landing on the same `ContentRevision`
  returning byte-identical PNG+text) and STILL observe a "torn" application-
  level frame, because `ContentRevision` (PRD §4.2) bumps once per PTY
  `process()` BATCH, not once per app-level "frame" — a fixture/TUI that
  paints a full screen as N separate raw writes (no DEC 2026 synchronized-
  output `CSI ?2026h`/`?2026l` wrapping) can have its repaint split across
  multiple batches under load, and a reader's atomic clone can legitimately
  land mid-repaint. Diagnosed by reproducing at small scale outside the
  frozen test (manual pump + concurrent `pane.glance` calls against F3),
  confirming the SAME revision always reproduces the SAME (possibly mixed)
  content, then tracing root cause to `f3_flip.sh` not using sync-mode.
  Do NOT "fix" this by adding retry/quiet-wait/PTY-draining logic inside a
  read-only glance-style RPC — that violates the "reflects exactly what the
  VT has processed at lock time, no implicit drain" contract and makes the
  API secretly fixture-aware. The correct fix belongs in the
  PRODUCER: wrap the writes in DEC 2026 sync mode, which `shux-vt` already
  supports (P1 shipped `sync_present`/`SyncPresentation` — Class-A events
  during sync are deferred and released as ONE atomic batch on `?2026l`).
  RESOLUTION (same day, adjudicated): the F3 sync-wrap was approved as a
  LENS-TEST-CHANGE and applied — G1 went 0/3 → 3/3 green with ZERO
  implementation changes, confirming the diagnosis. Companion ruling: OSC
  10/11/12 dynamic default colors were re-adjudicated Class A (they change
  the presented frame's pixels), detected by a before/after
  `default_colors` compare in the batch disjunction — the parser's
  change-guards keep value-equal sets net-zero, and the existing
  sync-deferral covers color changes under `?2026h` for free. OSC 4
  palette redefinition stays Class B (documented known limitation).
  When a red-suite fixture predates a new capability (glance/settle) that
  depends on frame-level atomicity, check whether it uses sync mode before
  assuming the RPC has a locking bug.
- **2026-07-08 (task 077 lens P1 — ContentRevision):** Detecting a "content
  changed" event in the VT write path without diffing cell values (identical
  repaints MUST still bump) needs a value-INDEPENDENT signal. The clean solution
  was a monotonic `Grid::mutations()` write tally, incremented in every grid
  mutation method (cell write, scroll, clear, erase, insert/delete, mark-all)
  regardless of the resulting value — then compared before/after each
  `process()` batch, together with cursor position/visibility and the
  alt-screen flag (which the §4.2 table names as "change" events, so comparing
  them is legitimate, not forbidden cell-value diffing). Crucially this tally is
  SEPARATE from `DirtyState`: DirtyState is drained/coalesced by the attach
  render path, so a concurrently attached client would make a DirtyState-derived
  counter miss frames. The alt-screen grid swap (`std::mem::replace` of
  `self.grid`) means the mutations tally belongs to whichever grid is now live —
  guard the before/after tally comparison with `before_alt == after_alt` and let
  the alt-toggle be its own Class-A term. Batch granularity ("one bump per
  `process()`") falls out for free from the single before/after comparison.
- **2026-06-29 (task 075 plugin DX):** Plugin package validation must account
  for the daemon boundary. Directory installs should canonicalize the package
  root and entrypoint in the client before sending `plugin.install`, reject
  symlink escapes after canonicalization, and default package cwd to the
  package root. If a package manifest advertises name/version, pass those as
  expected handshake values to the plugin manager so a process cannot register
  a different runtime identity than the package metadata. Keep package ids as
  metadata until the process protocol grows an id field.
- **2026-06-29 (task 076 Sightline):** Cargo test binaries must run with
  hermetic XDG config/state roots. Otherwise daemon-backed integration tests can
  inherit the developer's real `~/.config/shux/config.toml`, execute personal
  statusbar/starship hooks, and trip the leak guard on unrelated orphan
  automation children. Keep user-config dogfood explicit; keep the default test
  runner isolated.
- **2026-06-13 (task 072 origin mode and scroll regions):** DECOM is an
  addressing policy, not a separate cursor coordinate space. Store cursor rows
  as absolute grid rows, offset CUP/HVP/VPA and CPR/DSR row reports through the
  active scroll-region top only while origin mode is enabled, and clamp
  save/restore to the grid rather than to the current margins. Relative
  vertical motions (`CUU`/`CUD`/`CNL`/`CPL`/`VPR`) should clamp to margins only
  when the cursor starts inside the region; outside it they remain full-grid
  movement. Real PTY probes that paste multi-line scripts through an interactive
  shell should wait for shell readiness and submit CRLF so the fixture executes
  reliably under login-shell/readline variants.
- **2026-06-18 (dependency upgrades):** `nix` 0.31 moves fd helpers toward
  `AsFd`/`OwnedFd`: pass `&OwnedFd` to `fcntl`/`dup`, borrow raw async fds only
  at the PTY read/write boundary, and use `libc::dup2` for daemon stdio
  redirection to fd 0/1/2 so ownership of standard descriptors does not get
  confused. `sha2` 0.11 finalized digests no longer format directly with
  `LowerHex`; encode bytes explicitly to preserve existing `sha256:<hex>` audit
  strings. PTY response tests should assert the required escape sequence is
  present and stop their test server before panicking, because shell echo/newline
  bytes can legitimately precede terminal responses.
- **2026-06-12 (task 071 tab stops):** Mutable tab stops are terminal state,
  not parser-local cursor math. A flat bitmap seeded with `col > 0 && col % 8 ==
  0` avoids the Default-to-Explicit trap where first HTS/TBC wipes existing
  defaults. `TBC 3` needs a separate latch so resize growth does not resurrect
  cleared defaults, while local HTS/TBC mutations can still extend default
  8-column stops on grow. RIS resets tabs; DECSTR and alternate-screen switches
  must not.
- **2026-06-12 (task 070 DEC special graphics):** DEC charset selection is VT
  state, not parser-handler scratch state. Store G0/G1 and active GL selection
  on `VirtualTerminal`, translate only in `print()`, reset on RIS, and snapshot
  the charset set inside `SavedCursor` so DECSC/DECRC and 1049 alternate-screen
  transitions cannot clobber nested saved state. Baseline promotion for charset
  rendering must remain separate from verification and tied to DootSabha-approved
  fixture content; otherwise exact pixel matching can become a same-run proof.
- **2026-06-12 (task 069 grapheme storage):** Grapheme payloads are cell
  content, not cursor/style state. Store them only on the target cell and use
  `Arc::make_mut` so hyperlink/underline attrs shared by a styled run remain
  shared until a specific cell needs a payload. Parser anchors for combining
  marks and ZWJ clusters must be cleared by cursor/grid movement, ESC dispatch,
  resize, and alternate-screen transitions; otherwise later zero-width scalars
  can attach to stale cells after movement.
- **2026-06-11 (task 073 VT corpus harness):** A replay corpus needs three
  separate artifact classes: committed `.shux/fixtures/` input bytes,
  committed `.shux/goldens/` baselines, and PR-attached review evidence. Commit
  `.shux/qa/` PNG evidence only when it is a deliberate durable baseline or
  fixture with task and DootSabha approval. The check target should never
  promote baselines; promotion must be a separate Make target with
  council-approved provenance. Exact pixel gates
  are viable across local macOS and Linux CI when the raster path uses embedded
  font bytes, fixed rows/cols, fixed font size, fixed defaults, and
  cursor-disabled rich-TUI replays. Keep live `pane.record` refreshes in
  `.shux/out/` so installed-tool variance cannot mutate review baselines.
- **2026-06-11 (VT quality planning):** The libghostty spike exposed a concrete
  shux-side priority order: `Row.wrapped` already exists, so resize reflow can
  be improved inside `shux-vt` before replacing the backend; wide-cell
  invariant checks should come before grapheme storage; every VT-visible task
  needs full-resolution screenshots plus pixel-level PNG comparison, not only
  contact sheets or text captures. The local `shux-vt-solid-qa` agent is the
  hard gate for this track and must enforce each task file's exact DoD.
- **2026-06-11 (VT QA enforcement):** A VT hard gate is only real if it produces
  reviewable evidence. `.shux/out/` is scratch space; attach transient
  screenshots to the PR and commit only durable SOLID reports, manifests, and
  approved baseline/fixture PNGs under `.shux/qa/`. Baselines must have
  committed provenance or DootSabha approval; never let an implementation mint
  its own expected PNG and pass against it.
- **2026-02-18 (task 000):** `edition = "2024"` requires Rust 1.85+. The `rust-toolchain.toml` pins stable which is ≥1.85 as of Feb 2026, but CI should use `dtolnay/rust-toolchain@stable` to stay current.
- **2026-02-18 (task 001):** Rust edition 2024 makes `std::env::set_var`/`remove_var` unsafe. Wrap in `unsafe {}` with safety comments in tests. Use `tokio::time::pause()` + `advance()` for deterministic timer tests instead of real sleeps.
- **2026-02-18 (task 001):** nix 0.29 requires explicit feature flags per module: `"user"` for `getuid()`, `"process"` for `fork()`/`setsid()`, `"signal"` for signal handling, `"fs"` for `dup2()`. Grace timer pattern: store `Option<tokio::time::Instant>` deadline and use `sleep_until()` inside `select!` async block to avoid `Pin` complexity.
- **2026-02-18 (tasks 002-004):** pty-process 0.5 async API: `pty_process::open()` returns `(Pty, Pts)` (not `Pty::new()`); `Command` uses consuming builder pattern; `spawn(pts)` takes `Pts` arg. Error types: `pty_process::Error` for open/spawn/resize, `std::io::Error` for read/write. Use `child.start_kill()` (sync) instead of `child.kill()` (async) in `PtyHandle::kill()`.
- **2026-02-18 (tasks 002-004):** ArcSwap pattern for single-writer/many-readers: `Arc<ArcSwap<Snapshot>>` shared between GraphHandle (readers) and run_graph_loop (writer). Writer calls `state.store(Arc::new(snapshot))` after each mutation. Readers call `state.load()` for lock-free access. GraphCommand enum with oneshot::Sender reply channels for async request-response.
- **2026-02-18 (task 005):** vte 0.15's `Parser::advance()` accepts a full `&[u8]` slice (not byte-by-byte). The raw `vte::Perform` trait (`print`/`execute`/`csi_dispatch`/`esc_dispatch`/`osc_dispatch`) gives more control than `vte::ansi::Handler` and is the primary trait. VtHandler borrows all VirtualTerminal fields mutably.
- **2026-02-18 (task 008):** Rust edition 2024 requires `Send + Sync` bounds on `Box<dyn std::error::Error>` for tokio::spawn contexts. `ref` patterns in match arms are disallowed in edition 2024 — use `&` patterns instead.
- **2026-02-18 (task 009):** crossterm 0.29 `SetAttribute(Attribute::Reset)` resets fg/bg colors too, so after an attribute change the render backend must re-emit `SetForegroundColor`/`SetBackgroundColor`. Handle attributes before colors in `apply_style()`.
- **2026-02-18 (task 009):** When `RenderCompositor<W: Write>` borrows `&mut Vec<u8>`, tests needing multiple render passes hit borrow conflicts. Use `Cursor<Vec<u8>>` (owned by compositor) or separate compositor instances per render call. The `Cursor<Vec<u8>>` pattern works well with a `make_compositor()` helper in tests.
- **2026-02-19 (task 010):** `parse_key_from_bytes` must handle Enter (`\r`=0x0d) and Tab (`\t`=0x09) as specific match arms BEFORE the Ctrl+A-Z range (1..=26), since \r and \t fall within that range but should map to `KeyCode::Enter`/`KeyCode::Tab` rather than `Ctrl+M`/`Ctrl+I`.
- **2026-02-19 (task 010):** crossterm `enable_raw_mode()` is process-global (not per-thread). For async event loops, use `tokio::task::spawn_blocking` for `crossterm::event::poll()`/`event::read()` to avoid blocking the tokio runtime. The terminal_demo example shows the pattern: poll in main thread with Duration timeout, render after each key.
- **2026-02-19 (task 011):** `tokio::process::Command` (async) must be used instead of `std::process::Command` (blocking) inside `#[tokio::test]` when the test also runs a server task on the same runtime, otherwise the blocking `.output()` call starves the server and deadlocks.
- **2026-02-19 (task 011):** CLI output styling lives in `crates/shux/src/style.rs`. All CLI text output MUST use the style helpers (accent/success/warning/error/muted/bold + print_* functions) for consistent aesthetics. Respects NO_COLOR and IsTerminal. Color palette: accent=Cyan, success=Green, warning=Yellow, error=Red, muted=Dim.
- **2026-02-19 (task 012):** Edition 2024 disallows `unwrap_or(&vec![])` — the temporary `vec![]` is freed while the borrow is still live. Use `.cloned().unwrap_or_default()` instead.
- **2026-02-19 (task 012):** Session RPC methods (`session.list`, `session.create`, `session.kill`, `session.ensure`) must be registered in the binary crate (`crates/shux/src/main.rs`), not in `shux-rpc`, because they require `GraphHandle` from `shux-core` and the RPC crate intentionally has no dependency on core. The `register_session_methods()` helper is duplicated in test files (acceptable since binary crates aren't importable by integration tests).
- **2026-02-19 (task 013):** Centralize `GraphError` → `RpcError` mapping in a `graph_error_to_rpc()` helper function. Each RPC handler calls this mapper instead of ad-hoc error conversion, ensuring consistent error codes (`NotFound`, `NameConflict`, `InvalidParams`, `VersionConflict`) across all session methods. Similarly, `session_to_json()` standardizes response structure.
- **2026-02-19 (task 013):** When an RPC method accepts either a name or UUID identifier (e.g., `session.kill`, `session.rename`), try `SessionId::from_str()` first; if it fails as a UUID, treat it as a name lookup. This dual-mode resolution gives both humans (names) and programmatic clients (UUIDs) convenient access.
- **2026-02-19 (task 014):** Window RPC methods (`window.list/create/kill/rename/focus/reorder/ensure`) follow the same `register_*_methods()` pattern as session methods — registered in binary crate with `GraphHandle` closures, duplicated in test files. `window_to_json()` standardizes response structure with `title`, `index`, `pane_count`, `is_active`, `active_pane_id`.
- **2026-02-19 (task 014):** CLI `resolve_window_id()` tries numeric index parse first, then name lookup — same dual-mode pattern as session resolution. Window commands use `-s session -w window` flags consistently.
- **2026-02-19 (task 014):** `rpc_display()` extracts human-readable messages from RPC error data fields (`detail` for invalid_params, `name`+`resource` for name_conflict, `id`+`resource` for not_found) instead of showing raw "RPC error -32NNN: code_name". Makes CLI errors much more user-friendly.
- **2026-02-19 (task 014):** `ensure_daemon_running_at()` performs a version handshake: after connecting, calls `system.version` and compares against `CLIENT_VERSION` (`env!("CARGO_PKG_VERSION")`) AND `CLIENT_GIT_SHA` (`env!("SHUX_GIT_SHA")`). On mismatch, kills old daemon via SIGTERM (PID file), waits for exit, spawns fresh. Prevents `method_not_found` after rebuilds. The `build.rs` in both `shux` and `shux-rpc` captures `git rev-parse --short HEAD` at compile time.
- **2026-02-19 (task 015):** Pane RPC methods follow the same `register_pane_methods()` pattern as session/window. `resolve_pane_id_from_params()` provides flexible resolution: explicit `pane_id` → window's `active_pane` → session's active window's active pane. `resolve_window_id_from_params()` similarly chains session → active_window. Both helpers are duplicated in test files. clap auto-lowercases variant names, so `Pane` already creates the `pane` command — adding `#[command(alias = "pane")]` causes a panic.
- **2026-02-19 (task 060):** `TerminalContext::detect()` auto-switches Text→Plain when stdout is piped (`!is_tty`) or `TERM=dumb`. This means CLI integration tests that capture stdout via `tokio::process::Command` get Plain format (tab-separated, no box-drawing). Test assertions must match Plain format or explicitly pass `--format text`. Empty lists in Plain format produce no output (standard Unix convention).
- **2026-02-19 (task 060):** Hand-rolled `BoxRenderer` and `ColumnLayout` (~210 lines total) are sufficient for CLI tabular output — no need for `tabled` or `comfy-table` crates. Key pattern: `styled_if(text, colors, fg, bold, dim)` applies ANSI codes only when `colors=true`, enabling the same rendering code path for colored and plain output. `short_id()` truncates UUIDs to 8-char prefix (like git short SHA).
- **2026-02-19 (task 060):** Unicode width pitfalls in box-drawing: (1) Use `unicode-width` crate (`UnicodeWidthStr::width()`) not `.len()` or `.chars().count()` for terminal column calculations. (2) Rust's `format!("{:<width$}")` pads by char count, not display width — use manual `pad_right()`/`pad_left()` helpers with `display_width()` instead. (3) In `BoxRenderer::header()`, the between-corners fill must exclude the corner characters from the prefix length calculation — counting the corner inflates the prefix and makes the header 1 char shorter than rows/footer.
- **2026-02-19 (task 016):** PTY input echo contains the literal typed command. When using a marker technique (`SHUX_MARKER{id}EXIT{$?}SHUX_END`), the terminal's echo of the `echo` command matches the marker detector before the actual output, causing exit_code=None. Fix: split the shell string (`echo "SHUX_MAR""KER..."`) so input echo breaks the pattern while shell concatenation produces the correct output.
- **2026-02-19 (task 016):** Channel-based PTY write architecture: each pane gets an `mpsc::Sender<Vec<u8>>` write channel. A per-pane tokio task owns the `PtyHandle` and uses `select!` for concurrent read (PTY→VT+CommandEngine) and write (channel→PTY). This avoids ownership conflicts between `PtyManager::write(&mut self)` and the read loop that borrows the handle.
- **2026-02-19 (task 016):** `PaneIoState` (shared `Arc<Mutex<>>`) holds `writers` (HashMap<PaneId, mpsc::Sender>), `vts` (HashMap<PaneId, VirtualTerminal>), and `cmd_engine` (CommandEngine). Every `register_*_methods()` function that creates or destroys panes must also spawn/cleanup PTY tasks and VT instances via this shared state.
- **2026-05-08 (task 017):** `tokio::sync::Notify` `notify_waiters()` only wakes tasks **currently awaiting** `.notified()`; if the renderer is mid-CPU when the wakeup posts, it's silently dropped. Use `notify_one()` which queues a permit consumed by the next `.notified().await` — this is the correct primitive for "wake the next render".
- **2026-05-08 (task 017):** `tokio::select!` arm patterns: `Some(x) = recv()` is a **refutable** pattern that silently disables the branch when the channel returns None. You cannot detect channel close this way. Use `res = recv() => { match res { Some(x) => ..., None => break } }` so closing the sender prompt-exits the task.
- **2026-05-08 (task 017):** Multi-pane multiplexer winsize rule: each pane's PTY must be told its **layout rect size**, not the full client size. Apps polling `TIOCGWINSZ` (vim, htop, less) lay themselves out wrong otherwise. The daemon must re-fan winsizes after every layout-changing action (split, zoom, kill, resize, window switch), not just on initial attach + client resize.
- **2026-05-08 (task 017):** Don't infer client terminal size from a pane's VT grid. It creates a self-feeding shrink loop: split pane is half-width → its grid is 40 cols → daemon "infers" 40-col client → resizes compositor → pane shrinks to 18 cols → infers 18-col client, etc. Track client size as authoritative state (`Arc<Mutex<(u16, u16)>>`) updated **only** by `Resize` frames.
- **2026-05-08 (task 017):** Holding `Arc<Mutex<PaneIoState>>` across `.await` on a bounded `mpsc::send()` deadlocks the entire session if any single PTY task gets slow. Pattern: `let tx = { state.lock().await.writers.get(&p).cloned() }; tx.send(...).await` — clone the Sender out, drop the lock, then await.
- **2026-05-08 (task 017):** Interactive input forwarding should use `tx.try_send(bytes)` (drop the keystroke if full) rather than `tx.send(bytes).await` (block the whole attach loop). A backpressured pane shouldn't be able to freeze the user out of detaching or switching panes.
- **2026-05-08 (task 017):** Border-drawing compositor pattern: pane content goes inside a 1-cell-inset viewport (`Rect::new(content.x+1, content.y+1, content.width-2, content.height-2)`), and the outer ring is the border outline. Pass the OUTER content area to `compute_borders` so it can render the outline + inter-pane separators in the gaps reserved by `compute_rects`. Suppress borders entirely when content area is < 3×3.
- **2026-05-08 (task 017):** Daemon-renders-everything attach pattern: client is a thin pipe (writes daemon-supplied ANSI bytes to stdout, polls crossterm events on a separate OS thread, forwards keys as Input frames). Daemon owns the RenderCompositor, walks all VTs in the active window, runs render_multi_pane into a `Vec<u8>`, drains via `std::mem::take(compositor.inner_mut())`, ships base64'd as Render frames at 200ms tick + on render_pulse notify. This matches tmux's architecture and lets multiple clients attach independently.
- **2026-05-08 (task 017 followup):** Spawning user shells: use `<shell> -l -i` (login + interactive), not just `-l`. Many users' `~/.bash_profile` sources `~/.bashrc` gated on `$- == *i*`; without `-i` that branch never fires, so `~/.bashrc` (where starship/atuin/ble.sh init lives) never runs. Same flags iTerm2 uses by default.
- **2026-05-08 (task 017 followup):** Multiplexers must claim `TERM_PROGRAM` (don't inherit). User rc files branch on it (e.g. "skip starship under Warp", "iTerm-specific copy/paste"). Inheriting the parent emulator's value silently fires those branches wrong. Pattern: set `TERM_PROGRAM=<your name>` and `TERM_PROGRAM_VERSION=<your ver>` on every PTY spawn — tmux uses `TERM_PROGRAM=tmux`. shux uses `TERM_PROGRAM=shux`. Also inject `SHUX=1` (mirrors `TMUX` env var) so users can detect they're inside shux.
- **2026-05-08 (task 017 followup) — iterm2-driver patterns:** (1) Never use `app.current_terminal_window` — race-prone with parallel scripts. Use `iterm2.Window.async_create()` per script. (2) `Window.async_create()` returns BEFORE iTerm finishes init; the returned object's `current_tab` is None. Sleep ~0.5s, then refresh via `async_get_app()` and find your window by `window_id`. Skipping this is the #1 cause of intermittent automation failures. (3) Multi-level cleanup in `try/finally` — track every window/session, close all in finally even on crash. Add a `cleanup_stale_windows(prefix=...)` janitor at the START of every script too. (4) Screenshots: position-based Quartz correlation works without focus and for non-frontmost windows; `screencapture -l <quartz-id>` with id picked by minimum (Δx*2 + ΔW + ΔH) score. (5) For shell automation use `\n` (LF), not `\r` (CR) — readline replacements like ble.sh map `\r` to "insert-newline" within multiline edits and trap automation. `\n` bypasses the readline keymap entirely.
- **2026-05-10 (PR 3b — optimistic concurrency):** (1) `GraphError::VersionConflict { resource: &'static str, id: String, expected, actual }` — adding `resource`+`id` to the model makes `RpcError::version_conflict(...)` produce the full PRD §8.3 `data` shape without the RPC handler needing to know which entity it's talking about. The error mapper just unpacks the struct fields. (2) Layout ops (resize/zoom/swap) bump every pane's version in the affected window, not just the target's. Without this, `expected_version` checks on sibling panes after a concurrent layout op would silently succeed — pane.version must be a monotonic stamp for "anything visible on this pane changed", not just "name/exit_status changed". (3) Order-of-operations on destroy_pane / destroy_session / destroy_window: ALWAYS mutate the graph FIRST, tear down PTY/VT/writer state second. A stale `expected_version` must reject the destroy before any IO state is touched, otherwise a rejected kill leaves orphaned VTs. (4) `swap_panes(a, b, expected_version)` only checks pane `a` (the anchor) — sibling-bump makes either check equivalent, and checking both halves the success rate of concurrent swaps for no safety gain. (5) `shux api` should print `{result: ...}` xor `{error: {code, message, data}}` on stdout, with `std::process::exit(2)` on the error path. Agents parse the structured envelope; they shouldn't have to scrape human-readable `rpc_display()` text from stderr. (6) Test-file duplicates of register_*_methods() and graph_error_to_rpc() bit me again — every PR that adds an RPC param needs to update them all (`crates/shux/src/main.rs`, `tests/m0_integration.rs`, `tests/cli_integration.rs`, `tests/pane_io_integration.rs`). Worth eventually extracting into a `shux-test-helpers` crate.
- **2026-05-10 (PR 4 — pane titles):** (1) Per-pane title priority: `manual_title > osc_title > command-basename > cwd-basename`. `Pane.title` is the cached priority-resolved value (renderers read it directly); `effective_title()` is the live re-compute fallback. (2) `set_osc_title()` returns `bool changed` so subscribers can fire `PaneTitleChanged` only on visible movement — crucial because bash's `PROMPT_COMMAND` re-emits the same OSC 2 every prompt and we'd otherwise flood the event bus. (3) DO NOT `std::mem::take(&mut self.title)` before `recalculate_title()` — when `auto_title=false` the recalc is a no-op and `take` leaves title empty. Clone instead, then diff. (4) Per-pane PTY task should track `last_osc_title: Option<String>` locally and forward changes to graph OUTSIDE the `io_state.lock()` — holding a Mutex across a bounded mpsc send is the classic deadlock pattern (PR #7 lesson). (5) `MultiPaneFrame.titles: Option<&HashMap<PaneId, String>>` — caller passes `Pane.title` from the snapshot, NOT `vt.title()` (the VT only knows about OSC; it doesn't see manual overrides). Border overlay: ` title ` (space-padded so corners survive), truncated to `rect.width - 4` chars, written onto the pane's top border row in the same color as that pane's border. Suppress when `rect.width < 6`. (6) Pre-existing gap: `session.create` RPC spawns PTY with `command` but stores empty `Pane.command` in graph (codex P2 #10 only fixed this for `apply_batch`). Means `shux new --cmd vim` auto-derives title from cwd, not the command. Standalone fix later. (7) clap tri-state ("title: null clears, omitted leaves alone, set replaces") doesn't map directly to a clap arg. Use two CLI flags (`-t` and `--clear`) with `conflicts_with = "clear"` and synthesize the JSON null in the handler.
- **2026-05-12 (task 044a phase 0 — process plugins v0):** (1) `Subscription` type lives in `shux_core::bus`, not `EventSubscription`. `SubscriptionEvent::Lagged(u64)` is a tuple variant, not struct variant — easy to mis-import from RPC handler shapes. (2) Plugin Manager → Router circular dep is best broken with `Arc<tokio::sync::OnceCell<Router>>`: build the router with `register_plugin_methods(builder, mgr.clone())`, then `mgr.set_router(router.clone())` after `.build()`. Plugin → daemon RPC dispatches through this; tokio::spawn each dispatch so the I/O loop isn't blocked. (3) `pane.send_keys` requires UUID identifier fields (`session_id`/`window_id`/`pane_id`), NOT human names. The CLI handler resolves names → UUIDs before sending; plugins talking RPC directly hit "invalid_params" if they pass `{"session":"name"}` instead of `{"session_id":"<UUID>"}`. The event payload carries UUIDs in `params.data.data.session_id` — use those directly. Worth fixing in v0.next by accepting both forms at the RPC layer. (4) Daemon → plugin event subscription must use `subscribe_filtered(filters)` against an `Option<Subscription>` since plugins with `subscribes: []` should park forever in the select! arm — use `std::future::pending::<()>().await` as the None branch so the arm never fires. (5) Process plugins use `kill_on_drop(true)` on `tokio::process::Command`; combined with a oneshot kill signal + 2s grace via `tokio::time::timeout(_, child.wait())`, that gives a clean shutdown without explicit signal handling. (6) The handshake budget is 5s; long plugin init should happen lazily after sending the manifest. stderr is relayed to daemon `debug!()` logs tagged with the plugin name — no separate log file needed.
- **2026-05-12 (PR #23 follow-up — codex bot review fixes):** (1) **Biased `tokio::select!` + queued shutdown frame = silent force-kill.** `PluginManager::kill()` pushes `plugin.shutdown` onto `inbox_tx` (mpsc, queued) then signals `kill_tx` (oneshot, instant). The I/O loop has `biased;` and checks `kill_rx` first, so the shutdown frame sits in the queue while the kill branch starts the grace timer. Fix: drain `inbox_rx.try_recv()` into stdin on the kill branch BEFORE entering the grace wait. Any time you couple "send a goodbye over channel A, then signal exit over channel B" with a biased select, audit for this. (2) **Dedup + insert must be one lock window.** Two `Mutex::lock()` calls separated by `tokio::spawn` lets two concurrent installs of plugins reporting the same manifest name both pass `contains_key()` and overwrite each other in the HashMap — first child becomes unmanaged. Hold the lock across the entire stage-2 register window; spawn is non-blocking so the window stays tiny. The same pattern applies anywhere you `contains_key → do stuff → insert`. (3) **One canonical event-wire helper, hoisted to the lowest common crate.** `event_to_json` lived in the binary crate while the plugin host hand-rolled `serde_json::to_value(&Event)`. Moved to `Event::to_wire_json()` in `shux_core::event`. Now both `events.watch` consumers and process plugins get the identical shape — and codex bot review caught the drift before it shipped. Pattern: any helper consumed by ≥2 crates belongs in the crate they both depend on, not in whichever one happened to author it first. (4) **The `data.data.session_id` re-wrap is a separate ergonomics fix.** `#[serde(tag = "type", content = "data")]` on `EventData` means the wire shape has a payload nested under `.data.data.*`. Existing automation (`test_036_events_watch.py`) navigates that. Flattening further breaks every existing event consumer — punt to a deliberate breaking-change PR with all consumer updates batched.
- **2026-05-11 (PR #17 — landing page + skill):** (1) Bash `trap '...' RETURN` is NOT function-scoped unless `set -T` (functrace) is enabled — without it the trap persists past the function boundary and fires on every later function return. If the trap body references locals, they're long gone and `set -u` blows up with "unbound variable" AFTER successful completion. Pattern: route per-function tmp files into a script-global tmpdir + EXIT trap; never put `trap '...' RETURN` in a script that doesn't `set -T`. (2) On a Cloudflare Pages site where release-version metadata is staged into the deploy at build time (not fetched at runtime), `connect-src` should be locked to `'self'` — leaving it permissive "for the version fetch" is a stale comment that becomes an attack-surface lie. (3) GitHub Actions `workflow_run` trigger fires for completions on ANY branch by default. To gate a deploy on main, add `if: github.event.workflow_run.head_branch == 'main'` on the deploy job — `branches:` on the trigger itself is a separate, narrower filter and easy to miss. (4) `session.create.cwd` plumbing was a 3-line param-extraction fix that the prior `create_session_with_command(name, cwd, command)` graph method already supported — code/docs lied for months because the RPC handler hardcoded `current_dir()`. Always trace docs → handler → graph end-to-end when reviewing a new RPC surface.
- **2026-05-13 (PR #33 — plugin permission/audit model):** (1) **Identity must NOT be the plugin name.** Council caught this: keying grants and `Pane.created_by_plugin` on the manifest name lets a reinstall under the same name inherit the predecessor's authority + ownership. Per-install UUID at `<state_root>/by-name/<name>` is the fix; name is a display-only link. `created_by_plugin: Option<PluginId>` (typed UUID), not `Option<String>`. (2) **Sensitivity policy belongs on the route, not in a separate match.** `RouterBuilder::register_with_policy(method, Policy::fixed|param_aware, handler)` keeps the classification next to the handler; `Router::assert_every_route_has_policy()` panics at boot if you add a route and forget to classify. Some methods are param-dependent — `events.watch` filter starting with `plugin.<self>.` is `Public`, broader is `ContentRead`; use `Policy::param_aware(|params, plugin_id| ...)` for those. (3) **Audit BEFORE the plugin-only intercept early-return.** `event.publish` and `plugin.state.*` short-circuit before the router; if you only audit on the router-bound branch, those calls are invisible. Pattern: build the AuditEntry once at the top of `dispatch_plugin_frame`, write it on every parsed frame regardless of which branch handles it. (4) **macOS `tempdir()` returns under `/var` which IS a symlink to `/private/var`.** Walking parent components for symlink rejection breaks every test. Only check `symlink_metadata` of the final path; the threat is a symlink AT the grants/audit location, not symlinks in the tempdir prefix. (5) **`unwrap_or_else(PluginId::new)` triggers `clippy::unwrap_or_default`.** `PluginId` has `Default` (via the `define_id!` macro), so use `unwrap_or_default()`. Same applies to any newtype wrapping `Uuid` via the macro. (6) **Manifest `subscribes:` must lock after first install.** Hot reload re-runs handshake → without diff-vs-prior-allowlist, an attacker who edits the plugin's manifest mid-session widens the bus subscription silently. Compare new `manifest.subscribes` to the persisted `grants.subscribes.allowed`; fail handshake on net-new entries. First install snapshots whatever was in the manifest as the baseline. (7) **`Policy::ParamAware` boxed closure trips `clippy::type_complexity`.** Extract the trait-object type as `pub type PolicyFn = dyn Fn(...) + Send + Sync;` first, then wrap in `Arc<PolicyFn>`. (8) **`if d == "allow" { style::success(d) } else { style::error(d) }` doesn't compile** — each `style::*` returns a different `impl Display` opaque type. Call `.to_string()` on each branch (or use a `match` returning a single concrete `String`).
- **2026-05-15 (fix/snapshot-statusbar-segments):** (1) **Render-path parity is a recurring blind spot — anything the attach loop assembles must be re-assembled by every other render path (`window.snapshot`, `session.snapshot`, `pane.snapshot`).** PR #43 wired `populate_bar(&mut bar, &config, &segments)` into the attach loop but the snapshot path only called the first half (`build_status_bar_shared` → `build_snapshot_status_bar`) so user `[[statusbar.segment]]` entries silently vanished from PNGs. Pixel-perfect snapshots only stay honest if every render path runs the same assembly. Treat the snapshot path as "headless attach" and audit each new attach-side enhancement against it. (2) **`SegmentCache::set` should stay module-private — expose `set_for_test` under `#[cfg(test)]` instead.** Production has exactly one writer (the runner task); making `set` `pub` invites other call sites to invent a second writer and we lose the single-source-of-truth property. (3) **Visual proof matrix is cheap.** Capturing `(snapshot × no-segments)` + `(snapshot × rich-segments)` + `(session.snapshot × rich-segments)` takes ~30s with `target/release/shux session create --detached` and confirms the regression and the fix in one pass — far stronger evidence than just "tests pass". The OOTB-baseline shot also catches "fix accidentally always-appends" failure modes. (4) **`build_snapshot_status_bar` is exercisable as a `#[cfg(test)]` unit test inside `main.rs`** — binary crates can host their own tests, and the function only needs a hand-built `SessionGraphSnapshot` + `ConfigHandle` + `SegmentCache` to reach the integration seam. Cheaper than mirroring the snapshot machinery in `pane_io_integration.rs`.
- **2026-06-29 (task 076 — Sightline TUI QA plugin):** (1) A plugin package can honestly dogfood today's local plugin DX without pretending custom command dispatch exists: make the direct runner the product, use `entry.args = ["--plugin-host"]` for lifecycle smoke, and document `shux plugin run` as future host work. (2) TUI QA reports are only useful when they prove content, not existence: pair raw PTY recording for truecolor/indexed/basic SGR emission with PNG parsing for dimensions, nonblank pixels, grid dimensions, and rendered color samples. (3) Process hygiene must cover unhappy paths too; if a verifier spawns `shux pane record`, put the cleanup guard immediately after `Popen`, not after setup sleeps or stimulus construction. (4) Routine screenshots belong in `.shux/out/` plus PR comments. Committed `.shux/qa` manifests should remain a strict durable-artifact exception, not the default path. (5) Cold-context dogfood is a strong effectiveness gate: `Laghudarshi` caught and fixed seeded Textual TUI issues using Sightline evidence, validating the plugin against the actual agent-friction pattern. (6) Agent discovery should be layered: short trigger hints in the shux skill, detailed Sightline workflow behind a reference, and reusable plugin package bytes in a user cache; `.shux/out/` is for per-repo run evidence, not reusable plugin installs.
- **2026-05-15 (feat/snapshot-font-fallback-emoji — issue #46):** (1) **Council saved a wasted refactor.** Proposal opened with "should we swap fontdue for swash to get colour emoji?" — codex+gemini convergence pointed out the architectural blocker: `shux-vt::Cell` stores one `char` per cell, so the parser already splits ZWJ sequences (`👨‍💻`), VS16 (`🛠️`), regional-indicator flag pairs, and skin-tone modifiers BEFORE the rasterizer sees them. Even with the best COLRv1 rasterizer you can't reconstruct what was split. Colour emoji is gated on a `shux-vt` grapheme-cluster change, not a renderer swap. v1 lands monochrome standalone emoji via fontdue + bundled Noto Emoji — fixes 80% of the user complaint with zero new deps. (2) **No new TOML knob.** Initial design proposed `appearance.font_fallbacks`. Council pushed back: "if `font_fallbacks = []` regresses defaults, builtins must always be appended — then why expose the knob at all?" Just always end the chain with `[primary?, JBM_NF, NotoEmoji]`. `nerd_fonts: bool` stays because it controls **status-bar glyph emission** (what shux writes into the VT), orthogonal to **raster chain coverage** (what the rasterizer renders). (3) **Fallback-font glyph metrics don't match the primary.** Noto Emoji at 14pt is wider than JBM at 14pt. Naively blitting at the primary's baseline + advance spills the emoji into the next cell. The fix is per-glyph: `fit_and_rasterize(font, ch, primary_size, box_w, box_h)` re-rasterizes at a scaled-down font size that fits inside `cell_w * (is_wide ? 2 : 1)`, never enlarging, floored at 6pt. Placement then centers the bitmap within the cell box rather than using baseline metrics. Asserted by `fallback_emoji_glyph_stays_inside_wide_cell_bounds` (renders `"🍺 "` into a 1×3 grid and confirms zero non-bg pixels in column 3). (4) **Hot-reload via `ArcSwap<Rasterizer>`.** Original wiring built one `Arc<Rasterizer>` at daemon startup; font config changes needed a daemon restart. Replaced with `Arc<arc_swap::ArcSwap<Rasterizer>>`; spawned task subscribes to `ConfigHandle::change_notify()` and rebuilds on `appearance.font` change, keeping the last-good rasterizer on rebuild failure (bad path → warn + retain). Snapshot handlers `.load_full()` per call; cost is one Arc clone. `snapshot_font_key` short-circuits no-op reloads. (5) **Validator strict-mirror audit, not just patching the reported field.** `strict::Appearance` was missing `nerd_fonts` + `font`; `strict::Theme` was missing `status_muted` + `status_branch`; `strict::Segment.command` had `#[serde(default)]` while runtime requires it. Added `validate_emitted_default_config_is_ok` (round-trips `cli::DEFAULT_CONFIG_TOML`) so any future template field that lands in runtime but not the strict mirror trips a hard test failure — no more silent drift. (6) **TOCTOU bootstrap race in `notify_waiters()` hot-reload.** `ConfigHandle::replace` uses `notify_waiters()` which only wakes tasks ALREADY parked on `notified()`. A config change between initial rasterizer build and the spawn-task's first `.notified().await` was silently lost. Fix: pass the build-time font key into the task and re-check current config BEFORE entering the await loop. Council review caught this in the implementation diff pass — would have shipped as a latent bug. (7) **`build_snapshot_rasterizer` silent-fallback contradiction.** The old impl logged a warn and silently fell back to bundled JBM when the custom font failed, making the hot-reload `Err` branch unreachable and contradicting the "keep last-good rasterizer" comment. Now returns `Err` strictly; initial-build site does the graceful warn+fallback at startup, hot-reload site keeps last-good on rebuild failure. (8) **`r#"..."#` raw-string with `#hex` colour literals.** `border_focused = "#74c7ec"` inside `r#"..."#` terminates the raw string at the first `"` — compiler errors with "prefix `74c7ec` is unknown". Use `r##"..."##` for any TOML test fixture containing colour hex strings. (9) **Visual evidence cells take ~30s each.** `target/release/shux session create --detached -- sh -lc 'printf "🍺 ...\n"; sleep 600'` + `shux pane snapshot -o <name>.png` is sufficient — no L4 driver needed for monochrome PNG fixtures. The `XDG_CONFIG_HOME=/tmp/...` trick gives isolated config states without polluting the user's real `~/.config/shux`.
- **2026-05-14 (fix/pane-capture-after-exit-and-hyphen-args — agent friction):** (1) **`Pane.exit_status` is the dead flag — keep the VT.** The PTY-task cleanup at `main.rs:323` used to evict the VT from `io_state.vts` the instant the child EOF'd. The Pane stays in the graph (with `exit_status: Some(code)`), but every subsequent `pane.capture` / `pane.snapshot` / `pane.wait_for` returned `not_found: pane VT`. tmux's `remain-on-exit` model is the right one: keep grid + scrollback until the user explicitly destroys the pane; drop only the PTY-bound writer + resizer (so `send_keys` / `set_size` to a dead PTY fail meaningfully). Codex hit this with short-lived commands and had to wrap them in `sleep`. (2) **clap `allow_hyphen_values = true` for any agent-facing string arg that can plausibly take a `--`-prefixed value.** Agents wait for CLI help text, error strings, and flag names; `shux pane wait-for --text '--search'` silently failed with "unknown flag --search" until you knew to write `--text=--search`. Apply to `--text` / `--regex` on wait-for, `--text` on send-keys. Tradeoff: `allow_hyphen_values` lets a missing value adjacent to a flag silently consume the next flag (`--text --absent` becomes `text="--absent"`); acceptable here because (a) the arg requires a value, so clap still errors on a bare `--text` at end-of-args, and (b) the reported friction is exactly the leading-`--` value pattern. (3) **Test-scaffold mirror discipline.** `pane_io_integration.rs` has its own `run_pane_pty_task` mirroring `main.rs`. When `main.rs` cleanup changes, the scaffold MUST follow or tests pass against buggy mirror-of-buggy-prod logic. Same hazard as the duplicated `register_*_methods` in test files (PR3b lesson). (4) **TDD discipline pays here.** Two red tests written FIRST against current code (`UnknownArgument` for clap, `not_found: pane VT` for capture) made the symptom unambiguous and gave a green oracle for the fix. (5) **`#[arg(allow_hyphen_values = true)]` is per-arg, not crate-wide** — clap doesn't expose a top-level toggle, so opt in deliberately on the args where leading-`--` content is plausible.
- **2026-05-16 (fix/session-create-client-cwd):** Interactive CLI defaults must be owned by the client, not inferred by the daemon. A long-lived daemon has incidental cwd from whichever command first spawned it; `shux session create` must send the caller's cwd explicitly every time (and `--cwd` is just an override). Regression proof should start one isolated daemon from directory A, then create another session from directory B and assert both graph `pane.cwd` and PTY `PWD` are B. On macOS, `/tmp` renders inside the PTY as `/private/tmp`, so visual/e2e checks should compare physical paths (`pwd -P`) rather than symlink spelling.
- **2026-05-17 (fix/statusbar-starship-raw-ansi):** `starship prompt` is shell-shaped even when used as a statusbar command. With default/Bash detection it wraps ANSI escapes in Bash PS1 non-printing guards (`\[` / `\]`); zsh/tcsh use `%{...%}`. A shell consumes those as prompt metadata, but shux's statusbar runner parses stdout as terminal bytes, so the guard markers render literally. Inline `starship_config` segments should default their spawn env to raw ANSI mode (`STARSHIP_SHELL=cmd`, `TERM=xterm-256color`) while preserving explicit user overrides. Regression coverage needs both runner-env tests and a generated-config test so `shux config init/show` cannot drift back to shell-guard output.
- **2026-05-17 (fix/session-create-pane-title):** Session names and pane border titles are separate UX surfaces: the session name belongs in lists/status bars, while the top border title belongs to the initial pane and can be pinned with the same manual-title machinery as `pane.set-title`. A `session.create` convenience field should set `manual_title` before PTY spawn so OSC updates from shells/apps cannot immediately overwrite the user's requested label. Regression proof should assert both RPC state (`title` + `manual_title`) and a `window.snapshot` PNG from an isolated `/tmp/shux-demo` launch.
- **2026-05-18 (task 061 — render parity + mouse copy):** Crossterm's `EnableMouseCapture` is broader than shux needs: it emits `?1000h`, `?1002h`, `?1003h`, `?1015h`, and `?1006h`. Any-motion tracking (`?1003h`) is unnecessary for SHUX's current click-to-focus / border-drag model and can interfere with host-terminal modifier selection. Prefer an explicit mouse-capture profile (`?1000h` + `?1002h` + `?1006h`) and keep `DisableMouseCapture` for cleanup. For copy UX, reuse `CopyModeState` as the selection geometry/data type, but keep normal mouse selection as separate attach-layer state from modal copy mode; visible text copy must not trap keyboard input.
- **2026-05-18 (tasks 031/062/063 — human interactive core):** (1) Copy mode should treat `scrollback + visible` as one logical row space and render an overlay view only in live attach; snapshot RPCs should stay literal VT snapshots unless the API explicitly asks for an interactive mode. (2) Search in copy mode needs the current `VirtualTerminal` at key-handling time; routing attach input through a registry/helper that can borrow the focused VT avoids duplicating scrollback text extraction. (3) Attach keybinding config is best split into root and prefix tables, with config strings like `"prefix c"` normalized against the configured prefix; this keeps tmux-style config readable while preserving the existing action model. (4) Session save/restore should reuse the existing template + `state.apply` lowering path instead of inventing a second persistence format; exported split ratios should be rounded for human review because raw `f32` serialization leaks values like `0.44999998807907104`. (5) Dogfood scripts should write to `.shux/out/` and use `pane wait-for` against the active pane after template apply — split templates often leave the newly-created pane active, so waiting on the first pane's marker can falsely time out.
- **2026-05-18 (fix/copy-selection-and-cursor-churn):** (1) Copy-mode selection overlays must redraw the selected glyphs from the VT, not just paint a background rectangle. A block overlay can make text illegible depending on theme contrast; the regression should assert both the selected text byte and the expected fg/bg ANSI are present. (2) A render loop with a fallback tick must not emit cursor hide/show/move bytes when the framebuffer diff is empty and the cursor target is unchanged. Track terminal cursor state inside the compositor; first render initializes state, dirty frames hide-before-diff, idle frames emit nothing, and cursor-only movement emits just `MoveTo`. (3) Expect harnesses do not populate `log_file` unless output is actively read; `after` sleeps are not reads. Use fixed-duration drain loops and enable `log_user 1` while redirecting expect stdout to `/dev/null` so proof logs are captured without flooding CI output. (4) Parent agent environments can poison pane color (`TERM=dumb`, `NO_COLOR=1`). Multiplexers should normalize pane PTY env (`TERM`, `COLORTERM`, `CLICOLOR`, remove `NO_COLOR`) while still letting explicit per-pane env override those defaults.
- **2026-05-19 (test/coverage infra):** On macOS, `cargo-nextest` discovery can leave freshly built test binaries parked in dyld before libtest emits `--list --format terse`; direct binary execution may still work, so a simple exec preflight is not sufficient. Keep CI on nextest if stable there, but route local Makefile/pre-push tests through serial `cargo test -- --test-threads=1` plus a process-tree watchdog so local pushes cannot wedge, orphan sleeping discovery children, or trip plugin handshake timing races. **SUPERSEDED 2026-08-07 (issue #130).** This is the entry that installed the serial house pattern, and it cost 461s a run on every local gate since May. Two of its three fears were misdiagnosed: the "orphaned sleeping children" and the "plugin handshake timing races" were both the `Child::kill` process-tree leak in `crates/shux-plugin/src/lib.rs`, not a nextest problem — the serial suite simply ran long enough for the orphans to expire before anyone looked. `make test` is nextest now, matching CI, with machine-global resources bounded by test groups instead of by serializing everything.
- **2026-05-20 (fix/shux-launch-lag):** Multiplexers should not advertise `TERM=xterm-256color` unless they implement xterm request/response behavior. Some CLIs probe xterm-like terminals and wait for a timeout when no emulator answers; `tmux-256color` avoids that class, matches multiplexer semantics, and preserves richer TUI capabilities than `screen-256color` (for example italics). Guard that default with a runtime terminfo check (`tmux-256color` → `screen-256color` → `xterm-256color`) so minimal hosts do not inherit an unknown terminal type. Benchmark terminal startup through a controlling PTY, not only a raw PTY, because app probing behavior can differ.
- **2026-05-20 (feat/xterm-256color-support):** Truthful `TERM=xterm-256color` support requires a bidirectional VT layer, not just better rendering. Parse app-emitted DA/DSR/OSC/DCS probes, return response bytes from `VirtualTerminal::process_with_responses()`, and write them back to the PTY after releasing the pane I/O mutex. Keep the test-only PTY task mirror in sync with `main.rs`; otherwise integration tests can miss response-path regressions.
- **2026-05-20 (feat/xterm-256color-sync):** Modern TUI frameworks now treat synchronized output as part of terminal capability negotiation, not a nice-to-have. Bubble Tea v2 queries DECRQM mode 2026 and can use `CSI ? 2026 h/l` around frames. If shux reports mode 2026 support, it must freeze the presented pane grid/cursor while the app writes the frame and expose the working grid only on reset; just accepting the escape sequences would still allow partial-frame screenshots/attach renders.
- **2026-05-27 (fix/vt-cursor-save-restore):** Bubble Tea/Charm renderers can rely on multiple cursor-save families in the same frame path: DECSC/DECRC (`ESC 7/8`), SCO SCOSC/SCORC (`CSI s/u`), and DEC private 1048/1049. Treat no-parameter/all-zero `CSI s/u` as save/restore, but ignore parameterized forms such as Kitty keyboard `CSI 27;2u` so input protocol bytes do not clobber the saved cursor. Keep 1047/1048/1049 semantics separate: 1047 switches buffers only, 1048 saves/restores cursor only, and 1049 composes save/restore with alternate-screen behavior. For test harnesses, never capture `$?` after an `if cmd; then` branch; store the command status immediately with `cmd || status=$?`, or failed test binaries can be reported as success.
- **2026-05-27 (fix/vt-renderer-primitives):** Stale-cell redraws in optimized Bubble Tea/Charm output can come from missing renderer primitives even when cursor save/restore is correct. Support `REP` (`CSI Ps b`) because renderers may clear old text by writing one space then repeating it; ignoring `REP` clears only the first cell and leaves a stale prefix. Also handle cursor tabulation (`CSI I`/`CSI Z`) and relative moves (`CSI a`/`CSI e`) so optimized diff batches keep their intended column/row alignment. When re-entering `?1049h` while already on alt screen, preserve the just-saved cursor through the alt-grid clear/reset path so the matching `?1049l` can restore it.
- **2026-05-27 (fix/vt-renderer-parity):** Check Charm/Ultraviolet renderer capability bits directly when closing Bubble Tea redraw bugs. For tmux/kitty/wezterm-style terms it may emit `VPA`, `HPA`, `CHA`, `CHT`, `CBT`, `REP`, `ECH`, `ICH`, `SD`, and `SU`; missing any one can surface as stale cells or shifted diff batches. `HPA` (`CSI Ps \``) is especially easy to miss because `CHA` (`CSI Ps G`) looks equivalent, but the renderer can choose `HPA` first. Extended rendering metadata from the same stack includes OSC 8 hyperlinks plus SGR underline style/color (`4:n`, `58`, `59`), so preserve those in cell extended attributes when present.
- **2026-05-27 (fix/vt-extended-render-state):** If VT cells learn a new extended attribute, carry it through render-buffer equality and output state in the same patch. Storing OSC 8 or underline color/style only in `shux-vt` is insufficient: `shux-ui::RenderCell` diffing will otherwise treat metadata-only changes as clean, and the attach renderer can leave terminal hyperlink/underline state stale. OSC payload splitting is also semicolon-sensitive; reconstruct OSC 8 URI payloads from all fields after the params field.
- **2026-05-27 (issue-63 Bubble Tea / Charm coverage):** Bubble Tea requests cursor color with OSC 12 alongside OSC 10/11 foreground/background probes, and Charm's VT models OSC 12/112. Treat dynamic cursor color as part of the same terminal-default color state, then carry it through every visible path: VT query response, live attach cursor presentation, teardown reset, pane snapshot, and composed window snapshot. Cursor shape has the same rule — parsing `CSI Ps SP q` is not enough if attach and raster still force a block. For visual proof, include a synchronized-output held-frame capture that asserts the working frame is not visible until `?2026l`.
- **2026-06-08 (issues #65/#66 snapshot text-symbol fallback):** Missing PNG glyphs split into two scopes: tactical scalar-glyph coverage vs renderer-v2 parity. A bundled/configurable fallback chain can cover common TUI symbols (`↻`, braille spinners, key legends, status glyphs, blocks, box drawing, geometric markers) while preserving primary metrics, but it cannot solve composed emoji, variation selectors, shaping, color glyphs, or exact Ghostty/iTerm parity. Track that larger work separately. For isolated shux daemon proof, set `XDG_RUNTIME_DIR`; `--socket` is client-side and the daemon derives its actual socket from the runtime dir.
- **2026-06-11 (issue #69 attach color):** Crossterm color commands are not suitable for pane terminal-emulation bytes if the daemon may inherit `NO_COLOR`; `SetForegroundColor` / `SetBackgroundColor` serialize empty color SGR when crossterm's process-global color-disabled flag is set. Do not fix this with `force_color_output(true)` because it mutates global CLI color policy. Attach rendering should own local ANSI fg/bg/underline color serialization so pane colors survive while ordinary shux CLI output can still respect `NO_COLOR`. Regression proof should include both unit-level disabled-global tests and a release-binary PTY attach smoke under daemon `NO_COLOR=1`.
- **2026-06-11 (issue #70 lossless pane recording):** Keep sampled observation and lossless audit as separate primitives. `pane.output.watch` should stay cheap, sampled, and clearly labeled; absence-of-bytes audits need a source-level recorder that tees raw PTY bytes before VT processing and before sampled coalescing. A lossless path must fail closed: report `complete|error|aborted`, byte count, and error detail; do not let writer failure kill the pane or silently degrade into a false-pass audit. Bound the blast radius with one active recorder per pane, daemon-side duration limits for scripted use, client-side path absolutization, create-new defaults, and real TUI proof plus exact SHA verification.
- **2026-06-12 (task 067 — VT resize reflow):** `Row.wrapped` is a source-row flag: row `N` soft-wraps into row `N+1`. Reflow must reconstruct logical lines across the entire `scrollback + visible` grid, trim only default trailing cells on hard-break tails, and keep wide-cell heads atomic by moving a width-2 cell to the next row when it would start in the final column. `VirtualTerminal::resize()` needs separate policies for active primary/synchronized presentation (cursor-aware reflow) and alternate screen (fixed-canvas resize while saved primary reflows). Visual resize proof is strongest when a real shux pane returns to the same size and exact pixel comparison proves the before/after PNG is identical. Integration probes that launch shells through PTYs need realistic wait budgets under full-suite load; too-short waits create false failures that look like VT response regressions.
- **2026-06-12 (task 067 review fix — styled blanks and PTY cleanup):** Erased cells with non-default styling are still visual blanks. Resize reflow must trim trailing cells by visible content (`ch != ' '` or wide continuation), not by full `Cell::default()` equality, or colored erase tails can become fake wrapped content and push following hard lines down. Pane I/O test mirrors must also reap cancelled PTY children; otherwise orphan login shells accumulate under full-suite runs and create misleading PTY spawn/register failures.
- **2026-06-12 (task 068 — wide-cell invariants):** Treat width-2 characters as an atomic row invariant, not a write-only concern. Every bulk mutator should either expand through intersecting wide pairs (`ECH`/erase) or preserve terminal geometry then sanitize (`ICH`/`DCH`). Final-column width-2 writes need explicit auto-wrap-on and auto-wrap-off behavior, and alternate-screen/fixed-canvas resize must sanitize truncated heads/tails. For visual gates, keep expected PNGs in committed `.shux/goldens/` and mirror them into `.shux/qa/`; same-run `actual -> expected` inside QA is a circular proof even when pixel diff is exactly zero.
- **2026-07-09 (task 077 — lens P3 `pane.wait_settled`):** An event-driven "wait until quiet" is a `watch::Receiver` loop, not a poller. Subscribe under the io lock, clone the receiver out, DROP the lock before any `.await` (mutex-across-await is the daemon's cardinal deadlock), then `select!` between `rx.changed()` and `sleep_until(min(quiet_deadline, timeout_deadline))`. `subscribe()` seeds the receiver with the current published value AND catches every later publish, so an already-quiet pane returns immediately with no lost-edge race — this is exactly why the P1 substrate is a `watch`, not a `Notify`. The settle clock is a TRAP: `last_mutation_ns` is stamped by shux-vt's process-local monotonic epoch (`static START: OnceLock<Instant>`), so "now" MUST come from the SAME function (expose it `pub`, don't roll a second clock) or the `now − last ≥ quiet_ms × 1_000_000` comparison mixes epochs. Keep the unit conversion explicit and ns-on-both-sides; the councils flagged the ns/ms mixup as a recurring bug class. Class-B immunity (titles/bells/DECSCUSR) is FREE if you watch only the published `(revision, ns)` pair — the P1 publisher only sends on a content_revision change, so metadata churn never wakes the waiter. Timeout is a server-side monotonic deadline from request acceptance and returns `{settled:false}` as a RESULT (CLI maps to exit 1), never an RPC error. A golden for a settle-anchored frame (S1's `s1_ready.png`) can be minted in a phase that changes no rendering code — it's the prior-approved raster on a frozen fixture — but mark it PROVISIONAL and defer §16.3 approval to the downstream QA/council chain; the implementation must not self-certify its own expected PNG. Handy: `assert_png_golden` writes the actual PNG to `$TMPDIR/lens_actual_*.png` BEFORE it fails on a missing golden, so a red run is also the mint source.
- **2026-07-09 (task 077 — P3 review round, settle + rpc cancellation):** Three lessons with reach beyond lens. (1) Deadline precedence in any wait-until-quiet loop: decide from a PURE priority function (pending-fresh-state > success-condition > timeout > keep-waiting) evaluated at the top of every wake — returning timeout from inside a sleep arm without re-checking the success condition mis-times the `success == deadline` and late-scheduler-wake cases; `watch::Receiver::has_changed()` after `borrow_and_update()` is the cheap TOCTOU guard that keeps a stale snapshot from settling. (2) shux-rpc's connection loop awaited dispatch inline, which silently coupled EVERY long-running RPC's lifetime to handler completion, not client liveness — the fix (read task feeding a bounded queue + executor racing each dispatch against a per-connection CancellationToken) preserves serial response ordering while making disconnect drop the in-flight future. Any long-poll RPC (events.watch, wait_for, wait_settled) gets this for free now; a bounded queue means heavy pipelining can delay EOF detection, which is an accepted trade. Watch out for the semantic change: a fire-and-forget request whose client disconnects immediately may now be dropped instead of executed — no shux client does this, but it is the one behavioral delta. (3) `watch::Sender::receiver_count()` is a free, honest "live waiter" observable for tests when receivers exist only inside request futures — no debug RPC surface needed; pair an in-process receiver_count assertion (discriminating) with a black-box CLI-SIGKILL health smoke (end-to-end) when the daemon's logs go to /dev/null. Also: `and_then(as_u64).unwrap_or(default)` on RPC params silently swallows mistyped values — strict-parse helpers (absent → default, present-but-wrong-type → INVALID_PARAMS) should be the default pattern.
- **2026-07-10 (task 077 — lens P6 skill/CLI polish + golden minting):** (1) **"Hidden from `session.list`" and "resolvable by the CLI" are two different things that a single default-params RPC call can conflate.** `resolve_pane_window_id`'s "no `--window` given, use the session's active window" fallback queried plain `session.list` (no `include_scratch`) — so ANY pre-lens pane command (`send-keys`, `set-size`, ...) silently couldn't find a scratch session's active window even after fixing UUID resolution itself, because the lookup that finds "which window is active" is a SEPARATE call from the one that resolves "is this id valid" and both need the visibility flag independently. When a feature adds a new hidden-by-default entity class, grep for every INTERNAL, not just external-facing, listing call and check each one's default filter — the externally-documented default (`session list` omits scratch) can leak into helper code that has no business applying it. (2) **A "looks like an ID" short-circuit in a resolver is wrong the moment the OTHER namespace can legally contain ID-shaped strings — validate against the live namespace, don't trust the shape.** (CORRECTED mid-review: the first version of this entry claimed the pure `Uuid::parse_str` short-circuit "can never mask a real error" because downstream RPCs validate existence — both codex and claude refuted it: session NAMES may legally be UUID-shaped strings, so the short-circuit made such sessions unaddressable via `-s` and, worse, `Uuid::parse_str` also accepts the 32-hex simple form and any case, so a hex-shaped NAME could normalize onto a DIFFERENT session's real id and mistarget it.) The sound pattern: parse → NORMALIZE (`Uuid::to_string()`, hyphenated lowercase — ids serialize canonically, raw string equality misses simple-form/uppercase input) → resolve as id FIRST against the live list → fall back to name lookup on no id-match → document the precedence (id wins on double-match, warn when cheaply detectable) → pass the normalized form through on no-match so the server emits its canonical not-found. Cost: one list round-trip, same as the name path always paid. (3) **A real TUI's behavior can drift out from under a frozen black-box test even when the binary is genuinely installed and running — "not installed" and "installed but incompatible" need different handling.** `nidhi 0.1.0-alpha.1` grew a mandatory "Press Enter to continue" welcome screen after the frozen T1/T2/T3 harness was authored; the tests' `wait_for` sentinel never appears until that screen is dismissed, so they time out at 10s every run, indistinguishable at a glance from "nidhi isn't installed" (`skip_unless_bin`'s loud-skip path) but actually a completely different failure needing a completely different fix (add a dismiss step, don't skip). Diagnosis process that generalizes: capture the pane's actual text/PNG at the point of the failing wait BEFORE assuming the test's premise is still true — `pane.capture`'s `last_capture_preview` in the timeout error payload was the tell (empty, meaning zero of the expected content had rendered yet, not "rendered wrong"). Strings-scan the installed binary for config/env keys before concluding "no bypass exists" — cheap and definitive for a Go binary with embedded const strings. (4) **A byte-identical golden across two semantically-different test scenarios is a FEATURE to look for, not a red flag.** `e1_glance.png` (E1's fresh-launch frame) and `k1_pos3.png` (K1's frame after exactly 3 Tab presses) hashed identical because F4's focus marker cycles through exactly 3 grid positions and both scenarios land on the cycle's start — independently minted from two different driver scripts, two different daemons, agreeing byte-for-byte is stronger evidence of correctness than either mint alone. When minting goldens for a fixture with cyclic/periodic behavior, check whether any two of your new goldens SHOULD collide by the fixture's own design before treating a collision as a copy-paste bug. (5) **NO_COLOR suppresses an app's ANSI text colors, not its theme-background OSC — and a pixel predicate authored blind can be unsatisfiable by construction.** T3's strict `is_grayscale_png` (every pixel R==G==B), written in P0 before any real render existed, could NEVER pass: nidhi emits OSC 11 (theme background, rendered RGB(7,9,14), channel spread 7) even under `--no-color`+`NO_COLOR=1`, and shux-raster's own `bg_default` `[16,16,24]` has spread 8 — so even a perfectly color-suppressed frame fails strict grayscale. Two generalizable rules: (a) when a strict pixel assertion fails but the golden byte-matches, suspect the PREDICATE, not the render — measure the actual distribution (max channel spread, count above threshold) on both the passing and failing siblings before proposing a fix; the measured separation here (nocolor max spread 7/zero >8 vs color max spread 159/8,651 >8) made the <=8 threshold an evidence-anchored choice rather than a magic number, and the council required the anchors IN the doc comment plus a discriminating control (the color sibling must FAIL the predicate with meaningful signal, exact count unpinned). (b) `strings <binary> | grep ']11;'` is a fast way to check whether a Go TUI sets terminal colors via OSC regardless of NO_COLOR — the NO_COLOR convention governs SGR text attributes, and apps legitimately keep their OSC theme handshake. (6) **Agent-facing API docs drift into a recognizable failure class: shapes that describe the CLI's ERGONOMICS instead of the RPC's actual WIRE CONTRACT — sweep them against the handlers, not against what feels right.** The skill's api.md claimed `session.kill {"id": "name-or-uuid"}` (server parses `id` STRICTLY as UUID; names go via the separate `name` field — an agent following the doc got INVALID_PARAMS), `window.create {"title"}` (real param is `name`), `window.focus {"session_id", "window_id": "uuid-or-index"}` (real shape is bare `{"id"}`, no index at RPC level — index resolution is CLI-side sugar), wrapper objects for `window.list`/`pane.list` (both return BARE arrays), and `pane.list {"session_id"}` as "all windows of the session" (really the ACTIVE window only, via resolve_window_id_from_params → session.active_window). Every one of these is the same root mistake: documenting the flexible client-side resolution layer as if it were the server contract. The five-minute audit that catches the whole class: for each documented method, grep the handler registration and read ONLY the `params.get(...)`/`invalid_params` lines — field names, strictness, and response shape (bare vs wrapped) fall straight out; anything the doc adds beyond that is CLI sugar and must be labeled as such.
- **2026-07-10 (task 077 — lens P5 scratch sessions + `lens.run`):** (1) **Detect child-process exit event-driven, not by polling — the daemon already fires the event.** `run_pane_pty_task` calls `graph.set_pane_exit_status()` on natural exit, which fires `EventData::PaneExited{exit_status,..}` on the SAME `EventBus` `events.watch` reads. A composite RPC that needs "block until this specific pane's process exits" (`lens.run{wait:true}`, and any future reap-on-exit timer) should `event_bus.subscribe_filtered(["pane.exited"])` and loop `recv()` checking `pane_id`, never poll `pane.exit_status` on a timer. Subscribe BEFORE spawning the PTY — a `tokio::sync::broadcast::Receiver` only sees events published after it exists, so a fast-exiting child (spawn → print → exit in a few ms) can race ahead of a subscription created after spawn. Two independent subscribers (one for the reaper's `post_exit_ttl_ms` timer, one for the RPC's own `wait:true` branch) is simpler and race-free-by-construction versus trying to fan one subscription out to two consumers. (2) **A composite "run" RPC does not have to call the primitives it composes with in the product loop.** The instinct is to read `lens.run`'s conceptual place in "run → settle → glance → drive → diff" as "the RPC handler calls wait_settled/glance internally" — but the ACTUAL normative response schema (`{session_id, pane_id, revision}`, no png/text) says otherwise: those are separate RPCs the AGENT chains across multiple calls (proven by the frozen E1 test's own call sequence). Read the wire contract before assuming a natural-language product description maps 1:1 onto internal call structure. (3) **Reuse the existing kill escalation instead of re-implementing killpg.** `run_pane_pty_task`'s cancellation branch already does `handle.terminate()` (SIGHUP via `killpg`) → 500ms grace → `handle.kill()` (SIGKILL via `killpg`) whenever its shutdown token is cancelled — exactly the graceful-then-force contract a "reap" feature needs. Calling `state.teardown_panes(&[pane_id], true)` (which cancels that token) + `graph.destroy_session()` gets the full reap for free and can't drift from how every other kill path (session.kill, window.kill, pane.kill) already behaves; only a TRUE cross-daemon-restart orphan (no live PTY task to escalate through, e.g. `ScratchRegistry::startup_reap`) needs its own direct `killpg` call. (4) **`\u{XXXX}` escapes only expand inside real Rust string/char literals, not doc comments.** A `///` line is not processed as a string literal — `\u{2208}` in a doc comment prints as the LITERAL 8 characters `\u{2208}`, not `∈`. `--help` output caught this immediately; the fix is to either type the real UTF-8 character directly in the source (works fine in doc comments) or move the text into an actual `&str` literal (`const FOO: &str = "\u{2208}"`, which the existing agent-help/long-about render functions already do correctly). (5) **`std::process::Command::spawn()` failure IS the synchronous argv[0]-not-found / bad-cwd detector.** No extra PATH-resolution or cwd-existence pre-check is needed before calling `PtyHandle::spawn` — Rust's std `Command::spawn()` on Unix uses a self-pipe around fork+exec specifically so a failed `execvp`/`chdir` in the child is reported back to the parent as a synchronous `Err(io::Error)`, not a silent background failure. This is exactly what LENS-R-045's "spawn is SYNCHRONOUS inside the RPC" requires — map the `Err` straight to `SPAWN_FAILED`.
- **2026-07-17 (task 078 — lens gate: capture schema + frozen contract):** (1) **A VT's stored `cell.width` is NOT `UnicodeWidthChar(base_scalar)` for all graphemes, and NOT `UnicodeWidthStr(cluster)` either — so a validator that re-derives a captured grapheme's width from the string will reject valid captures.** Two failure modes, opposite directions, both ubiquitous in real CLI output: VS16 emoji-presentation (`❤️` = U+2764 U+FE0F) is stored width-1 (base-scalar width) but `UnicodeWidthStr` sums the cluster to 2; regional-indicator flags (`🇺🇸`) are stored width-2 though the base scalar `🇺` is `UnicodeWidthChar`-width 1 (the VT special-cases flags/ZWJ beyond base-scalar width). The only source that always agrees with the encoder is the VT's own `cell.width`, which the wire format encodes as the explicit `""` continuation (R3 "geometry is self-describing"). Resolution: the validator TRUSTS the `""` structure for multi-scalar graphemes and only width-checks SINGLE-scalar entries (where `UnicodeWidthChar` is authoritative — enough to reject a spurious `""` after a narrow char, or a wide single scalar like `漢` missing its `""`). Decode must never consult `unicode-width` at all. This took two adversarial rounds to fully surface (VS16 first, flags second) — the general rule: **when serializing terminal cells, the width is the VT's, not any string-width function's; make geometry self-describing and never re-derive it.** (2) **Adversarial subagents that DRIVE THE REAL SYSTEM find bugs that static reasoning and design councils miss.** Both width bugs above were dismissed as "theoretical" by the author AND passed over by 5 design councils; an agent that ran `❤️`/`🇺🇸` through the real VT found them in minutes. Codified as the `adversarial-review` skill + feature-protocol step 5. The discipline that matters: charter each agent to break ONE disjoint surface, tell it to drive the real system (not read source), and INDEPENDENTLY REPRODUCE every finding before believing it (agy's 3 findings were 2-real-1-test-bug; adv-schema's were all real). (3) **A QA subagent with `Bash` in a SHARED working tree can corrupt your git.** The `shux-vt-solid-qa` gate, to test the freeze guard, ran `git checkout -b qa-freeze-probe` + made commits + switched back — which moved HEAD and orphaned a commit I made concurrently (recovered via `git reflog` + cherry-pick). Constrain audit agents to read-only git, or make them use an isolated `git worktree add /tmp/...`; never let a concurrent agent mutate the checked-out branch. (4) **Extending a frozen-path guard for a sub-namespace is NOT additive.** `check-lens-frozen.sh`'s `crates/shux/tests/lens_` prefix already matched the new `lens_gate_*` paths, so a gate-only commit with a `GATE-TEST-CHANGE:` trailer would have FAILED the lens lane. A two-lane guard needs the sub-namespace lane tested FIRST and the parent lane's regex tightened to EXCLUDE it. Also: the guard failed OPEN in a shallow CI clone (unresolved base → HEAD-only check a multi-commit PR slips past) — a range-mode guard must fail CLOSED in CI when the base ref doesn't resolve. (5) **The repo's mechanism for a quarantined red-test lane is `[[test]] test = false` in `Cargo.toml` + a dedicated `make` target, NOT `#[ignore]`** (the lens suite comment says so explicitly) — because `make test` runs `cargo test` while CI runs `cargo nextest run --workspace` directly and the two don't share a filter, and daemon-backed tests must stay out of CI's PARALLEL default run. Pure-schema unit tests live in the lib crate and DO run in CI; the daemon-backed dogfood + the red contract lane are `test = false`. A separate `lens_gate_exit_contract` is a NORMAL (CI-run) frozen target that pins the exit-map VALUES independently, so weakening the map fails CI (the RED-lane rollup test used `worst()` as its own oracle and could never catch a wrong exit map).
- **2026-07-17 (task 079 — lens gate: one comparator in `shux-vt`):** (1) **Lifting a concrete-typed diff onto a view trait needs WRAPPER views, not `impl for &Grid` — a bare `Grid` has no ambient state.** OSC defaults, cursor, and the OSC-4 palette-history bit live on the `VirtualTerminal`, not the `Grid` (the daemon already threaded them as separate params). So `CellGridView` is implemented by `GridFrame<'a>{grid, defaults, cursor, palette_overridden}` (live) and `FrameView` (golden), never `&Grid` directly — the task text's literal "impl for &Grid" was the design escalation it anticipated. `cell()` returns an OWNED `CellRef` (a newtype over `Cell`, equality value-exact to `Cell: Eq`) so the golden view never has to hand out a borrow into its RLE-decode buffer; the object-safe `&dyn CellGridView` diff then has ONE code path over both. Both view impls must make `cell(r,c)` TOTAL (out-of-range → `Cell::EMPTY`): `GridFrame` first shipped `visible_row(row)` which PANICS on an OOB row (impl-review MINOR) — use the Option-returning `grid.row(scrollback_len()+row).and_then(|r| r.get(col))` to honor the contract. (2) **`grid.row(r)` is ABSOLUTE (index 0 = oldest scrollback); `visible_row(r)` = `raw[scrollback_len()+r]` is the viewport — a "capture the visible frame" function that iterates `0..rows()` (the viewport count) but indexes `grid.row(r)` silently snapshots the OLDEST scrollback for any scrolled pane.** This latent bug sat in task-078's `build_row_runs` (`FrameEnvelope::from_terminal`) because no 078 capture test ever scrolled — `scrollback_len()==0` masks it entirely — and would have corrupted every scrolled golden (build logs, `ls`, `cat`) in task-080's gate. The daemon's `pane.diff_since` was immune only because it feeds `clone_visible()` grids (scrollback-free) on both sides. Fix: `grid.row(grid.scrollback_len() + r)`. General rule: **any absolute-vs-viewport index in a capture/snapshot path is a scrollback bug waiting for the first test that scrolls — write that test.** Found by the adversarial view-equivalence agent (`diff_frames(GridFrame(F), FrameView(capture(F)))` must be a fixed point), not by design review or the full test suite. (3) **A refactor's parity oracle must be minted by the OLD function BEFORE it is deleted, hand-mapped to the new shape (not via the new type's `Serialize`), then frozen — and its independence is process-only once the generator is gone.** The parity corpus (`.shux/fixtures/lens-gate/parity/`) was generated by the pre-extraction `compute_lens_diff` over live grids and asserted bit-for-bit against the moved `diff_frames`; regenerating from the new function would make it tautological, so the generator is deleted (keeping it = the two-implementation hazard the extraction removed) and a committed `README.md` + the `GATE-TEST-CHANGE:` freeze trailer carry the provenance. Keep parity (preserved semantics) SEPARATE from divergence fixtures (new behavior: `geometry_changed`, `palette_overridden_differs`, cursor/blink) — the size-mismatch case can't be a parity entry because the old fn had no `geometry_changed`. (4) **A general golden-vs-live comparator must NOT inherit assumptions valid only inside its original caller.** The old `compute_lens_diff` min-cropped to the smaller grid safely because `pane.diff_since` invalidates resize/alt checkpoints before diffing; a gate comparing arbitrary frames has no such guarantee, so `geometry_changed` is first-class (min-crop for diagnostics, but the flag is decisive) — the council-#1 BLOCKER. Likewise `palette_overridden` is a STICKY HISTORY bit, not palette state: the diff can only report the bits DIFFER (`palette_overridden_differs`), never "palette changed", and it must NOT fold into `cells_changed`; portability is a per-frame check (`overridden && has_indexed`, OR'd), which the gate (080) owns — not keyed on the diff field (both sides can be `true`, so `differs==false`, yet the frame is unportable). (5) **A faithful lift PRESERVES pre-existing quirks — pin them, don't "fix" them.** A coloured wide glyph stores its head bg concrete but its spacer bg `Default` (wide-continuation cells carry default style), so an OSC-default-bg change flips the spacer and wide-pairing propagates to the concrete head — an over-report the old fn already had. Under a "byte-identical output" DoD, changing it would break the refactor contract; the right move is a pinning test (`default_bg_change_flips_colored_wide_head`) + an honest doc note, and leave the behavior for a follow-up. (6) **`try_view` must VALIDATE before it decodes, and CAP the size before it allocates.** `to_cells()` is intentionally reconstructive/tolerant (it truncates an over-width run silently), so the comparator's golden entry point must call `validate()?` first — and no test asserted it did, so deleting that one line would silently normalize malformed goldens into false-passes with the whole suite green (adversarial A-F2 regression guard now pins it). It must also reject a schema-valid-but-absurd size (a `65535×65535` golden = ~95 GB → allocator ABORT, uncatchable) with a typed error, not let `to_cells` OOM — R9's "typed error, never a panic" applies to the eager-decode consumer too.

- **2026-07-17 (task 080 — lens gate: capture emission + 3-tier golden compare):** (1) **A per-frame "is this indexed colour palette-mapped?" portability check must scan EVERY channel the rasterizer resolves through the palette — including the extended UNDERLINE colour, not just fg/bg.** `has_indexed_colors` originally checked `cell.fg()`/`cell.bg()` only, but `shux-raster` renders an indexed undercurl colour (`SGR 58;5;N`, the nvim/LSP error-squiggle path) through the SAME `indexed_to_rgb` as fg/bg — so a frame with a DEFAULT-fg/bg cell whose only indexed colour is the undercurl, under an OSC-4 override, is genuinely unportable yet certified as a portable `cell`-tier match: the exact D8 "silent false pass" the design forbids. Found by the adversarial determinism/palette agent driving the real `has_indexed_colors` + inspecting the real raster path; fixed by also scanning `cell.extended.underline_color`. General rule: **a portability/completeness predicate over "does X depend on palette/theme" must enumerate every field the RENDERER consumes, and the way to find the gap is to diff the predicate's field set against the rasterizer's `resolve_color` call sites.** (2) **A mask redacts CELL content, but frame METADATA derived from that content can still leak it — the cursor column encodes a masked secret's LENGTH.** The cursor lands just after a printed secret; captured unmasked, its column flows into `capture_sha256` (cell tier) AND `rgba_sha256` (pixel tier), so two different-length secrets behind an identical fully-covering mask produce different hashes — a length side-channel that defeats the D4 invariance for both content pins, with zero existing test coverage (the mask tests all used same-length secrets, so the cursor never moved). Fix: clamp a cursor that falls INSIDE a masked rect to the mask origin (`MaskSet::cursor_redaction_col`) in every capture constructor. General rule: **when you redact a region, redact the derived metadata that points INTO it (cursor, selection, scroll offset), not just the cells; and test invariance with DIFFERENT-length hidden payloads, never a same-length pair (a same-length pair leaves every position-derived field unchanged and hides the leak).** (3) **A derived `PartialEq` over an `f64` field is a latent false-inequality bug for any equality-based staleness/cache check — `NaN != NaN` makes a same-config golden eternally stale, and `f64::to_bits` "fixes" NaN but REGRESSES `-0.0`/`0.0` (different bit patterns, semantically equal).** The correct comparison for a staleness key is `a == b || (a.is_nan() && b.is_nan())` — `==` already unifies `-0.0`/`0.0` and every finite value, and the reflexive-NaN arm adds NaN without the `to_bits` regression. Also: `serde_json` silently coerces a `NaN` field to `null` on write and then REJECTS it on read, so a NaN-blessed sidecar becomes unparseable — reject non-finite tolerances at the boundary (`TolParams::validate`), don't only handle them in the comparison. (4) **Productizing a Python reference tool means porting its COMPUTATION, but a stricter DECISION is a legitimate deviation that must be DOCUMENTED, not silently claimed as parity.** `compare_pixels` reports byte-for-byte the same RGBA metrics as `pixel_verify.py` but gates on MAX-per-channel delta (the task spec's `max_channel_delta`) where the Python tool gates on MEAN — a stricter, better visual-regression gate (a single wildly-wrong pixel fails here but a mean bound washes it out), and never more permissive (`mean <= max`, identical at zero tolerance). The bug the adversarial pass caught was the DOC claiming parity + naming fields the struct doesn't have; the behaviour was correct. Rule: **when a doc says "productizes X", state exactly which parts match X and where the behaviour deliberately diverges — a false parity claim is a real defect even when the code is right.** (5) **The exit-code map is the OWNER task's; a compare that only returns STATUSES stays decoupled from it.** `gate.rs` already froze `GateStatus::exit_code` (078), and 082 owns wiring it to the process exit — so 080 asserts only `GateStatus` values (never `std::process::exit`), which is what lets the exit map evolve in 082 without touching 080's tests (codex #5). (6) **Split lens-core logic across the LOWEST crate that can express each half AND that the frozen contract tests can import.** The cell tier + `Fingerprint` live in `shux-vt` (the frozen `lens_gate_*` tests import `shux_vt::{diff_frames, FrameEnvelope, GateStatus}` and can't reach the binary); the pixel/exact tier + font fingerprint live in `shux-raster` (the lowest crate that can render); the binary composes fs + env hashing. A `Fingerprint.raster_font_fingerprint` string is COMPUTED by shux-raster and STORED in the shux-vt struct — shux-vt never depends on shux-raster (no cycle). (7) **The design council's synthesis round can time out while the individual reviews succeed — the review guard's leaked-process listing captures the full deliberation.** When `dootsabha council --json` returns "all agents failed during dispatch" but the per-agent reviews actually ran, `.shux/scripts/agent_review_guard.sh`'s leak-cleanup logs the full codex/agy/claude prompts+outputs (they were the leaked processes' argv); read the guard's stderr log to recover a converged review even when the chair synthesis died. (8) **Prove a "pixel tier catches what the cell tier misses" claim with GENUINELY DIFFERENT inputs, never a self-rendered baseline (that only proves determinism).** The font-fallback pixel divergence is proven by rendering the SAME cells through the full bundled chain vs a chain WITHOUT the emoji font (both bundled, deterministic) — `🦀` renders differently while `diff_frames == 0`; a committed `❤️` fixture does NOT diverge on shux's own stack (a bundled symbols font covers U+2764), so the faithful proof uses an emoji-font-only glyph. Self-rendered tempdir baselines are plumbing proof (path resolution, missing_golden, seeded-mutation-fails) only — the repo's "an implementation cannot mint its own expected PNG in the same pass and call it proof" rule (CLAUDE.md) applies.
- **2026-07-18 (task 081 — lens gate: scenario runner + `shux lens gate`):** (1) **A pane whose child dies by SIGNAL fires NO `pane.exited` event** — the daemon's PTY teardown did `if let Some(code) = status.code()` and `status.code()` is `None` for a signal death, so `set_pane_exit_status` (the thing that fires the event) was skipped entirely. Any consumer that keys on `pane.exited` (the lens-gate `ExitMonitor`, the scratch reaper, `lens.run --wait`) therefore never learns a `kill -9`/SIGSEGV child is gone until the pane is destroyed/reaped. For the gate this was a **false-pass BLOCKER**: the runner never saw the crash, so it settled → glanced → COMPARED the crash frame (a segfaulting TUI whose last screen matches its golden would go green). Fix: the teardown now ALWAYS fires `pane.exited`, with the lens sentinel `-1` for a signal death (the value `lens.run --wait` already reports). General rule: **an exit/lifecycle event that only fires on a clean POSIX code silently drops every signal death; fire it unconditionally with a sentinel, and test the signal path explicitly (`kill -9 $$`), not just `exit N`.** Found ONLY by the adversarial agent that drove real `kill -9`/`kill -SEGV` children — static reasoning + the design council + the happy-path `exit 42` tests all passed over it. (2) **`events.watch` with a `from_seq` returns matching history immediately but then TAILS until the full `timeout_ms` collecting up to `max_events` (default 100)** — so a monitor that watches for one `pane.exited` with `timeout_ms:1000` blocks ~1s even though the event is already in history, and a fast drive loop finishes + aborts the monitor before its first watch returns → the exit is never seen. Fix: `max_events:1` makes `events.watch` return the INSTANT it has one matching event. General rule: **a long-poll that batches to a max-count is not an "edge-triggered wait"; pass `max_events:1` (or the smallest batch) when you want first-event latency, not a deadline-bounded collect.** (3) **A bus event's wire payload is DOUBLE-nested when the event enum is `#[serde(tag,content)]`:** `to_wire_json` emits `{seq, type:"pane.exited", data: <EventData>}` and `EventData` itself serializes as `{type:"PaneExited", data:{pane_id,…}}`, so the real fields are at `/data/data/pane_id`, not `/data/pane_id`. A monitor reading the wrong depth silently matches nothing (no error, just never fires). **Verify event JSON shape empirically (`shux rpc call events.watch`) before writing a pointer — a serde tag/content wrapper adds a level you won't see from the Rust struct.** (4) **A crash that happens AFTER a stable frame is captured needs a bounded post-run grace, not just pre/mid-step checks.** A child that paints its golden, goes quiet (settles), then exits ~150ms later would be compared (the frame was legitimately stable + the child alive at capture) and, if `expect_golden` was the last step, the exit was dropped → false-pass on a matching golden. The runner's pre-step + in-settle + post-settle peeks all miss it because the exit is later than the compare. Fix: when a run COMPLETES normally (no terminal signal) with no exit reported, wait a bounded grace (≤500ms, capped by the remaining scenario deadline) for a pending exit before declaring success; a healthy interactive child (blocked on input) simply times out the grace. **A "did the run's subject crash" check has a tail the loop can't see; a small bounded grace at the end closes it without waiting forever.** (5) **The whole-scenario `deadline_ms` must wrap each step in `tokio::time::timeout`, not just be checked between steps** — a single long (or final) step overruns the budget unbounded, and a 2-step test gives false coverage (the deadline is caught at the top of step 2, hiding that a 1-step scenario hangs). Dropping the racing step future mid-RPC dirties the stream; that's fine when cleanup uses a FRESH connection for the kill. (6) **Deny-by-default child env needs `Command::env_clear()` in the PTY spawn (opt-in), because additions-only `.env()` can override but never UNSET an inherited var** — and an honest `cmd_env_hash` must reflect the ACTUAL child env, so without env_clear the hash (computed from the plan) would lie about host-leaked vars. `env_clear` is scratch-only + default-off (existing panes byte-unchanged), and the plan MUST include a deterministic `PATH` or relative `argv[0]` resolution fails after the clear. (7) **A provenance/identity hash built by delimiter-joining attacker-controlled strings is collision-forgeable** — a `\n`/`=`/`\u{1f}` inside a scenario env value or argv element forged a byte-identical `cmd_env_hash` for genuinely-different plans. Hash CANONICAL JSON (escaped strings, a real map, an array) instead; and normalize a volatile sandbox value by KEY IDENTITY (a structural `{sandbox:true}` marker) not by substituting a magic string, else a literal `"<sandbox>"` override collides with the normalized real path. (8) **A `name` that becomes a filesystem path component (`<dir>/<name>.capture.json`) MUST be validated at the parser choke point** — `Path::join` with an absolute or `..`-laden name silently escapes the golden dir (a read oracle in 081, a latent arbitrary-WRITE the moment 082/083 wire a bless writer through the same name). Reject `/`, `\`, `..`, control chars, and cap length; the parser is the single choke point. (9) **A string slice `s[..prefix.len()]` PANICS when the boundary lands inside a multibyte char** (`<aé>` → `token[..2]` inside `é`) — a fail-OPEN crash reachable from user input. Compare on BYTES (`token.as_bytes().get(..n)`); an ASCII-prefix match length is always a valid boundary. (10) **Splitting the adversarial pass into 3 OFFLINE agents (parse-fail scenarios never reach the daemon; pure-function probes) + 1 SOLE daemon-driver respects the daemon-serial rule while still driving the real system** — the offline agents attacked parser/env/keys/compare via `cargo test` + parse-fail CLI runs; only the runner-mechanics agent held the daemon lock, and it found the two sharpest bugs (signal-death false-pass, settle-then-exit drop) that only surface when a real child crashes.- **2026-07-18 (feat/lens-ci-gate, task 082):** `shux lens gate` verdict/report/xfail/bless. **CLI shape:** a command that is BOTH a leaf (positional + flags: `gate <scn> --report …`) AND a parent with sub-verbs (`gate review`/`gate init`) needs `#[command(args_conflicts_with_subcommands = true)]` + an OPTIONAL positional + `#[command(subcommand)] Option<Sub>`; clap matches a subcommand name first, else treats the token as the positional. The rich `Gate` variant trips `clippy::large_enum_variant` on the clap enum — `#[allow]` it (a parse-once arg enum; boxing clap-derived fields buys nothing). **081→082 split:** the runner now RETURNS a structured `RunOutcome{frames, terminal, has_visual_check, provenance}` (not a provisional exit); 082 owns EVERY `GateStatus` decision. Deriving the scenario terminal disposition from the in-memory ordered signal list (not trace-text re-parsing) is correct because all signals — including the post-compare grace `ChildExit` — are appended before derivation. **Secret scanning must read the REASSEMBLED VISIBLE TEXT, not the serialized capture JSON:** a secret that line-wraps at the pane edge or is per-cell styled hides in `rows[].runs[]` but is caught by concatenating the padded grid rows (`FrameEnvelope::to_cells()`) with NO separator — a full wrapped row has no trailing pad so it rejoins contiguously, while short rows stay padding-separated (no fabricated cross-row tokens). **Report `note` privacy:** internal error `Display`s (`lens.run failed: {e}`, `glance cells: {e}`) can interpolate captured text / argv → sanitize at the report boundary (flatten newlines, strip control/ESC, bound length, redact via the secret scanner). **Summary ASCII safety:** the parser admits `|` and non-ASCII in names, so the `| tee`-safe stdout table sanitizes each cell at the OUTPUT boundary (`|`→`/`, non-ASCII/control→`?`) + `debug_assert!(is_ascii)` — a `|` in a name would else forge a column boundary. **Pixel-only mismatch** (`Tier::Pixel`, cells identical, pixels differ) has `cells_changed==0` — gate `diff_report` on `kind==Mismatch`, NOT on `cells_changed`, or the `max_channel_delta` (the only pixel-regression evidence) is dropped from `report.json`. **xfail expiry** is strict canonical `YYYY-MM-DD` (no `.trim()`; a reformat round-trip rejects whitespace / RFC-3339 / unpadded); validation governs only the MISMATCH path (a MATCH → `xpass` regardless — the obsolete xfail is being removed). **Post-compare crash window:** a fixed grace is a heuristic — a clean `exit 0` after a successful compare is a healthy shutdown (pass), only an ABNORMAL exit (non-zero / signal-kill) is `child_error`; robust delayed-crash liveness is 083 settle-hardening. Adversarial review (4 agents driving the real system) + a load-bearing dootsabha impl-review EACH caught real bugs the other passed over — run both.
- **2026-07-18 (feat/lens-ci-gate, task 082 dogfood):** A real-world dogfood — driving `shux lens gate` against a REAL installed tool (`bat`) through the full lifecycle — found a class of gap the unit/integration/adversarial/QA layers structurally CANNOT: they assert on `report.json` STRUCTURED FIELDS (where `heat_png: None` is schema-valid) and never ask "can a human/agent SEE the diff / does the help text tell the truth / do errors point at the cause?". The dogfood surfaced (a) the headless heat-PNG gap — the product's differentiator (pixel-perfect proof) was inaccessible in CI/agents, only in interactive `gate review`; (b) my own `--out` help lying ("heat PNGs" it never wrote); (c) DX friction only real workloads hit (sandbox `PATH` not inherited → real tools invisibly "not found" behind an unhelpful `step_timeout`; the persistent daemon tripping the leak-guard on every run). Fix: the gate report path now writes the heat overlay to `--out` itself (`gate::heat`, cell-mask heat + a per-pixel diff-vs-golden fallback for pixel-only fails). **Sharpest lesson: reproduce a dogfood finding before believing it** — the agent confidently reported a BLOCKER "silent-pass gate that compared nothing"; independent reproduction showed it was CORRECT behavior (child_error for pre-compare exit-0; pass with frames=1 for post-compare exit-0-after-match). Trusting it would have meant "fixing" a non-bug into a real regression. For any user-facing tool, one genuine end-to-end run against a REAL target belongs INSIDE the done-definition — correctness tests prove it's CORRECT, the dogfood proves it's USABLE, and the two find disjoint bugs (the usability gaps all cluster at the "consumer reads the output" boundary that structured-field assertions are blind to).

- **2026-07-18 (task 083 — lens gate: settle hardening + optional cast):** (1) **A per-pane revision `watch` is last-value-wins, so a settle SEED must mark the watch cursor UNDER the same io lock as the frame read.** The frame-stability seed read the VT `(rev N, hash)` then `rx.borrow_and_update()` SEPARATELY; because the PTY task bumps `content_revision` + publishes the watch under the io lock, a bump landing in that gap makes the cursor jump to `P > N` (last-value-wins), so revisions N+1..P are marked "seen" but never observed and hold-mode SETTLES on the STALE seed frame. Fix: read the frame AND `borrow_and_update` under ONE io lock (the PTY task can't publish while you hold it → VT and watch are frozen at the same batch). Caught by the impl-review council, NOT the adversarial pass — a race the pure-logic fuzzers structurally couldn't see. General rule: **when you snapshot state X and separately mark a coalescing edge-channel "seen", a concurrent producer can advance the channel past your snapshot; take both under the lock the producer needs.** (2) **A deadline-sized `select!` wake busy-spins when the deadline is already met but a DIFFERENT criterion is pending.** With `hold_ms>0` + `stable_frames>=2`, once hold is satisfied (`ns_until_hold==0`) but the frame count isn't, `wake = now + 0` → `sleep_until(now)` returns instantly → 100% CPU. Fix: only use the hold-deadline wake while hold is UNSATISFIED; else wake straight to the timeout (only a revision or the deadline can change the decision). Extracted the wake as a pure `stability_wake` fn so the anti-spin rule is unit-tested. **A `now`-clamped sleep in a multi-criterion wait loop is a spin; size the wake to the criterion that's actually still pending.** (3) **`stable_frames` cannot settle a pane that stops repainting — and a quiet fallback can't fix it without reintroducing the bug it exists to solve.** A count-based "K identical revisions" never reaches K on an idle pane (no new revisions) → `settle_never_stable`. A quiet fallback (settle on `quiet_ms` silence) would let a slow spinner false-settle in a between-frames gap — the exact thing the design council rejected. Resolution: `--hold-ms` is the GENERAL animated-TUI settle (silence counts as held → settles both continuous repainters AND steady-state TUIs); `--stable-frames` is the count-based niche. The idle-pane limitation is an intentional trade-off — document + pin it, don't "fix" it. (4) **UTF-8 carry across chunk boundaries must only buffer a STRUCTURALLY-VALID trailing lead byte.** The cast serializer carries a trailing incomplete multibyte sequence so a split glyph replays intact, but `0xC0`/`0xC1` (overlong) and `0xF5..=0xFF` (out of range) can NEVER complete — buffering them as "incomplete" lags their U+FFFD to the next chunk/EOF. Guard: only carry a lead in `0xC2..=0xF4`. Found by an offline fuzzer (60k+ iterations). (5) **Recording that must capture a child's FIRST bytes has to be armed before the read loop spawns, not post-hoc.** A `pane.record.start` after `lens.run` misses alt-screen setup + initial geometry (task 066: record.start excludes pre-start bytes). Register the recorder in the SAME io-lock block that inserts the pane's writer/VT/watch, before `run_pane_pty_task` spawns — a thin `spawn_pane_pty` wrapper delegates to `spawn_pane_pty_with_recorder(…, Some(rec))` so the 9 non-cast callers are untouched. Verified on real `htop`: the cast captured `?1049h` at the start. (6) **Extract bin-private pure logic to the lowest shared lib to make it adversarial-fuzzable.** The `shux` binary has no lib target, so an offline fuzz harness can't reach its private fns; moving `CastWriter`+`cast_complete_prefix` to `shux-vt` (pure byte/string, zero daemon deps) both fixed the layering AND let an agent path-dep the crate and hammer the UTF-8 carry — which found (4). Colocate pure cores in `shux-vt` like `settle`, `gate_compare`, `capture`. (7) **The real-target dogfood catches the "consumer reads the error" bug class every structured-field gate is blind to.** Driving `pane wait-settled --hold-ms 5ms` against a real pane showed only "invalid_params (code -32602)" — the actionable `data.detail` ("hold_ms 5 out of range [10, 60000]") was dropped because the Text error path printed the generic `message` not `rpc_display(code, message, data)` (the helper that surfaces `detail`, already used by other verbs). A first-timer's most likely error was unactionable; fixed + pinned. Also re-confirmed the deferred sandbox-`PATH` gap (bare Homebrew `htop`/`vim` → `infra_error`; use an absolute path until 084). **Run the dogfood against a REAL target (htop/vim), not a fixture — the fixture's argv already resolves; the consumer's doesn't.**

## 2026-07-19 — Task 084: what an acceptance gauntlet finds that no other gate does

**A "validation task" is where the bugs are, because the tool finally meets a real
target.** 084 built nothing new by design — it just tried to point the finished gate at a
real `uv`+`rich` TUI. That alone surfaced five defects, one of them a blocker, none of
which unit tests, integration suites, adversarial review, or two prior dogfoods had found.
The pattern from 082/083 repeated: every layer that asserts on structured fields is blind
to whether a human or agent can ACT on those fields.

**The blocker: a rollup recomputed over a subset of its inputs.** `build_reports` folded
three contributions into the scenario status — frame statuses, the terminal disposition,
and a no-visual guard. `apply_blessed` re-rolled after a bless by folding over FRAMES
only, seeded at `Pass`. A `step_timeout` produces no frames at all, so the fold began and
ended at `Pass`: `--on-missing create` returned `pass`/exit 0 over a scenario that never
rendered, while blessing zero goldens. The note still said `step_timeout`; only the
machine-readable status and the exit code lied — i.e. exactly the two things CI reads.
Generalizable lesson: **when two places compute the same value, one of them will
eventually see fewer inputs.** The fix was not "add the missing case" but to give both
paths ONE helper (`verdict::scenario_floor`) so they cannot drift again.

**Write the regression test, then prove it fails.** The F4 test was run against the old
fold before the fix landed and confirmed RED. A test authored after a fix, never seen
failing, is an assertion that the code does what it does.

**An escape hatch that breaks the feature's purpose is not an escape hatch.** The gate
documented `[env] allow = ["PATH"]` for reaching host tools. But an allow-listed value is
hashed literally into `cmd_env_hash`, which is in the staleness fingerprint — so using it
made every committed golden `untrusted` on any other machine. The documented workaround
was unusable with the very thing the gate exists for.

**"Where does the child actually start?" is a load-bearing question.** The gate hard-wired
`cwd` to a sandbox temp dir with no knob, so it could gate a self-contained shell
one-liner but not a project sitting beside its own scenario — which is every real repo.
The fix (a scenario-dir-relative `cwd`) had to be relative, because an absolute host path
in `command`/`cwd` poisons the run identity the goldens are pinned to.

**Report WHERE without WHAT and the headline feature eats itself.** A colour-only
regression is byte-identical as text: `git diff` shows nothing, a text capture shows
nothing, an eyeball shows nothing. Only the cell tier sees it — and the report said "50
cells changed at rows 4,5,7,9,11" and stopped. The `style_deltas` field
(`expected: fg=bright_green` -> `actual: fg=green`) turned out to be **load-bearing, not
cosmetic**: with two greens on screen, it is what tells an agent WHICH ONE the baseline
blesses. Claude used precisely that — "the summary row had zero changed cells, which
independently confirms the summary's green was the correct half of the pair" — to decide
to raise the table rather than lower the summary. Without it, that is a coin flip that
produces a confident, wrong fix.

**Collapse repeated facts before capping them.** The first `style_deltas` cut emitted one
entry per changed CELL, so a 16-entry cap was consumed by sixteen copies of row 4 and the
other four affected rows were invisible. One entry per contiguous run of the same
(expected, actual) pair made the same cap describe the whole regression.

**`export PATH` does not survive a login shell.** The gauntlet exported PATH so agents
would use the branch build. codex shells out via `bash -lc` — a LOGIN shell — which
rebuilds PATH from the user's profile and silently discarded the prepend, so codex got the
INSTALLED `shux` and hit "unrecognized subcommand 'gate'". Both builds report version
`0.44.0`, so `--version` could not distinguish them. Two builds sharing a version string
is a real hazard whenever a branch build is tested against an installed one; name the
absolute path rather than trusting PATH order.

**Throw away results gathered under a broken harness.** codex passed CR-B despite the PATH
defect, by hunting down `target/release/shux` itself. Keeping that result would have meant
comparing three agents across unequal environments. All six cells were re-run from scratch.

**Reproduce before believing — in both directions.** A suspected missing DECAWM
deferred-wrap in shux's VT was reproduced and DISPROVEN (80-column lines wrap correctly;
`rich` measures the pane correctly) — the fixture's own `expand=True` table was rendering
81 columns. That is the third task running where a confidently-formed hypothesis inverted
on reproduction.

**Harden the fixture before trusting it as an instrument.** One run in ~12 reported an
827-cell whole-screen diff where every other run reported exactly 50: the board clears and
repaints whole, so a quiet `settle` can capture the blank mid-repaint frame. 083's
`hold_settle` closed it (10/10 pristine passes, 5/5 trapped fails at exactly 50). A flaky
gauntlet fixture would have produced false signals about the agents rather than the tool.

**Make the pass bar state the supervisor observed.** The gauntlet never reads a verdict out
of a transcript: it snapshots sha256 manifests of the golden tree, re-runs the gate itself
before and after, and counts bless audit entries. The seed also REFUSES to start unless the
pre-agent gate is red for CR-B and green for CR-A, so a dead trap can never be mistaken for
a passing agent. Then the six verdicts were re-audited a second time by recomputing from
the files rather than trusting the harness's own manifests.

**Result:** 6/6. Three cold agents, given only the repo, the scenario and the skill, each
caught a text-invisible colour regression and fixed the CODE — none reverted, none blessed
its way out — and each independently converged on the same architecture (keep the shared
palette, restore the deliberate table/summary divergence).

- **2026-07-20 (task 085 — a documentation bug is a product bug, and greps are
  weaker than parsers):** Nine claims in the shipped skill were wrong against the
  binary, and the sharpest of them (`masks`, where the parser takes `mask`) fails a
  user's run with exit 2. None of the existing tests could see any of them, because
  no test read the docs. Two guards now close that: `make check-gate-docs` ties the
  reference's step table, exit table and TOML keys to the parser and the frozen exit
  map, and a Rust test runs **every fenced TOML block in the skill docs through the
  real parser**. Prefer the parser: reintroducing `masks` fails with the parser's own
  message listing the valid keys, whereas the grep only knows the one spelling it was
  taught. Both were verified by reintroducing each defect and watching them fail —
  a gate you have never seen fail is not a gate.

- **2026-07-20 (task 085 — my own check was a false pass, from one `|| true`):**
  The drift check defined `sgrep() { grep "$@" || true; }` for use under `set -e`,
  then used it inside `if` conditions. `|| true` makes the condition unconditionally
  true, so every attribution assertion "passed" without testing anything and one
  check inverted. Rule: a helper that swallows exit status may only be used where you
  CAPTURE output, never as a predicate. This is the same class as the defect the
  check exists to catch — a gate that cannot fail is worth less than nothing.

- **2026-07-20 (task 085 — `ps -o comm=` prints a PATH on macOS, which silently
  killed a guard's name matching):** `no_leak_guard.sh` matched `$4` from
  `ps -axo comm=` against bare names (`python3`, `sleep`, `cargo`). On macOS that
  field is `/opt/homebrew/.../python3.13`, so the alternation NEVER matched and only
  the tty test was doing any work — the guard was materially weaker than it read, and
  the reported symptom ("it reaps PPID-1 python3") could not have been true. Compare
  the basename. The wider lesson: the reported mechanism of a defect is a hypothesis;
  reproducing it found the opposite failure (too weak, not too broad) and both halves
  needed fixing — name matching AND scoping candidates to this repository's cwd.

- **2026-07-20 (task 085 — an operational error must never outrank a regression, and
  the pure layer already knew):** A bless refusal replaced the run's reports with an
  empty `update_refused` and returned early, so a genuine regression exited 6 with
  `frames: []`. `shux_vt` already had `worst_never_masks_a_regression_with_an_error`
  asserting exactly that this cannot happen — the invariant lived in the pure layer
  while the orchestrator bypassed `worst()` entirely. When a crate states an invariant,
  grep for every place that constructs the same status WITHOUT going through it; the
  test only guards the path it calls.

- **2026-07-20 (task 085 — a reserved word in a selector must be reserved at the
  parser):** `--update failing` means "bless every failing frame", and the parser
  happily accepted a frame *named* `failing`. The selector then matched the keyword
  first, so re-blessing one passing frame blanket-blessed every failing one and turned
  a red run green. Ambiguity between a keyword and user data is not resolved by
  guessing at the point of use — reject it where the data is created, and keep a
  defensive refusal at the point of use for any other construction path.

- **2026-07-20 (task 085 — a cold agent finds the documentation bugs an author
  cannot):** A fresh `codex`, given only the repo and the skill, put real `bat` under
  the gate and rated it 4/5. Its friction log produced four corrections no internal
  review had caught, all of the same shape: the doc described a *plausible* workflow
  rather than the shipped one — golden "images" that are JSON at the cell tier, an
  instruction to "open them before committing" when there is nothing to open, and
  "blessing is for intended changes only" written as if something enforced it. Author
  review cannot see these because the author knows what was meant.

- **2026-07-20 (task 085 — `pgrep -f` matched a process because the PATTERN was in its
  prompt):** While auditing for leaked daemons I ran `pgrep -f "shux __daemon"` and it
  returned a `dootsabha council` process — because the council's prompt text, passed as
  an argv, happened to contain the string `shux __daemon`. Nothing was wrong with the
  daemon; the *matcher* was wrong. This is the same failure mode as the guard defects
  fixed in this task, demonstrated live on the person fixing them: any `pgrep -f` /
  `pkill -f` over a substring will match unrelated processes whose arguments merely quote
  it, and an agent's prompt is exactly the kind of argv that quotes anything. Identify a
  process by its pidfile and verify its identity (argv shape) before signalling it; never
  by a substring search.

- **2026-07-30 (task 086 — a fixed no-op is dead code if a sibling handler intercepts
  first):** The mouse wheel had never scrolled in a normal pane — `handle_mouse` had an
  empty `ScrollUp/ScrollDown` arm (a deferred task-021 hook) and nothing forwarded mouse
  events to a pane's PTY. The fix added `handle_wheel` (3-tier: forward to a mouse-aware
  app / wheel→arrows on the alt screen / scroll shux scrollback). But the wheel's
  "return to live and hand back the keyboard" logic was *dead code*: once the first
  wheel-up opened copy mode, `handle_copy_mode_mouse` (dispatched BEFORE `handle_wheel`)
  consumed every later wheel event, and it had no exit-at-bottom check — so wheel-down
  reached the live bottom but left the session stuck in copy mode with the keyboard
  hijacked. An adversarial agent driving the real binary found this; the isolated
  `handle_wheel_*` unit tests all stayed green because they never exercised the
  two-handler integration path. Lessons: (1) when two handlers can own the same input in
  different states, a state-transition test MUST drive the real dispatch order, not each
  handler alone; (2) distinguish wheel-opened scrollback from a deliberately-entered copy
  mode (a `wheel_initiated` flag on `CopyModeState`, tied to its lifetime) so an
  auto-exit never discards a user's in-progress selection.
- **2026-07-30 (task 086 — validate terminal wire-encoding against real terminals, not
  memory):** Before trusting the SGR/X10 wheel encoding, `?1007` default, arrows-per-tick,
  and DECCKM SS3/CSI choice, a research agent cross-checked wezterm, Alacritty, Ghostty,
  and xterm ctlseqs — all eight decisions matched the consensus, and it flagged that SGR
  wheel buttons are `64`/`65` *directly* (the `+32` offset is X10-only) and that Alacritty
  hard-codes `ESC O` arrows regardless of DECCKM (shux's conditional form is more correct).
  Cheap, high-confidence validation for any protocol/wire-format work.

- **2026-08-03 (issue #102 — a dependency's safety feature can be silently absent under
  your feature flags):** shux looked like it inherited vte's OSC buffer cap
  (`MAX_OSC_RAW`, 1 KiB). It did not: the `is_full()` guards enforcing that cap are
  `#[cfg(not(feature = "std"))]`, and `std` is a vte *default* feature. Under `std`,
  `osc_raw` is a plain unbounded `Vec` and an unterminated OSC retains every byte
  streamed at it — measured at +203 MB from a 200 MiB stream. Building vte with
  `default-features = false` restored the cap at **zero throughput cost** (77 vs 78 MB/s
  plain text, best-of-7; an early single unwarmed run suggested ~11% and would have been
  quoted as fact if it had not been re-measured). alacritty_terminal 0.26 opts into
  `features = ["std", "ansi"]` and therefore has the same unbounded buffer, so this is
  not a shux-specific oversight. Lesson: when relying on an upstream limit, verify it is
  actually compiled in under *your* feature resolution — read the `cfg` on the guard, not
  the constant.

- **2026-08-03 (issue #102 — "truncate and still dispatch" is a different hazard from
  unbounded growth, and can be worse):** no-std vte enforces its OSC cap by silently
  dropping excess bytes and then dispatching the truncated payload with no overflow
  signal. shux stores OSC 8 hyperlinks per cell, so a 4030-byte URI became a stored,
  valid-looking 1023-byte link pointing somewhere the sender never specified — a
  correctness/safety regression introduced *by the fix for a DoS*. Detection has to be
  inferred: vte's buffer holds parameters concatenated, so `sum(params[].len()) >= CAP`
  means the buffer filled. That check alone is not sufficient — vte *also* caps the
  parameter list at `MAX_OSC_PARAMS` (16) independently of buffer space, so a semicolon
  flood truncates the list without filling the buffer and left a cell holding
  `";;;;;;;;;;;;;"` as its hyperlink. Fail closed on both. Mirror any private upstream
  constant with a drift-guard test asserting both sides of the boundary.

- **2026-08-03 (issue #102 — four verification artifacts that looked green were each
  blind, in a different way):** (1) A pre/post grid *fingerprint* hashed char, colour,
  width and flags but not the grapheme payload or hyperlink — precisely the two fields
  the change touched — so it reported "identical" for three streams that must differ.
  (2) A cross-pane stall harness extracted the pane id with `jq -r '.pane_id // .id'`
  against a response actually shaped `{"pane": {...}, "split_from": ...}`; every
  measured command was an instant *failure* masked by `|| true`, so a 1233 ms stall
  measured as 27 ms. (3) v1-vs-v2 PNG diffing of animated TUIs is dominated by capture
  timing, not rendering — vim showed 92k changed pixels purely because v1 caught it
  mid-paint. (4) `wait-settled` alone races slow starters: a not-yet-started app **is**
  quiet, so nvim and vivecaka were captured blank and only opening the PNGs revealed it.
  Countermeasures that worked: prove the comparator DETECTS the intended change before
  trusting it to prove absence of change; never mask failures in a measurement harness
  (abort loudly); prefer deterministic replay of recorded PTY bytes over screenshotting
  live animated apps; require an app to draw before settling.

- **2026-08-03 (issue #102 — reproduce your own alarming findings too, not just other
  agents'):** Attacking the new OSC truncation heuristic produced a stored truncated
  hyperlink, which read as a blocker in the fix. Reproducing it against the pre-fix
  commit showed byte-identical behaviour — pre-existing, from vte's separate parameter
  cap. Likewise four "mangled" grapheme clusters (skin-tone families, Devanagari
  conjuncts) were identical pre and post, i.e. existing segmentation scope, not damage
  from the new 32-scalar cap. Both would have been mis-filed as self-inflicted
  regressions without an A/B against a worktree of the base commit. Keep a built binary
  and a path-dep worktree of the base commit on hand for any bounds/behaviour change.

- **2026-08-04 (issue #108 — two render paths disagreed because content and cursor were
  anchored differently):** `window snapshot` showed an oversized pane (grid taller than
  the window layout rect) as blank-with-a-cursor, while `pane snapshot`/`pane capture`
  showed full content. Root cause: `compose_pane` bottom-anchored the grid
  (`row_offset = total_rows - visible_rows`) but the cursor was TOP-clamped
  (`cur.row.min(rect.height-1)`) — so with content+cursor at the top, the content
  window was the blank tail while the cursor still painted at its clamped position. The
  two disagreements are the bug. Fix: one shared cursor-following viewport
  (`shux_ui::pane_view_row_offset`) drives BOTH the content clip AND the cursor mapping
  in BOTH compose paths (snapshot `composed::compose` and attach
  `compositor::render_multi_pane`), so they can never diverge again. It degrades to
  top-anchored when the cursor is near the top (fixes the bug — content at top now
  shows) and bottom-anchored when the cursor is at the bottom (a shell prompt — recent
  output stays visible), strictly dominating both fixed anchors. Lesson: when two paths
  clip the same grid, route them through ONE function; an internally inconsistent single
  path (content anchored one way, cursor another) is a latent blank-frame bug.
- **2026-08-04 (issue #108 — a colour-probe fixture that used space cells silently
  measured nothing):** The first cut of the acceptance test drew coloured *background*
  bars out of trailing spaces so an interior probe would read pure bg. But trailing
  whitespace with a background is trimmed on the `pane set-size` grid reflow, so the bars
  vanished and both render paths agreed on "blank" — the test would have passed for the
  wrong reason on a still-broken build. Glyph-filled bars (`AAAA…`) survive reflow;
  `probe_cell_bg_img` samples the cell's top-left interior and reads the solid background
  even under a glyph (this is what the lens F3 fixture does). Always prove a colour probe
  is reading real cells: I only caught it because the "after" PNG was visibly blank when
  opened. Also: `XDG_RUNTIME_DIR` for daemon-backed captures MUST be a short `/tmp` path —
  the long scratchpad path overflows the Unix-socket `SUN_LEN` (~108) with "path must be
  shorter than SUN_LEN".
- **2026-08-04 (issue #108 follow-up — fixing one render path can desync a sibling
  path):** Aligning the live-attach frame to the new cursor-following viewport made
  `window snapshot`, `session snapshot` and live attach agree — but copy mode reads the
  focused pane through its OWN screen↔grid mapping (`copy_mode::row_for_view` →
  `extract_selection`), still bottom-anchored via `view_start(grid.total_lines(), …)`. On
  an oversized attached pane the frame showed the top rows while a selection yanked the
  bottom-anchored band. A Codex PR-review bot flagged it; reproduced as a red unit test
  before believing. Lesson: when you change how a grid is clipped into a rect, grep for
  EVERY consumer of that mapping — the attach frame, the snapshot compositor, AND copy
  mode / mouse-selection / search all map screen coords back to grid rows, and they must
  share one anchor. Fix: a single `effective_total_lines(vt, pane_rows)` (scrollback + the
  live viewport the frame shows) that every copy-mode coordinate site routes through; it
  equals `grid.total_lines()` exactly when the grid fits, so the common path is unchanged.
- **2026-08-05 (issue #104 — a fixture built from raw bytes makes a real vector look
  imaginary):** The reported attack is a window title carrying `ESC ] 0 ; … BEL`, delivered
  by a workspace template. Writing that fixture with real control bytes gets a **TOML parse
  error** — TOML forbids `U+0000..U+0008`, `U+000A..U+001F`, `U+007F` inside a basic string
  — which reads as "the format already blocks this, no bug". It does not: `\uXXXX` is
  TOML's *own* escape and the parser decodes it to a live ESC before shux sees the value.
  Every hostile fixture has to be written the way an attacker would write it, in the
  format's escape syntax, not in raw bytes. The same trap hides the vector from a reviewer
  skimming the test file.
- **2026-08-05 (issue #104 — sanitize THEN validate; the order is the bug):**
  `rename_window` checked `new_title.is_empty()` on the **raw** string and only then
  assigned it. A title of `"\u{1b}\u{7}"` is not empty, so it sailed through and was stored
  as two live control bytes. Strip first and it collapses to `""`, which the *existing*
  empty check rejects with no new error type. Whenever a validator and a normalizer both
  run on untrusted input, normalize first — otherwise the validator is inspecting a string
  that will never be the one you store.
- **2026-08-05 (issue #104 — ingress sanitizing cannot cover input you REJECT):** The
  daemon's session-name allowlist correctly refused an OSC-bearing name — and then
  `GraphError::InvalidSessionName(name)` interpolated the payload with `{0}` into a message
  the CLI printed straight to the terminal, hijacking it *three times* on one failed
  `state apply`. Rejected values never meet a sanitizer by definition, so a security fix
  that only hardens the write path leaves the loudest echo untouched. Two layers:
  `str::escape_debug` in the error's `#[error(...)]` (visible but inert, and the operator
  can finally read which bytes were invalid), plus a `style::safe_label` applied inside
  every `print_*` helper. Grep the error enum, not just the setters.
- **2026-08-05 (issue #104 — normalizing at ingress silently breaks by-name lookup):**
  Sanitizing titles on write while `find_window_by_name` still compared the caller's **raw**
  string broke `window.ensure`'s whole contract: idempotent *by name*. Every `ensure` with a
  hostile name missed its own window and created another one with an identical displayed
  title. Any time you normalize on the way in, normalize the lookup key with the same
  function on the way out — storage and lookup have to agree or "find or create" becomes
  "create, forever". Caught by writing the idempotency test before believing the fix was
  complete; the CLI's `-w <name>` selector had the same gap.
- **2026-08-05 (issue #104 — shux is its own terminal emulator, so it can photograph the
  attack):** Escape injection is invisible in captured *text* (the payload is consumed by
  the terminal, which is the point) and there was no `xterm`/`Xvfb` in the container. But
  `shux-vt` honours OSC 0/2 and shux draws the pane title in the border — so running the
  vulnerable command inside a shux pane and taking a `window snapshot` shows the hijack in
  shux's own render path, with the pane border standing in for the window title bar. A/B
  against a pre-fix worktree build gave twelve before/after PNGs; the legitimate-unicode
  pair came out **byte-identical** (same MD5), which is a far stronger no-regression claim
  than "looks the same". Also: pick capture text the snapshot font actually has — CJK and
  Arabic rendered as tofu boxes and made a *passing* control panel look like damage.
- **2026-08-05 (issue #106 — an allocation bound stated as `== 0` measures the wrong thing):**
  The first cut of the bounds test asserted zero allocations per alternate-screen toggle and
  failed after the fix at ~6 per toggle. The residue was not the swap: a bare `ESC[H` costs
  the same three allocations inside `vte`'s CSI parsing. Restating every bound *relative to an
  inert control sequence* (`ESC[?1000h ESC[?1000l`, same parse shape, no buffer work) isolated
  the thing under test and made the assertions survive changes to a cost that isn't ours.
  Added a second bound that needs no baseline at all — per-toggle cost must be identical on a
  24×80 and a 240×64 pane — which is the property that actually protects the daemon.
- **2026-08-05 (issue #106 — a global counter in a test binary tallies the other tests):**
  The allocation harness used `static AtomicU64` counters with an `ARMED` flag. `cargo test`
  runs test functions on several threads in one process, so armed measurements absorbed every
  other test's allocations: a 24×80 pane "cost" 21,709 allocations against a 240×240 pane's
  12,160, and one control subtraction came out *negative*. Thread-local `Cell<u64>` with const
  init fixed it (and `try_with`, since TLS is gone during thread teardown). Under nextest each
  test gets its own process and the bug is invisible — it only appears under plain `cargo test`,
  which is what a contributor runs.
- **2026-08-05 (issue #106 — a differential test cannot see a bug both arms share):**
  The reuse-on/reuse-off proptest caught a broken pristine check in four steps and shrank it to
  a minimal case. It did *not* catch deleting `mark_all_dirty()` from the swap — both arms call
  the same `ScreenSwap`, so shared-path defects are structurally invisible to it. The existing
  `dirty_alternate_screen_enter_and_leave_are_full_frame` caught that one. Differential testing
  proves an *optimisation* is unobservable; it is not a substitute for absolute assertions on
  the path both sides run.
- **2026-08-05 (issue #106 — a frame-dropping recorder hides the freeze you are recording):**
  The first demo video recorded a counter in a shux pane and showed it ticking happily under
  attack. VHS emits fewer frames when the screen stops changing, so a stall compresses into
  nothing — the recorder erases exactly the evidence. Fix: put the proof in a *single frame*
  instead of in motion. Two copies of one animation whose position is a pure function of the
  wall clock, one rendered through shux and one not, stay in phase on their own; any lag the
  daemon adds shows up as the two bars being in different places. 0.76 s apart before, 0.04 s
  after, legible in a still. Also: a pane's geometry is capped, and one 240×64 pane on a 4-core
  box only produced ~90 ms of lag — six of them were needed to make it watchable, which is
  itself an honest scenario.

## 2026-08-05 — issue #115, DEC 2026 synchronized output (task 089)

- **A deferral fixes the trigger, not the ceiling.** Deferring the synchronized-output
  freeze to the first write took `ESC[?2026h ESC[?2026l` to the parse floor and left
  `ESC[?2026h a ESC[?2026l` at 87 KB and a **51x** end-to-end latency regression, because
  the interleaved character legitimately takes the copy. Ask separately what the cheapest
  trigger costs and what the WORST window costs; a fix can move one a millionfold and the
  other not at all.
- **A retained snapshot has a second, invisible price.** Holding a frozen grid does not
  just cost what it cost to take. Every line it references is a line the live grid can no
  longer recycle as it scrolls, so the live side allocates a replacement instead —
  29 MB for 416 bytes, spread across the scrolls rather than paid up front, and therefore
  invisible to a benchmark that only measures the freeze. Copy-on-write moves the cost, it
  does not remove it; the only way to remove it is to reference fewer rows.
- **"Presented frame" and "everything a reader can see" are different sets.** The frame
  mode 2026 promises to hold still is the viewport. Scrollback is reachable from copy mode
  but is not part of the frame, and freezing it was the whole expense. Splitting the two —
  frame frozen, history read live through one indirection — was tractable only because
  every history read already funnelled through `Grid::row(abs)` + `total_lines()`. Count
  the funnel before assuming a reader-surface change is too big.
- **Make the hook unforgettable rather than exhaustive.** A hand-maintained list of
  "places that mutate the presented frame" fails silently the first time someone adds a
  path. Wrapping each component in a guard whose `DerefMut` snapshots first means the
  parser's existing code is unchanged, reads stay free, and there is no way to reach the
  mutable state except through the freeze. It is also precise in the direction a coarse
  hook is not: `ESC[6n` never reaches `DerefMut`, so it cannot re-arm the copy.
- **A differential oracle only compares what you hand it.** The lazy-vs-eager proptest
  passed 400 cases while comparing `grid().scrollback_len()` — which both arms agreed on
  and which stopped describing anything real once the frame went viewport-only. It had to
  be re-pointed at `presented_total_lines()`/`presented_row()`, the surface a reader
  actually uses. When the shape of an observable changes, the oracle's `observe()` is the
  first thing to re-derive, not the last.
- **The suite could not detect a single write path escaping `Row::cells_mut`.** An
  adversarial agent injected a raw-pointer write into `Row::reset` alone — the path behind
  every clear and behind scrollback recycling — and all 459 tests stayed green, including
  the differential oracle written for this change. A safety argument of the form "X is the
  only way to do Y" needs a test that reintroduces a violation of it, per path, in a
  sandbox. `crates/shux-vt/tests/cow_aliasing_adversarial.rs` now walks 27 write paths.
- **The differential oracle could not see this one, because both arms shared it.** The
  viewport-only freeze reads history out of the live grid at a shifted index, and the shift
  went the wrong way: eviction removes lines from the FRONT, so survivors slide down to
  meet index 0 rather than the index sliding up to meet them. Adding the eviction count
  instead walked past the survivors into the live viewport, so content written *after* the
  freeze appeared inside the frame that exists to hide it. Both arms of the proptest call
  the same accessor, so 800 generated programs stayed green; it took an absolute assertion
  on real content under partial eviction. A differential proves an optimisation is
  unobservable. It is not a correctness test for the shared path, and the shared path is
  exactly where a refactor puts its new arithmetic.

- **Read the other implementations before designing.** Alacritty had already tried the
  snapshot-on-`?2026h` design and abandoned it, and their commit says why in one line
  ("this can happen thousands of times per frame"). They also carry two liveness bounds
  shux had none of — a 150 ms deadline and a 2 MiB cap — which turned "a crashed app
  freezes its pane for ever" from an unknown into a fixed defect in the same PR.
- **Then measure the constant against your own workload rather than copying it.** btop is
  the one installed application that genuinely drives mode 2026 (`vim` and `htop` gate on a
  terminfo `Sync` capability that shux's `TERM` does not advertise, so they are regression
  coverage for the row change, not for this path). It holds a window for **0–6.3 ms**,
  which is what makes 150 ms defensible rather than borrowed.
- **A resize is not a state to preserve through, it is a reason to stop.** Reflowing a
  frozen frame is wrong in two ways at once — no history to rewrap against, and the
  alternate screen is canvas-resized rather than reflowed — and both disappear if the
  resize simply releases the window. Deleting the interaction beat getting it right.
- **A wedged daemon hangs the harness that is measuring it.** `subprocess.run` with no
  timeout turned "the victim never answered" into a run that never finished and left
  orphans. A ceiling that RECORDS the timeout ("15 of 15 captures never returned within
  8 s") is the measurement; waiting it out is not, and neither is `|| true`.
- **VHS compresses a still screen and will erase the freeze you are filming.** A 43-second
  tape came out as 7 seconds on the fixed build and 17 on the broken one, because the
  recorder drops duplicate frames. Put the proof in a single frame — a bar whose length is
  a pure function of wall clock — and normalise the two clips' playback speed afterwards so
  they are comparable. Also: redraw the demo pane in place rather than clearing it, or a
  frame grabbed mid-redraw shows a half-erased screen and reads as a bug in the demo.
- **A regression test that asks the code under test for its expectation is vacuous.**
  The copy-mode paging fix came with a test that recomputed "how many lines are readable"
  by calling `readable_rows` — the very function being fixed — so it tiled perfectly with
  the bug reintroduced and passed either way. Stating `pane_rows - 1` literally, with the
  reason (the hint bar covers the bottom row), made it fail against the defect. Whenever a
  test derives its expected value from the implementation, check what it does when the
  implementation is wrong; that is the only thing that distinguishes a bound from a
  tautology.

- **Open the frames, every time.** The first take recorded `shux attach victim` — not a
  subcommand — and produced 20 seconds of a usage error. `ffprobe` said the file was
  valid, the right size and the right duration.

## 2026-08-06 — issue #117, DECALN (task 090)

- **A "silently ignored" sequence is not automatically a small fix.** DECALN's own
  semantics are four lines. What made the change worth care is that a full-screen write
  in shux has to satisfy invariants owned by two *other* issues: the write tally that
  licenses alternate-screen buffer reuse (#106) and the copy-on-write row sharing the
  synchronized-output freeze depends on (#115). The interesting failure mode was never
  "the fill is wrong" — it was "the fill is right and the next application in that pane
  inherits it".

- **`Grid::is_blank_canvas` reasons from `mutations == 0`.** Any new path that writes
  cells must go through something that bumps the tally, or a retired alternate buffer
  full of that content reads as a blank canvas and is handed to the next program. The
  `debug_assert!(is_actually_blank)` in `ScreenSwap::enter` is the backstop, and it only
  fires if a test happens to drive that exact sequence — so the tally bump is the
  invariant, not the assert.

- **A no-op in the alphabet of a differential test is a hole in it.** Both
  `sync_output_differential.rs` and `cow_aliasing_adversarial.rs` already fed `\x1b#8`
  as "a sequence that touches the grid". `cow_aliasing_adversarial.rs` was honest about
  it — it listed `DECALN` in an explicit vacuity guard naming the hammer cases known to
  write nothing. That guard is what turned "implement DECALN" into "and now remove the
  entry, and the surrounding assertion becomes real". **Write the vacuity guard.** A
  test suite that cannot tell you which of its cases proved nothing will not tell you
  when one of them starts mattering.

- **Mutation-test the fix, not just the bug.** Ten mutations (drop the tally bump, drop
  the wrap clear, drop the margin reset, drop the cursor home, assign `ch` only, fill
  scrollback, clip to the scroll region, bypass the freeze, drop the dirty mark, fill
  with the SGR pen) were each applied and each killed by a *named* test. Two of them
  (`ch`-only, and filling with the pen) initially survived — the tests that should have
  caught them were passing vacuously, because the screen under test started blank and
  unstyled. Both tests were strengthened to draw first.

- **`wait-settled` cannot tell "finished" from "not started".** The first evidence run
  photographed panes mid-script and produced a scene that looked exactly like the
  content leak the fix prevents — a screen of `E` where the next application should
  have been. It was the harness. Every scene now has its pane touch a done-file after
  its last write; the harness waits for the file, *then* settles.

- **And the pane must be the right size before it draws.** A pane spawns at the daemon's
  default geometry and is resized a moment later. A script that draws before the resize
  lands gets *reflowed*, which for a screen of `E` means rows of 48 alternating with
  rows of 32 — a picture that looks like a rendering bug and is not one. The scenes now
  block on a go-file the harness touches after `pane set-size`.

### Gate round two — what the QA gate found that the implementer's own review did not

- **A gate that returns FAIL on the evidence rather than the code is still right.**
  The `shux-vt-solid-qa` gate did not dispute one line of the DECALN change. It failed
  the task for shipping with no tracked QA record — and on the way there it found two
  defects in *shared verification machinery* that had been wrong for eight tasks.

- **`.claude/automations/pixel_verify.py` had been writing fully transparent diff
  PNGs.** `ImageChops.difference` on two opaque RGBA images yields alpha 0 in every
  pixel, and `point(lambda v: 255 if v else 0)` maps 0 to 0. Every diff image the tool
  ever produced was a valid PNG of the right size that rendered blank. The numeric
  metrics were always correct, which is exactly why nobody noticed: the JSON said
  `changed_pixels: 34778` while the picture beside it showed nothing. **The gate clause
  "the diff image reveals obvious defects even if the numeric threshold is permissive"
  was unexercisable repo-wide.** Fix: diff on RGB so the saved PNG is opaque. Of the ~50
  committed diffs, the 19 with a committed input pair were regenerated and their metrics
  reproduced exactly — all 19 are genuine zero-difference cases, so nothing was hidden;
  but that was luck, not design.

- **`pane capture` defaults to `--lines 50`.** A harness that captures a pane taller
  than 50 rows and asserts on row 1 fails for a reason that has nothing to do with the
  thing under test — and looks exactly like a grid silently dropping its top rows. The
  gate reported it as a 50-row grid clamp with content loss. It is not: a 60-row pane
  keeps a 60-row grid, `stty size` says 60, and `ESC[60;1H` lands on row 60. **Reproduce
  before believing — including a gate's findings.** Four of its six substantive findings
  were real and fixed; one was a misdiagnosis of my own harness bug; one was a follow-up.

- **Gating assertions on the output label masks failures.** `if [ "$label" = after ]`
  around the failure counter meant every other label printed FAIL lines and exited 0.
  Recording a known-broken baseline has to be an explicit opt-in (`EXPECT_DEFECT=1`),
  and that mode must fail when the defect does *not* reproduce — otherwise the baseline
  arm passes vacuously too. Both directions were exercised: four label/flag combinations,
  four expected exit codes.

- **Measure the measurement.** The first run of that four-way check printed `exit=0` for
  a case that had actually exited 1 — `cmd | tail -4; echo $?` reports `tail`'s status.
  The check that verifies a harness can fail is itself a harness that can fail silently.

- **VT gate enforcement keys off a hand-written task-file field, not the touched
  surface.** `scripts/check-progress.sh` only demands QA artifacts when a task file says
  `**Quality Gate:** shux-vt-solid-qa` or `Milestone: VT Quality Track`. An M3 task can
  touch `shux-vt`, capture, cursor, alt screen and scroll regions — every row of
  CLAUDE.md's gate table — and be waved through. Tasks 087, 088, 089 all did. Worth its
  own task: enforcement should follow the diff, not the prose.

### Gate round three — what the adversarial agents found

- **The dangerous finding was on a surface the fix made dangerous, not one it
  touched.** `ESC[?47h` — the original xterm alternate-screen mode — was never
  implemented, so it fell through unhandled and apps using it drew on the primary
  screen. That was survivable while DECALN was a no-op. The moment DECALN worked,
  the same gap destroyed the user's whole page with nothing to restore. **Ask not
  only "what did I change" but "what did I make load-bearing".**

- **A matrix test can drive the broken case and still pass.** The alternate-screen
  permutation test included `?47` and asserted the next application got a blank
  screen. Entering `?1049` always yields a blank screen, so the assertion held
  while `?47` was destroying the primary. The missing assertion was the boring
  one: *did the thing we were protecting survive?* Coverage of an input is not
  coverage of an outcome.

- **Losing two hours of agents to a container restart cost nothing because the
  commits were already pushed.** Transcript mtime is NOT a liveness signal — a
  completed agent stops writing exactly like a dead one, and a working agent's
  transcript can sit unflushed for 7 minutes while it starts daemons. Watch the
  work (runtime dirs, probe files, daemon ages), and detect restarts directly by
  reading `/proc/uptime` going backwards.

- **An agent's root-cause can be wrong while its observation is right.** One
  reported a "50-row pane grid clamp losing content"; the grid was fine and the 50
  was `pane capture`'s documented `--lines` default — but that default WAS
  silently truncating my harness. Another reported `less` leaving `E` residue,
  then recorded the raw PTY bytes both ways and dismissed its own finding: `less`
  emits an identical repaint stream either way and relies on being at the bottom
  row to scroll blank lines in, which DECALN's cursor-home prevents. That second
  one is the standard to aim for.

## 2026-08-07 — issue #120, entity id references (task 092)

- **A one-way abbreviation is a bug even when both halves are correct.** Every
  listing printed the first 8 characters of an id; every id parameter demanded all
  32. Neither piece was wrong on its own, and nothing in either code path looked
  suspicious. The defect only exists in the seam. **Whenever output is shortened,
  ask what happens when someone types it back** — for ids, filenames, hashes,
  anything a person is meant to copy.

- **`glance` echoed the short form in its own SUCCESS line.** That is the
  strongest possible signal and it had been on screen the whole time. A command
  whose success output cannot be fed back into the same command is worth a second
  look.

- **Making the change strictly additive is what made it safe.** A complete UUID
  resolves *without consulting the graph at all* — same code path, same errors as
  before. Only prefixes take a lookup. That single rule means no currently-working
  call can change behaviour, which is worth far more than the tidiness of routing
  everything through one uniform path.

- **The fix exposed three defects in the error-reporting machinery that had been
  invisible because nothing informative ever reached it.** `rpc_display` threw
  away the message of every error the CLI composed itself and printed
  `resource not found` instead; its `name_conflict` arm was keyed on `-32003`
  (`auth_required`) instead of `-32007`, so a duplicate session name printed a raw
  error code with the colliding name sitting unused in `data`; and the lens verbs
  printed only the error's code name, discarding the actionable `data.detail`.
  **Adding a good message to a channel nobody had tested is how you discover the
  channel is broken.**

- **Echoing the caller's own text back is a new egress path.** The moment
  `rpc_display` started printing client-composed messages, the escape-injection
  suite (issue #104) went red — a hostile window selector was being quoted
  verbatim to the terminal. The guard belongs in `rpc_display`, the single funnel
  every printed RPC error passes through, not at the dozen sites that compose one.
  That the existing suite caught it immediately is the argument for keeping such
  suites broad.

- **`--cmd` is documented as a shell command and is split on whitespace.** Cost
  an hour: the test fixture `--cmd "printf 'X\n'; sleep 300"` exec'd `printf` with
  `sleep` and `300` as arguments, `printf` exited, the pane's PTY went away, and
  `pane send-keys` failed with `pane PTY not found` — which reads like a bug in
  the thing under test. **Use trailing `-- sh -c '...'` for anything with shell
  syntax.** Filed as #125.

- **Statistically rare is not unreachable — measure before declaring it
  unstageable.** The ambiguity path (one prefix, two entities) looked impossible
  to demonstrate with random v4 ids. It took 1,000 concurrent sessions to force a
  4-hex collision (620 produced none), but it took 40 seconds and 150 MB to find
  out, and it turned "covered by unit tests, trust me" into a screenshot of the
  real binary refusing a real collision. **Try the brute-force route once before
  writing the paragraph explaining why you did not.**

- **The background shell's exit code is not the command's.** `make check > log
  2>&1; echo "EXIT=$?"` reported success while clippy had failed — the task
  notification reports the *shell's* status. Grep the log for `Error`/`FAILED`
  rather than trusting the wrapper's exit code.

- **Open the video frame, not the file listing.** The first before/after take was
  a valid 15-second MP4 in which every single command failed with
  `Permission denied` — the recorder ran as an unprivileged user against a
  root-owned socket. It would have shipped as "proof of the bug" and every failure
  in it would have been the wrong failure.

## 2026-08-07 — issue #125, `--cmd` shell semantics

**A test's colour probe can pass on text and fail on colour.** The pre-fix screen for
`--cmd "printf '<probe>'; …"` reads
`'TRUECOLORprintf: warning: ignoring excess arguments, starting with '\033[38;5;208mINDEXED\033[0m'`
— printf quotes the arguments it refused back at the screen, so the literal words
`INDEXED` and `BASIC` appear as **uncoloured text inside an error message**. A
`grep INDEXED` on `pane capture` calls that a passing colour probe. `pane glance
--cells` gives style runs; asserting "a run reading INDEXED carries 256-colour 208"
is the assertion that cannot be satisfied by a mangled screen. Beware also that a
run is `[col, text]` when it carries the default pen and `[col, text, style]` when
it does not — an unconditional three-way unpack raises on exactly the uncoloured
rows the check exists to reject.

**A before/after evidence harness cannot always resize the pre-fix pane.** The
defect here killed the pane's PTY immediately, so `pane set-size` failed outright
and took the harness down before it could shoot anything. Both arms now shoot at
the daemon's default 80x24, which removes the failure and makes the two sides
pixel-comparable for free.

**`env::var` returns `Ok("")` for a set-but-empty variable.** `std::env::var("SHELL")
.unwrap_or_else(|_| "/bin/sh")` therefore never fires its fallback for `SHELL=""`
and the pane execs the empty program name. Two places in this repo resolved `$SHELL`
and only one of them filtered blank, so one daemon gave a working `--cmd` pane and a
dead default pane. Filter blank, always.

**Deriving a display name from a shell script needs the first *simple command*, not
the first token.** The first cut skipped `NAME=value` prefixes token by token, so
`A=1;htop -d 10` — one token that is a complete assignment — was skipped and the
scanner landed on `-d`, a flag belonging to a command it had never established.
Splitting on shell operators first (`| & ; < > ( ) \``) bounds the search to one
command, and it also makes `ls|wc` read as `ls` rather than falling back to the
shell's name.

**`pkill -f <pattern>` matched the checking shell itself and killed this session.**
The pattern `/tmp/vhs/shux __daemon` appeared in the invoking bash's own argv, so
`pkill -f` reaped the caller. Identify processes by pidfile; the repo rule exists
because this really happens.

**VHS: overwriting the binary between takes fails with ETXTBSY** when the previous
take's daemon is still executing it, and `cp && … && clear` swallows the failure —
the second take silently records the FIRST binary. Put each build in its own
directory and switch `PATH`, so nothing is ever overwritten, and verify by md5 that
the take used the binary you meant.

**A spawn failure that returns success is worse than a crash.** `let _ =
spawn_pane_pty(...)` at five call sites turned "your program does not exist" into a
session that reported `✓ Created`, listed a pane with `exit_status: null`, and
failed every subsequent verb with "pane VT not found". `state.apply` already had the
right instinct (per-pane `spawn_results`); the single-pane verbs now roll back and
return `SPAWN_FAILED`.

**Attach was resurrecting dead panes.** The spawn-if-no-writer branch exists to close
a race with a freshly created session, but a pane that *exited* is indistinguishable
from one that has not started yet if you only look at the writer table. `exit_status`
is the discriminator. It was also spawning with `Vec::new()` and the daemon's cwd
rather than the pane's own — so the race it closed, it closed with the wrong process
in the wrong place.

### Round two — the fixes needed fixing

**Rollback is not just "undo the entity".** Creating a window focuses it and
creating a pane focuses it, and the destroy paths hand focus to *whatever the
container yields first* — the session's first window, the layout tree's first
pane. So an error path that only destroys the entity silently relocates the
operator: active window `three` → `1`, and every later `-w`-less verb then
targets the wrong window. Capture the prior focus before the create and restore
it after the destroy. The generic destroy paths are right for a deliberate kill
and wrong for a rollback; the difference is the caller's, not theirs.

**`MAX_ARG_STRLEN` counts the terminating NUL.** `PAGE_SIZE * 32` is 131072 and
the longest argument that actually fits is 131071. The first cut capped at
131072 — and wrote a unit test asserting that exactly-131072 was acceptable,
which passed, because the unit test and the bug shared the same wrong constant.
It took an agent bisecting through the real binary to find it. **A boundary
constant needs a test that spawns**, not a test that compares the constant to
itself; `crates/shux/tests/pane_command_e2e.rs` now pins both sides against a
real `execve`.

**A per-item cap does not bound the aggregate.** Forty individually-legal 100 KiB
arguments are 4 MiB and still `E2BIG`, and the spawn-failure hint then blamed
`argv[0]` and the cwd, neither of which was wrong.

**A flag is not a program name.** The title extractor rejected shell syntax but
not a leading `-`, so `--cmd "-n is a valid sed script"` — the example printed
in the flag's own help — titled the pane `-n`. And `basename` returns the whole
token when there is no file name in it, so `/`, `..` and `+++` became titles
verbatim. Require at least one alphanumeric character and no leading dash.

**`--dry-run` must run the same validation the real path runs.** The argv rule
lived only in the daemon, so the flag whose entire purpose is "will this
succeed?" answered yes to templates the real run rejects. One function, called
from both.

**A green tick over a failed spawn is the same bug as issue #125.**
`state.apply` deliberately does not roll back a partial batch, which is
defensible — but it printed `✓ Applied` and exited 0 when *every* pane failed,
so `shux state apply t.toml && shux attach` walked straight into dead panes.
Not rolling back is a policy; reporting success is a lie.

**Invisible characters spoof titles without needing bidi.** `U+200B` is not
`is_control()` and renders as nothing, so `ht<ZWSP>op` and `htop` are
indistinguishable on a border. Same class as the Trojan Source set already
handled; `U+200D` ZWJ still has to survive because emoji need it.

### Round three — the fixes for the fixes needed fixing

**"Capture before, restore after" is a lost update.** Round two's focus restore
read the active window before the create and wrote it back after the rollback —
so an operator who moved focus while the PTY was starting had their choice
silently reverted, measured at 14/30 with two concurrent clients, *worse than
the 12–15 % baseline the restore was added to improve*. The fix is
compare-and-restore: put focus back only if it is still on the entity being
undone (1/30). Any restore-a-captured-value pattern needs that guard.

**A statistical race test with a sleep in it discriminates nothing.** The first
attempt slept 20 ms between starting the doomed create and issuing the focus,
which let the create finish first — the reverting build passed it. Both clients
have to be in flight at once. Verify a race test against the build that has the
bug, or it is decoration: 12/16 reverted on the old build, 0 on the new one,
only after the sleep came out.

**A cap you cannot compute is worse than no cap.** `MAX_ARGV_BYTES` was wrong in
both directions, proven by measurement: `ARG_MAX` is shared between argv and the
environment, so a 1.2 MB environment still produced `E2BIG` under a 1 MB argv
that passed the check, while an ordinary environment exec'd 1.5 MB that the
check refused. No number available in the process is the kernel's number. What
the cap was really for — an oversized argv failing with a diagnosis that blamed
`argv[0]` and the cwd — belongs in the error message, not in a guess.

**An allowlist where the property is what matters will always be incomplete.**
Round two stripped the three invisible characters a reviewer happened to name;
fifteen more survived, and a program in a pane can set its own title via OSC 0
without any operator cooperation. Enumerate the Unicode property
(`Default_Ignorable_Code_Point`), not the examples.

**Judge a program name by the path, not by its glyphs.** "Reject a token with no
alphanumeric character" also rejects `/usr/local/bin/+++`, which is a real
program. `Path::file_name()` returning `None` is the honest test for `/`, `//`,
`.` and `..`.

**Fixing the human output path is half a fix.** `state apply` stopped calling a
batch of dead panes a success — in text mode. `--format json`, the mode scripts
and agents actually use, returned before the check and still exited 0.

### Round four — removing a cap removed a protection nobody had named

**A bound can be wrong as a prediction and right as a policy.** Round three
deleted the aggregate argv cap because it could not predict `execve` — true, and
the right call for *pre-flight rejection*. But the cap had been incidentally
bounding something else: what the graph stores and what `pane.list` echoes.
`state.apply` keeps a pane whose spawn failed, so a few multi-megabyte argvs
pushed the response past the 16 MB frame limit and every read of that session
died with `early eof`. The cap is back with an honest justification — shux's own
storage bound — and the kernel's limit stays the kernel's business.

**"Not in this batch's failures" is not "alive".** The focus rescue asked the
wrong question and handed focus to a corpse left by an earlier apply, which is
precisely what it was written to prevent. The daemon already knows the truth:
`io_state.writers` holds exactly the panes with a live PTY.

**`HashMap::values().find(...)` is a coin toss.** The same template focused a
different pane on each run, so a script that applied and then used the focused
pane targeted something different every time. Iterate the layout tree, which has
an order that means something.

**An exception list has to cover the whole reason it exists.** ZWJ was exempted
from the invisible-character sweep because dropping it splits family emoji. The
variation selectors are mandatory in the same RGI sequences and were not
exempted, so `❤️‍🔥` became `❤` and `1️⃣` became `1` — the rule broke exactly what
the exception was protecting.

**Widening two rules at once opened a gap between them.** Round three widened
what counts as a program name *and* what the sanitizer strips; a command whose
name sanitizes to nothing then produced an empty title, because the fallback to
the cwd only ran when no name was found at all — not when the name evaporated.

**A test built from the wrong ingredients passes on the broken build.** The
first corpse test put its corpses in separate sessions, so they were never
siblings and the rescue never saw them. Only `state.apply`'s `split_pane` op
leaves a corpse in an existing window — `pane.split` the RPC rolls one back.
Three green runs against the known-broken binary is the signal to rebuild the
fixture, not to trust the test.

## 2026-08-08 — task 095 (issue #135): `pane list` title + argv quoting

**`UnicodeWidthStr::width` is a property of the STRING, not the sum of its
characters.** `☀️` (U+2600 U+FE0F) is 1 if you add up its characters and **2** as
a string; a ZWJ family is 6 added up and **2** as a string. A truncator that
accumulated per-character widths therefore returned cells wider than the width
it was asked for, `pad_right` then added no padding, the row under-reported its
own visible length, and the box printed lines up to 240 columns wide in an
80-column terminal — at 100 of the 177 widths swept. Everything else in
`style.rs` measures whole strings, so anything new there must too. Measure the
accumulated string; never sum.

**A guard whose numbers restate an arithmetic done elsewhere will drift.** The
minimum boxable width was written down as 24, derived by hand from "an id and a
`◀ focus` marker are 18 columns". A *zoomed* pane's marker is `◀ focus
[zoomed]` — 16 columns, not 7 — so the stated guarantee was wrong by seven and
the test that asserted it used only unzoomed panes, which is why it was green.
Derive the constant from `pane_marker(true, true)` and put the zoomed case in
the sweep.

**Making quoting correct can turn a transient failure into a permanent one.**
`pane.run_command` types its line into the pane's *terminal*, so the tty line
discipline reads it before any shell does. The old denylist left a `0x03`
unquoted, and the line discipline's truncation happened to leave valid shell
behind — one failed call, then recovery. Correct quoting puts the byte *inside*
the single quotes, so the truncation now leaves an unterminated quote: bash
drops to `>` and swallows every later command sent to that pane, forever.
**Validate for the sink you actually have.** `reject_unexecutable` was written
for `execve`, where NUL is the impossible byte; this sink is a tty, where NUL is
the harmless one and the signal bytes are fatal.

**An `expect ... no <pattern>` check passes vacuously when the file is missing.**
The first run of the evidence harness passed `titles` where the helper wanted
`titles.txt`, so `grep` errored, `hit=0`, and every "must NOT contain" assertion
reported ok — against a file that did not exist. A missing artifact has to be a
harness failure, not a satisfied negative.

**A baseline arm can hide the defect it exists to show.** The injection scene
used `A;touch FILE` — which contains a space, which the *old* denylist quoted.
The pre-fix arm dutifully reported no injection. A probe must not rely on any
character the code under test happened to handle; the space-free `A;>FILE`
reproduces.

**A control that asserts something only the fixed build can show is not a
control.** `expect_always ... "/bin/sh"` read the rendered box — which pre-fix
has no COMMAND column — so it failed on the baseline arm for the very reason the
task exists. Controls must read a path both binaries share; here that is the
RPC's own JSON, which this task did not change.

**`-w 1` is the second window.** `window list` shows the default window at index
0 with the name `1`, and index resolution runs before name resolution — so the
evidence harness photographed the viewer window and looked plausible doing it.
Use ids in automation. Filed as #142.

**Idempotence and injectivity can be mutually exclusive.** `safe_label` maps a
control character to the text `\u{a}`, which is ambiguous with a literal
`\u{a}` — two different argvs print identically. Escaping the backslash fixes
that and breaks `test_safe_label_is_idempotent`, which `print_success` depends
on. Filed as #141 rather than traded away in passing.

**Equivalent mutants are not survivors.** Swapping the egress guard and the
quoting looked like a real mutation and no test killed it — because the two
orders are provably identical (every character the guard rewrites already fails
the quoting allowlist; 200,000 random argvs, zero differences). Counting it as a
survivor would have meant writing a test that cannot fail. Check whether a
mutant is equivalent before treating a survivor as a coverage gap.
