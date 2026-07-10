# shux — Implementation Progress

> **STRICT RULE:** This file MUST be updated at the end of every coding session.

## Current Phase

**M0: Architecture Spike** — **Complete** (000–012).
**M1: Daily-Driver Core** — In progress, ~88% by task count.
- **Done:** 013, 014, 015, 016, 017, 018, 019, 020, 021, 022, 023, 026, 027, 029, 033, 060, 062, 063, 064, 065.
- **Partial:** 024 (theme: only border + status-bar overrides — full token cascade pending), 028 (cap negotiation: TERM_PROGRAM claimed, no DA2/XTVERSION query yet), 031 (attach keybinding config/validation landed; runtime keybinding RPC/plugin provenance deferred).
- **018 (Tier-1 keys) finalized (PR #13):** Bare Alt+h/j/k/l → directional focus; Alt+n/p → next/prev window; Alt+1..9 → switch directly to Nth window via new `ActionKind::SwitchToWindow` + `ActionArgs.window_index`. `key_to_bare_action` return type bumped from `Option<ActionKind>` to `Option<(ActionKind, ActionArgs)>`. Out-of-range Alt+digit silently ignored (matches tmux). 4 new unit tests in `crates/shux-ui/src/attach.rs`.
- **027 (pane titles) — Done (PR 4 / #12):** `Pane` gained `manual_title: Option<String>`, `osc_title: Option<String>` alongside existing `title` + `auto_title`. Priority resolution: manual > osc (when auto on) > command basename > cwd basename, computed by `Pane::recalculate_title()`. `sanitize_title()` strips control chars + clamps to 64 chars. `set_pane_title()` and `set_pane_osc_title()` on SessionGraph fire `PaneTitleChanged` only when displayed title actually moves. Per-pane PTY task tracks `last_osc_title` locally and forwards changes to graph outside the io_state lock (deadlock avoidance). Compositor `MultiPaneFrame` gained `titles: Option<&HashMap<PaneId, String>>`; titles render as ` title `-padded text on the top border row, truncated to fit. `pane.set_title` RPC accepts `{title: string|null, auto: bool|null}` tri-state. `shux pane title` CLI with `-t/--clear/--auto/--no-auto`. 11 model unit tests + 5 graph unit tests.
- **Pending:** 025 (per-pane theming), 032 (command palette), 034 (M1 quality gate). (030 — session templates — moved to M2 group as part of PR 3a since it lands alongside `state.apply`.)
- **Done:** 061 (render parity analysis + mouse copy UX follow-up).

**M2: API + Plugin System** — kicked off.
- **Done:**
  - **036 (events.watch + events.history)** — control-plane events only (PR 2a). EventBus wired, SessionGraph publishes typed events on every successful mutation. `shux events watch`/`history` CLI. Subscribe-FIRST / history-SECOND / dedup-by-seq.
  - **030 (session templates + `shux apply`)** + the foundational pieces (PR 3a):
    - Every pane-scoped EventData variant carries `session_id` + `window_id` (taxonomy fix; `events.watch --session` filter now works correctly over historical events).
    - `EventMetadata.correlation_id: Option<String>` + `EventBus::publish_with_correlation()` so subscribers can attribute event bursts.
    - **Staged-snapshot transaction primitive** (`SessionGraph::apply_batch`): clone snapshot once, validate all ops against staged, mutate staged, collect events in `Vec`, commit ONCE, then publish events with a shared `correlation_id`. Atomic at graph level; PTY spawn happens after commit and surfaces per-pane outcomes in `BatchResult::spawn_results`.
    - Generic `Op` enum (`crates/shux-core/src/apply.rs`): `CreateSession` / `CreateWindow` / `SplitPane` with `SessionRef::BackRef{ref_op}` / `PaneRef::BackRef{ref_op}` so templates express "the session I just created" without round-trips.
    - `state.apply` RPC method — generic delta ops (NOT template-shaped per codex P0 #2; future SDKs / MCP servers target the same primitive).
    - `shux apply <template.toml>` CLI parses PRD §10.3 TOML, lowers to ops. `--dry-run` and `--watch` flags.
    - Fixes existing bug (codex P2 #10): graph create APIs persist `initial_command` so PaneCreated events stop lying about empty command.
  - **037 (optimistic concurrency surface)** — Done (PR 3b / #11). `expected_version: Option<u64>` plumbed through every mutating RPC (session.rename/kill, window.kill/rename/focus/reorder, pane.kill/resize/zoom/swap). `GraphError::VersionConflict` now carries `resource` + `id` so `RpcError::version_conflict` produces the full `data` shape (resource, id, expected_version, actual_version, hint) per PRD §8.3. Layout ops (resize/zoom/swap) bump the version of every pane in the affected window. `--expected-version` CLI flag on session/window/pane subcommands. `shux api` now wraps the JSON-RPC response in `{result | error}` envelope so agents can parse structured errors. L4 visual test in `.claude/automations/test_pr3b_optimistic_concurrency.py`.
  - **PR 2c — sampled pane.output events** — Done. Separate `data_plane: broadcast::Sender<PaneOutputEvent>` channel in `EventBus` with NO history — the secret-leak vector is sealed. Per-pane PTY task rate-limits at the source: ~10 chunks/sec/pane via `output_sample_interval = 100ms` + 64KB pending buffer cap. `pane.output.watch` RPC long-polls the data plane with pane_id filter and `from_seq` resumption. `shux pane watch -s X -p Y [--limit N]` CLI base64-decodes chunks to stdout. 4 new unit tests pin the data-plane / control-plane separation. Design rationale in `docs/PR2c-DESIGN.md`. L4 visual test in `.claude/automations/test_pr2c_pane_output.py` (A1..A6 green).
  - **Post-merge followups (PR #15):** (1) `session.create` RPC now persists `command` onto `Pane.command` via new `create_session_with_command(name, cwd, command)` method — closes the codex P2 #10 leftover so `shux new --cmd vim` auto-derives title as "vim" instead of cwd basename. (2) `.config/nextest.toml` added with retry override (2 retries) for known-flaky tests: `test_spawn_echo`, `test_m0_pty_spawn_echo`, `test_tcp_auth_required`. Default retries=0 elsewhere so new flakes stay loud. (3) `test_tcp_auth_required` gained an in-place comment documenting the bind→drop→re-bind TOCTOU race as a `KNOWN FLAKE`.
  - **066 (lossless pane output recording)** — Done. `pane.record.start` / `pane.record.stop` add a daemon-owned raw PTY recorder for byte-exact audits while keeping `pane.output.watch` sampled. Recorder state reports `complete|error|aborted`, byte count, `lossless`, and error detail; v1 allows one active recorder per pane, applies explicit backpressure, supports daemon-side `duration_ms`, resolves CLI `--to` paths client-side, and protects existing files unless `--force` is used. `shux pane watch` help now says absence assertions are unsound and text/plain mode warns on sampled chunks.
  - **075 (plugin DX v0.5 and OCP extraction)** — Done. Local-only plugin authoring and lifecycle foundation: plugin feature command dispatch boundary, `sh` scaffold/create/init, manifest-directory validation and canonicalized directory installs, package name/version handshake checks, `plugin stop` lifecycle alias, existing permission/audit path preservation, and leak-guarded Shux dogfood QA.
- **In Progress:** none.
- **Pending:** 035 (complete RPC surface). 038–050 (plugin host + bundled plugins + MCP).

**VT Quality Track** — in progress.
- **Implementation order:** 073 first, then 067/068, then 069-072, then 074.
- **Done:** 073 (VT corpus regression harness), 067 (resize reflow), 068
  (wide-cell invariants), 069 (grapheme-aware cell storage), 070
  (DEC special graphics), 071 (tab stops), 072
  (origin-mode/scroll-region semantics), 074 (dirty-region tracking).
- **In Progress:** none.
- Every task in this track requires DootSabha design review, DootSabha
  implementation-diff review, unit/integration/raw replay/shux automation
  coverage as applicable, full-resolution visual evidence, pixel-level PNG
  verification, and a tracked `.shux/qa/<task>/SOLID-QA.md` hard-gate
  `VERDICT: PASS`.

**M3: Polish** — not started. Release pipeline + binary distribution already exist.

shux is a usable interactive multiplexer end-to-end (multi-pane render, attach client, Tier-1 + Tier-2 keybindings, scrollback-backed copy mode, direct mouse selection/copy, TOML config + hot reload, themed border + status bar, help overlay, script-driven status segments, session save/restore).

## Status

### Milestone Targets

- [x] **M0: Architecture Spike** (tasks 001–012)
  - [x] Daemon skeleton with fork-before-tokio daemonization
  - [x] PTY manager with async I/O
  - [x] Virtual terminal grid (vte + VecDeque)
  - [x] Minimal TUI client (single pane)
  - [x] JSON-RPC server on UDS (system.version, system.health, session.list)
  - [x] Basic input decoder (legacy + Kitty)
  - [x] `shux` binary with `new`, `attach`, `ls`
  - [x] L1 + L2 tests passing

- [ ] **M1: Daily-Driver Core** (tasks 013–034)
  - [x] Full session/window/pane CRUD (API + CLI)
  - [x] Splits, directional focus, resize, zoom, swap
  - [x] Copy mode with clipboard (OSC 52)
  - [~] Graded keybindings — Tier 2 (prefix) full; Tier 1 (bare) partial (Alt+arrows/Enter/|/\\/-/z/x/Tab; bare Alt+h/j/k/l + Alt+n/p + Alt+1..9 still missing)
  - [x] Help overlay
  - [ ] Command palette (`Prefix + :`)
  - [x] TOML config with live reload
  - [~] Theme engine (border + status bar overrides; full token cascade + per-pane theming pending)
  - [x] Mouse support
  - [x] Pane titles (manual + auto)
  - [x] Status bar (built-in 3-zone + script-driven `[[statusbar.segment]]`)
  - [x] Session templates + `shux apply` (PR 3a)
  - [~] Keybinding config + conflict detection (attach config landed; RPC/plugin layer pending)
  - [x] Scrollback-backed copy mode
  - [x] Session save/restore
  - [x] L1–L4 tests passing
  - [x] Dogfooding begins

- [ ] **M2: API + Plugin System** (tasks 035–052)
  - [ ] Complete JSON-RPC API surface
  - [x] Event stream with filters and sequence numbers (036, PR 2a)
  - [x] state.apply batch + templates (030, PR 3a)
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

**2026-07-10 — fix(lens): PR #91 bot round — heat pixel budget + LENS-R-038b default-color diffing + greptile docs/perf (task 077, gate 27/10 held)**
- **codex P1 — heat memory:** `pane.diff_since{heat_png:true}` rasterized the full RGBA image before the 8 MiB PNG check; a 1000×1000 pane (valid per set_size) could exhaust daemon memory. Fix: shared `lens_pixel_budget_check` (the EXACT 16M-pixel glance/snapshot cap, glance refactored onto it — byte-identical error) now gates the heat path inside the lock BEFORE any allocation, AFTER the LENS-R-033 lookup (stale/invalidated wins over payload). Hint names `heat_png=false`. Tests: `lens_pixel_budget_check_guard_predicate` (unit, 162M>16M exact values) + `diff_heat_budget.rs` black-box (1000×1000 F1 pane: -32013 with `{pixels,max_pixels,hint}`, CLI `--heat` exit 5 + no file written, heat-less diff on the same pane still succeeds).
- **codex P2 — ADJUDICATED as LENS-R-038b:** checkpoints now capture the pane's OSC 10/11/12 defaults (`PaneCheckpoint.default_colors`, read in the same critical section as the grid clone in BOTH writers); `compute_lens_diff` resolves `Color::Default` against each side's respective defaults — a changed fg/bg default marks every cell that is Default in that channel on both sides; concrete-colored cells stay unmarked; OSC 12 (cursor) never marks (DEC-11); unchanged defaults are byte-identical to raw equality (D-tier 6/6 green incl. `d2_heat.png` byte-identical — F4 never touches defaults). Tests: (a) unit `compute_lens_diff_default_color_change_marks_default_cells` + black-box `diff_default_colors.rs` (REAL OSC 11 `#204060` through a token-paced exec'd sh pane: exactly 395/400 cells, full-width regions split around the 5 truecolor COLOR cells); (b) `compute_lens_diff_unchanged_defaults_matches_raw`; (c) heat base uses CURRENT defaults — unit pixel math + black-box probes (changed blank cell = heat-over-#204060 = (97,50,75); unchanged truecolor cell = desat50 = (58,103,73)).
- **greptile P2s:** checkpoint exit-code doc now enumerates INVALID_PARAMS→2 (their text); diff CLI help enumerates PAYLOAD_TOO_LARGE in exit-5; `diff.changed_mask` MOVED into the heat closure (clone dropped). Debug find: first black-box draft used a plain prompt-bearing shell — the long bash prompt wrapped at 40 cols and SCROLLED the pane (shifting all rows, 400/400 marked); rewritten as an inline `exec sh -c` token-paced script (fixture pattern, promptless, EOF-safe read loop).
- Gates: lens gate 27/10 (identical red set) · D-tier 6/6 vs ratified goldens · new tests 2/2 black-box + 4/4 unit · `make test-rpc` · full lanes · lint clean · leak-guard clean. Pushed to PR #91; replies on all 5 bot threads.

**2026-07-10 — fix(lens): P4 convergence round 1 — invalidation gating + store-marker guard + wire-shape pins (task 077, gate 27/10 held)**
- Verifier VERIFIED-WITH-NOTES, goldens RATIFIED (3d70a31). Codex 1B+1M / claude 1B+1m, all fixed on-branch; both P4 design calls (cursor-less heat base, Rec.601 luma) ruled ACCEPTABLE — no rendering change, ratified goldens + SOLID QA stand.
- **Codex B — checkpoint resurrection:** `store_checkpoint` now refuses revisions STRICTLY BELOW the pane's invalidation marker under the store lock (glance's two-lock race could silently undo a concurrent invalidation → later `diff_since(R)` diffed stale-dimension frames instead of -32011). `<` not `≤` per LENS-R-033's "revision ≥ marker is found by rule (1)" (an ==marker clone is the post-mutation frame; refusing it would orphan the immediately-post-resize checkpoint). Honest no-op refusal (`checkpointed: false`), same channel as the teardown race. Unit: `checkpoint_store_refuses_pre_invalidation_revisions` (deterministic direct store path + -32011 decision assert).
- **Claude B — ungated resize invalidation:** the PTY resize branch invalidated on EVERY request; the attach loop's `apply_resize_to_window` re-fan (attach/client-resize/window-switch/zoom at unchanged client size) and same-size `pane.set_size` destroyed checkpoints on unchanged panes. Fix: dims captured across `vt.resize`, invalidate only on actual change (mirrors alt_switched gating). RED-FIRST proof (`.shux/out/lens-p4/red-receipt-gating.txt`: both repros -32011 pre-fix, incl. `invalidated_at == requested` on the same-size case): new `crates/shux/tests/checkpoint_invalidation_gating.rs` — same-size set_size survival + REAL attach client (AttachHello handshake, Action window switches, in-band Tab/`a` Input, render-frame drain) with exact 2-cell then 12-cell deltas WHILE attached (claude minor folded in). Resize-channel flush via synchronous same-size set_size ack makes both directions deterministic.
- **Codex M (adjudicated, PRD §7.3 amended):** -32011 keeps `{requested, invalidated_at, hint}` — wire shapes pinned at unit (shux-rpc `data_keys` exact-set) + black-box (`error_wire_shapes_pinned`) levels.
- Gates: `make test-lens` 27/10 (same red set) · gating tests 3/3 · checkpoint/diff-lookup unit 5/5 (incl. the new marker-refusal test) · `make test-rpc` 43/43 · `make lint` clean · full lanes green · leak-guard clean. NO push (convergence round 2 next).

**2026-07-10 — feat(lens): P4 `pane.checkpoint` + `pane.diff_since` — IMPLEMENTED, gate 27/10 (task 077)**
- Branch `feat/lens-p4-checkpoint-diff` from origin/main (d3b6282, includes P3 #90 + v0.40.0). §7 SPEC-D (LENS-R-030..038): `pane.checkpoint` + `pane.diff_since` RPC + `shux pane checkpoint`/`shux pane diff` CLI. Existence-first lookup (PANE_NOT_FOUND before checkpoint lookup); LENS-R-033 disambiguation — exact checkpoint → diff / `since ≤ last_invalidation` → RESIZE_INVALIDATED (-32011) / else STALE_REVISION (-32010) with `{requested, available}`. Resize + alt-screen (PRESENTED-flag compare) invalidation markers (LENS-R-032/DEC-4, monotonic, freed on teardown). Structured diff: value-equality of underlying cells, wide head+spacer pairing, merged half-open row spans (cap 256), half-open bbox, `changed_row_text` (glance byte-parity), `cursor_moved` separate. Heat PNG (LENS-R-037): P2 raster + integer overlay (red on changed / desaturate-50% unchanged) — shux-raster/shux-vt source UNCHANGED. New error codes StaleRevision/-32010, ResizeInvalidated/-32011.
- **Gate:** `make test-lens` **27 passed / 10 failed** (was 21/16) — D1–D5 + A1 green (CLI+RPC twins); 10 reds = R1–R8 (lens.run, P5) + K1 (missing P6 golden) + E1 (lens.run, P5), all untouched. `make test-rpc` 43/43 (+2) · `make test-vt-corpus` byte-exact · `make lint` clean · new `make test-lens-diff-concurrency` green · leak-guard clean, zero stray procs. Council D2 concurrency proven in-process (DirtyState drain between checkpoint+diff) + black-box (`diff_concurrent_readers.rs`). 5 new unit tests (lookup ordering, monotonic marker, wide pairing, dirty-independence, heat determinism).
- Goldens `d2_heat.png` (NEW heat rendering — full SOLID gate), `a1_alt.png`/`a1_normal.png` (raster-untouched glance) minted PROVISIONAL; evidence-manifest + BASELINE-APPROVAL P4 addendum. **Pending: SOLID VT QA (heat scope) PASS + convergence review + orchestrator golden sign-off before push.**

**2026-07-10 — fix(rpc): codex round 5 — degraded-mode decode error regains disconnect detection (task 077, gate 21/16 held)**
- Codex round-5 on `3861745..3adf935`: BOTH bot findings FIXED (half-close + byte cap), dup'd-fd lifetime/discrimination/accounting clean — NOT CONVERGED on one narrow NEW blocker, an interaction between the round-3 and bot-round changes in DEGRADED (monitor-less) mode only: with `have_monitor=false`, the InvalidData decode-error exit enqueued the Err, did not cancel, and left the read task with no drain — nothing could observe a later disconnect, so a valid long request + oversized frame + disconnect left the in-flight handler running to completion.
- **Fix (c3db30a):** the round-3 raw EOF drain restored for exactly `decode_error && !have_monitor` — deterministic error response kept (no cancel while connected), raw bytes discarded, `conn_closed` cancelled only on Ok(0)/Err from the raw read. Monitor-present path byte-identical. Tests spawn the module-private `serve_connection` directly with `monitor: None` (no public knob): `test_monitorless_decode_error_then_disconnect_drops_inflight` (the exact blocker scenario; times out against the un-fixed code) + `test_monitorless_decode_error_response_still_deterministic` (10×, client held connected — the drain does not reintroduce the round-3 race). shux-rpc lane **41/41**.
- Gates: full lanes green · `make test-lens` **21/16 unchanged** (S2 100/100, 79.4s) · lint clean · zero stray daemons/fixture procs. Pushed to PR #90.

**2026-07-10 — fix(rpc): PR #90 bot round — half-close-safe client-death detection + pending-bytes cap (task 077, gate 21/16 held)**
- Chain CONVERGED; PR #90 opened. Bot round found two real server.rs issues, fixed in one commit (`4b08312`, shared read-task accounting region):
- **codex-bot P2 (half-close regression):** read-side EOF was conflated with client death — a client doing `write → shutdown(Write) → read` had its queued frame discarded / in-flight dropped by the EOF-triggered cancel, never receiving its response. Fix: EOF no longer cancels anything (executor drains queue + in-flight and responds; connection ends naturally at `recv() → None`). Client DEATH now detected on the WRITE direction by a new `PeerDeathMonitor`: dup'd socket fd → `AsyncFd` (WRITABLE interest) resolving only on `is_write_closed()/is_error()` (EPOLLHUP Linux / kqueue EVFILT_WRITE EV_EOF macOS); plain-writable wakeups `clear_ready()` and re-park — edge-triggered, no busy-spin. `conn_closed` now fires only on: monitor (peer dead), IO-level read error, cap violation. Monitor-less fallback (dup failure, effectively never) degrades to cancel-on-EOF so B2 promptness is never lost. **Empirical macOS verification:** every existing full-close drop-guard test (disconnect-mid-request, backlog-disconnect, cross-connection, in-process lens waiter-drop, black-box CLI SIGKILL) passes THROUGH the monitor unmodified. New tests: half-close fast request (10×), half-close draining a backlog behind a gated in-flight request (both responses in order), full-close backlog-discard (dead client's `test.count` executes 0 times).
- **greptile P1 (unbounded bytes):** the 256-frame cap retains up to ~4 GiB (256×16 MiB). New `MAX_PENDING_BYTES = 64 MiB` in the same accounting (frame len added on queue, subtracted on dequeue; over-cap = same deliberate connection-cancel; worst case 64+16 MiB). Test: ~65×1 MiB frames behind a stalled handler sever the connection far below the frame cap.
- shux-rpc lane **39/39** (35 pre-existing unmodified + 4 new). Gates: full lanes green (settle proofs re-verified through the monitor) · `make test-lens` **21/16 unchanged** (S2 100/100, 80.2s) · vt 254/0 · corpus byte-exact · lint clean · zero stray daemons/fixture procs. Pushed to PR #90; replied to both bot threads (unresolved) — lean codex consult on this delta closes the loop before merge.

**2026-07-09 — fix(rpc): P3 review round 3 — deterministic frame-error responses (task 077, gate 21/16 held)**
- Codex round-3 on `029669d..faf826e`: the over-pipelined MAJOR is FIXED, 256 cap accepted — NOT CONVERGED on one NEW defect the round-2 fix introduced: **frame-error handling regressed into a race**. On a codec error the read task enqueued `Err(e)` then immediately cancelled `conn_closed`; the executor's outer select could take the already-ready close branch before dequeuing the Err, making the UDS `frame_too_large` response nondeterministic (sometimes the response, sometimes bare EOF).
- **Fix (9e21103):** codec/protocol errors and socket death no longer share the cancel path. The read task keys on the REAL error kind — LengthDelimitedCodec's oversized/invalid frame is `io::ErrorKind::InvalidData` (verified against tokio-util 0.7 source, PINNED by `codec::tests::test_codec_max_frame_size` so a tokio-util bump that changes the kind fails loudly). Decode error → enqueue the Err, do NOT cancel `conn_closed`: the executor dequeues it serially (after any in-flight request completes — original pre-split ordering), sends the response to the still-connected client, and closes; while the client is connected the executor's only ready branch is the queued Err, so determinism is BY STRUCTURE, not polling order. The read task then drains the RAW socket purely for EOF, so a client disconnecting in that window still gets prompt in-flight drop. EOF / IO read errors / cap violation cancel as before. Also fixed the `MAX_PENDING_FRAMES` doc wording per the round-3 caveat: the cap explicitly bounds the PRE-AUTH TCP window (up to 256 staged frames before the first-frame auth check severs), not an "already-trusted" caller.
- Tests: `test_frame_error_response_is_deterministic` (25× raw oversized frame → ALWAYS `frame_too_large` (-32001) then EOF, never bare EOF; no sleeps; the racy round-2 code fails with p ≈ 1 − 2⁻²⁵) + the codec-kind pin. shux-rpc lane **35/35**.
- Gates: `make test-rpc` 35/0 · `make test` full lanes green · `make test-lens` **21/16 unchanged** (S2 100/100, 78.9s) · `make test-vt` ok · `make test-vt-corpus` byte-exact · `make lint` clean · zero stray daemons/fixture procs.

**2026-07-09 — fix(rpc): P3 review round 2 — unbounded frame queue + pipelining cap (task 077, gate 21/16 held)**
- Round-2 verdicts on `ddebb43..029669d`: B1/M1/M2 FIXED by both reviewers, fire-and-forget delta ACCEPTED — but B2 PARTIALLY FIXED: both codex and claude independently found the **bounded(8) frame queue defeats EOF detection under pipelining** (long `wait_settled` + 9+ pipelined frames + disconnect → read task blocks on `frame_tx.send()` before it can read EOF → `conn_closed` never fires → the abandoned waiter leaks again through the over-pipelined path). Claude pre-reviewed the uncommitted fix mechanism and judged it correct ("convergence is one commit away").
- **Fix (3dff905):** frame queue now UNBOUNDED — the read task's forward is non-blocking by construction, so it is always parked on the SOCKET and EOF/read-error is reachable in ALL states, regardless of executor progress or pipelining depth. Memory re-bounded at the protocol level: `MAX_PENDING_FRAMES = 256` (inc by read task pre-send, dec by executor on dequeue; the inc-before-send window can only over-count, never evade). Exceeding the cap is a protocol violation that DELIBERATELY cancels the whole connection (warn log, in-flight handler dropped, client sees EOF while still connected). `conn_closed` also raced in the executor's OUTER select (both reviewers' requirement): a disconnect between requests breaks instead of draining a dead client's queue. Serial execution / response ordering / TCP auth-first / UDS frame-error semantics all unchanged.
- Tests: `test_pipelined_backlog_disconnect_still_drops_inflight_handler` (codex's exact scenario, Notify-signaled, no sleeps — deadlocks against the old bounded queue) + `test_pipelining_cap_exceeded_cancels_connection` (300-frame flood with the connection held open → handler dropped + server-side EOF; mid-flood broken-pipe tolerated, the severing IS the behavior under test). shux-rpc lane now 34/34.
- Gates: `make test-rpc` 34/0 · `make test` full lanes green · `make test-lens` **21/16 unchanged** (S2 100/100, 79.4s) · `make test-vt` 254/0 · `make test-vt-corpus` byte-exact · `make lint` clean · zero stray daemons/fixture procs. Settle proofs re-verified post-change (in-process waiter-drop, M1 not-found, M2 typing, black-box CLI-kill).

**2026-07-09 — fix(lens)/fix(rpc): P3 review round — codex 2B+2M / claude convergent (task 077, gate 21/16 held)**
- Verifier VERIFIED P3; `s1_ready.png` independently re-rendered byte-identical + visually inspected → **RATIFIED** (ddebb43, `provisional:false`). Review round: codex NOT CONVERGED (2 BLOCKERS + 2 MAJORS), claude NOT CONVERGED (independently converged on the same architectural finding + a TOCTOU guard). All four fixed on-branch, one commit each:
- **B1 (bf975bc) — deadline precedence + TOCTOU guard:** every wake of the settle loop re-evaluates quiet FIRST; timeout fires only when quiet is still false at the deadline (with `timeout_ms == quiet_ms` allowed, quiet-at-the-shared-deadline now settles). New pure `settle_decide()` (pending > quiet > timeout > wait) + `rx.has_changed()` guard: a revision published after the `borrow_and_update` snapshot restarts evaluation instead of settling stale. Unit tests: equal-deadlines settles, late-wake-past-timeout-with-quiet settles, revision-in-return-window does NOT settle (drives a real watch channel), full priority table.
- **B2 (04032a2) — ARCHITECTURAL, shux-rpc cancellable request execution:** server connection tasks awaited `process_frame` inline, so disconnects went unseen until the handler returned — an abandoned `wait_settled` lived until settle or the 600s cap (LENS-R-023 violation). `serve_connection()` now splits the stream: a read task keeps decoding into a bounded queue and cancels a per-connection token on EOF; the executor drains STRICTLY SERIALLY (ordering preserved) and races each dispatch against the token — on disconnect the in-flight handler future is DROPPED. TCP auth-first + UDS frame-error semantics preserved. shux-rpc tests ×3 (drop-guard abort proof; serial/ordered pipelining; cross-connection isolation), all 32 green. Lens proof, two halves: `production_settle_waiter_dropped_on_client_disconnect` (PRODUCTION router behind a REAL UDS server; observable = pane revision-watch `receiver_count` — 0→1 on subscribe, 1→0 promptly after socket drop; pre-fix fails at 60s park) + black-box `crates/shux/tests/settle_waiter_drop.rs` (SIGKILL a real `shux pane wait-settled` CLI mid-wait → daemon healthy, fresh waiter settles; runs in the default `make test` lane). New `make test-rpc` target.
- **M1 (7ee229a) — pane killed mid-wait → NOT_FOUND:** sender-dropped no longer settles on the frozen value; both channel-closed observation points re-check via `settle_reacquire_watch()` → publisher gone → -32004 (matching the entry-time missing-VT error). Test parks a waiter, kills via the real `teardown_panes(remove_vts)` path, asserts prompt -32004.
- **M2 (1a7981b) — strict param typing:** present-but-mistyped `quiet_ms`/`timeout_ms` (`"5ms"`, `5.5`, `null`, `-5`) → INVALID_PARAMS instead of silently defaulting; absent still defaults. Raw-RPC tests on both params + helper unit table.
- Gates: `make test-lens` **21/16 unchanged** (S2 100/100 byte-identical, 79.4s; all 16 reds still `-32601` P4/P5 roots) · `make test-rpc` 32/0 · `make test-vt` 254/0 · `make test-vt-corpus` byte-exact · `make test` full lanes green (incl. new black-box waiter-drop test) · `make lint` clean · zero stray daemons/fixture procs.

**2026-07-09 — feat(lens): P3 `pane.wait_settled` (task 077, P3 implemented — gate 21/16, S1–S5 + V1 green)**
- **Implemented `pane.wait_settled` RPC + `shux pane wait-settled` CLI (1:1), §6 SPEC-C / LENS-R-020..025.** Event-driven off the P1 per-pane `watch::Sender<PaneRevision>` — no polling, no output-synchronizing sleeps. The waiter `subscribe()`s under the io lock, drops the lock before any await (never holds the mutex across `.await`), then loops: read `(content_revision, last_mutation_ns)`, compute `settle_is_quiet(now, last, quiet)` = `now_ns − last_mutation_ns ≥ quiet_ms × 1_000_000`, and `select!` between `rx.changed()` (a genuine Class-A batch resets the window) and `sleep_until(min(quiet_deadline, timeout_deadline))`. Already-quiet panes return immediately (LENS-R-020; the watch makes S4's two-call pattern race-free). Timeout is a server-side monotonic deadline from request acceptance → `{settled:false}` as a RESULT, not an error (DEC-19). Waiter lifetime is bound to the future, so a client disconnect drops it (LENS-R-023). Class-B (titles/bells/DECSCUSR/OSC 4) never publishes a revision change, so it can't reset quiet — S5 comes free from watching only the published pair (LENS-R-024).
- **Same-clock invariant (councils-caught ns/ms bug class):** exposed `shux_vt::monotonic_now_ns()` (was private) so settle reads "now" on the SAME process-monotonic `START` epoch that stamps `last_mutation_ns`. Unit conversion is explicit `× 1_000_000` on both sides in ns.
- **Param validation (LENS-R-025):** `quiet_ms ∈ [10, 60_000]`, `timeout_ms ∈ [quiet_ms, 600_000]` → INVALID_PARAMS (-32602) → CLI exit 2. CLI `--quiet`/`--timeout` accept human durations (`300ms`, `2s`) via a new `parse_duration_ms` (bad string → clap usage exit 2). Exit table (§10): 0 settled / 1 timeout / 2 usage / 3 other+PANE_NOT_FOUND / 4 perm. `--format json` emits the `{result|error}` envelope byte-identical to `shux rpc call` (manually verified parity).
- **Golden `s1_ready.png` minted PROVISIONAL** (F2's post-settle READY frame; S1/S2 anchor). P3 changes NO rendering code, so it is the P2-approved raster + harness fixture-font chain on the frozen F2 fixture; byte-identical across 3 standalone runs AND the S2 100× gate (100/100). Visually inspected (LENS-F2-SPIN, amber spinner, green READY, full truecolor/256/basic legend — no monochrome regression). Recorded in `evidence-manifest.json` (`goldens.s1_ready`, `provisional:true`) + `BASELINE-APPROVAL.md` P3 addendum; **awaits the downstream verifier/QA/DootSabha ratification** (P3 has no SOLID VT QA gate in PRD §14). No frozen path (`crates/shux/tests/lens_*`, `.shux/fixtures/lens/**`) touched.
- **7 L0 unit tests** (settle math ns-conversion + boundary, already-quiet immediate, remaining-quiet shrink/zero, backwards-clock saturation, param bounds accept/reject, waiter subscribe/drop is bounded).
- Gates: `make test-lens` **21 passed / 16 failed** (S1–S5, V1 flipped green; the 16 reds UNCHANGED, all `-32601` on P4/P5 `pane.checkpoint`/`pane.diff_since`/`lens.run`; R8 wants -32014). S2 flake gate 100/100 byte-identical (~79s). `make test-vt-corpus` byte-exact (default raster chain untouched). `make test` all lanes green (192 shux-bin incl. the 7 settle unit tests). `make lint` clean. Anchored leak check clean (zero orphan shux daemons / fixture procs). Serial, leak-guarded throughout.

**2026-07-09 — fix(lens)/test(lens): PR #89 bot round + user-ordered golden re-mint with real fixture fonts (task 077)**
- **Bot fixes (PR #89):** (P1) glance now enforces the 16M-pixel raster budget BEFORE any clone/render/encode — mapped to `PAYLOAD_TOO_LARGE (-32013)`; a 1000×1000 pane previously forced hundreds of MB of RGBA allocation before the post-encode cap could fire; pinned by `production_glance_rejects_over_budget_panes_before_render` (guard fires; text-only glance on the same pane still succeeds). (P2) `glance_text` doc comment rewritten — blank cells PAD rows to full width, nothing trims. (P2) `--text-only`+`--png` now rejected by clap `conflicts_with` at parse time (exit 2, no RPC) + a defensive handler guard. (P2) clone routing is now a move-matrix: only PNG+checkpoint pays a clone; text-only+checkpoint MOVES the grid.
- **Golden re-mint (user adjudication, PRD §17):** committed OFL fixture fonts at `.shux/fixtures/fonts/` — full `NotoSansDevanagari-Regular.ttf` (notofonts.github.io hinted static) + `NotoSansJP-shuxlens-subset.ttf` (pyftsubset of google/fonts NotoSansJP wght=400 to EXACTLY the 9 fixture CJK codepoints, ~4 KB; reproduction commands in the README). Lens harness (`lens_common::Harness::new`, LENS-TEST-CHANGE p2-fonts) writes an isolated-config `appearance.font_fallbacks` chain appending both fonts after `builtin:nerd-font`; bundled primary unchanged → cell metrics identical; DEFAULT raster chain untouched.
- Re-minted `g2_f1_80x24.png`/`g2w_f5_100x30.png` (+ contact sheet): Devanagari + CJK tofu GONE — real glyphs, rendered per-codepoint (no OpenType shaping; conjuncts/matras decomposed — KNOWN + ACCEPTABLE per the adjudication, stated in BASELINE-APPROVAL). Text goldens unchanged (font-independent). `evidence-manifest.json` regenerated: new PNG sha256s, fixture-font hashes + provenance URLs. BASELINE-APPROVAL → "RE-MINTED — pending QA re-inspection", prior approval preserved as history.
- Gates: `make test-lens` 15/22 (G1/G2/G2w green vs NEW goldens; all 10 fixture smoke tests green under the font config) · `make test-vt-corpus` byte-exact (default chain untouched, verified) · full lanes 18/18 ok · lint clean · shellcheck clean · leak-guard clean.

**2026-07-09 — fix(lens)/docs(lens): P2 ship round — claude minors + baseline approval + council evidence (task 077)**
- Claude full review CONVERGED (0 new blockers/majors, 3 minors) — chain complete: verifier ✓, codex round fixed ✓, claude converged ✓, SOLID VT QA PASS ✓ (`.shux/qa/lens-p2/SOLID-QA.md`, VERDICT: PASS, commit 1a578b4).
- **Minor (a), REAL — sync-enter color lag:** a color change in the SAME batch that opens `?2026h` is frozen INTO the presentation (presented frame changed) but the bump was routed to the deferred flag → revision lagged the visible pixels for the whole sync window. Fix: the presented-colors compare (sync-aware on both sides) bumps IMMEDIATELY even when sync is active at batch end — it can only fire then if the presentation itself changed this batch; other Class-A signals keep the defer path. Test: `osc_color_set_then_sync_enter_same_batch_bumps_immediately` (+1 at enter, release adds nothing). shux-vt lane 254/0.
- **Minors (b)+(c) — checkpoint FIFO by CREATION REVISION:** `store_checkpoint` now inserts sorted by revision so eviction (front) always takes the LOWEST creation revision even when two racing glances reach their second lock windows out of order (LENS-R-031; pure insertion-order FIFO would evict the newer frame). Test: `checkpoint_fifo_evicts_lowest_creation_revision` (ascending 5th-store evicts rev 1; out-of-order arrival [10,5,20,30]+40 evicts 5 not 10; deque stays revision-ascending). shux bin lane 184/0.
- **Baselines APPROVED:** BASELINE-APPROVAL.md flipped to APPROVED citing the QA PASS + orchestrator sign-off under the user's standing ship authorization; `evidence-manifest.json` `provisional: false`; golden bytes verified unchanged (sha256 re-check against the manifest).
- **Council evidence committed on-branch:** `.shux/qa/lens-p2/council/{lens-p2-codex.json,lens-p2-claude-full2.json}`.
- Gates: `make test-lens` 15/22 (zero golden diff) · shux-vt 254/0 · vt-corpus byte-exact · lint clean · leak check clean. Pushed; PR opened (do-not-merge pending bot triage).

**2026-07-09 — fix(lens): P2 codex review round — presented-frame consistency fixes (task 077, gate 15/22 held)**
- Codex review (NOT CONVERGED: 1B+2M+1m, all presented-frame-doctrine descendants) after the verifier passed P2 (VERIFIED-WITH-NOTES, goldens byte-matched a live drive). Applied:
- **BLOCKER, torn alt_screen under sync:** `SyncPresentation` gains `alternate_screen` (captured at `?2026h` freeze); `is_alternate_screen()` is now presented-aware like `grid()`/`cursor()`/`default_colors()` — an alt toggle inside sync can no longer pair old pixels with a future flag in glance. Live state via `modes()`. Test: `sync_alt_toggle_glance_consistency`.
- **MAJOR, OSC net-zero false bump under sync:** the Class-A color compare now uses PRESENTED colors on both sides — hidden set-then-restore inside sync nets to no bump at release; a real net change bumps exactly once (release compares frozen-vs-live). Test: `osc_color_net_zero_under_sync_no_bump` (+ control).
- **MAJOR, checkpoint resurrection:** `store_checkpoint` refuses panes with no live VT (the glance store runs under a SECOND lock; teardown can interleave) — returns `(stored, evicted)`, handler reports `checkpointed: false` honestly. Test: `checkpoint_store_refuses_resurrection_after_teardown`.
- **MINOR, CLI json envelope — DISPUTED with evidence:** bare-result emission per codex's §10 reading breaks the FROZEN harness (`cli_envelope` parses `{result|error}`; verified: G2 CLI twin panics at lens_common/mod.rs:59, gate would drop 15/22 → 12/25). Envelope kept (byte-parity with `shux rpc call`, M9); dispute documented at the emission site; escalated to the claude convergence round.
- Gates: `make test-lens` 15/22 (identical fail set; goldens byte-stable) · shux-vt 253/0 · shux bin 183/0 · vt-corpus byte-exact · full lanes 0 failed · lint clean · leak-guard clean.

**2026-07-09 — fix(lens): P2 adjudication round — F3 sync-wrap + OSC 10/11/12 → Class A (task 077, P2 gate now 15/22)**
- All three P2 findings adjudicated by the orchestrator (PRD §4.2 OSC row, §11 F3 row, §17 font-risk row updated). Applied:
- **F3 sync-wrap (approved LENS-TEST-CHANGE):** `f3_flip.sh` wraps each `draw_frame` in DEC 2026 synchronized output — the 24 row writes present as ONE atomic Class-A batch at `?2026l` release. F3 smoke test extended with the sync contract: one token → exactly one revision step. **G1 green, 3/3 consecutive runs**; `make test-lens` now exactly **15 passed / 22 failed**, all remaining roots unchanged (`-32601` on P3/P4/P5 methods). Fixture shellcheck-clean.
- **OSC 10/11/12 re-adjudicated to Class A** (revision tracks the PRESENTED frame — the P2 evidence made the P1 Class-B ruling untenable): `process_with_responses` now includes a before/after `default_colors` compare in the Class-A disjunction. Parser change-guards make same-value sets net-zero (no bump); sync-deferral respected (color change under `?2026h` → one bump at release). Sets AND resets (110/111/112) covered both directions; reset-with-nothing-set is a no-op. Tests renamed/added: `osc_10_11_12_bumps`, `osc_110_111_112_bumps_when_set`, `osc_dynamic_color_defers_under_sync` (shux-vt lane 251/0). OSC 4 remains Class B (documented known limitation). Glance-handler comment updated to the new ruling.
- **Goldens NOT regenerated** (approved as-is pending QA gate): verified F1/F5 emit zero OSC 10/11/12 (SGR only), so the reclassification cannot alter their rendering.
- Gates: `make test-lens` 15/22 · shux-vt unit lane 251/0 · `make test-vt-corpus` byte-exact · `make test` all lanes 0 failed · `make lint` clean · all daemon-backed runs under `no_leak_guard.sh` (anchored count_fixture_procs matching), zero leaks.

**2026-07-09 — feat(lens): P2 `pane.glance` (task 077, P2 implemented — G1 blocked on a spec/fixture decision)**
- Built §5 SPEC-B: `pane.glance` RPC (LENS-R-010..016) + `shux pane glance` CLI, mirroring `pane.snapshot`'s existing atomic-clone-under-lock pattern. One `PaneIoState` lock acquisition clones grid (visible-only)/cursor/size/alt_screen/dynamic-default-colors/`content_revision`; render (via unchanged `shux-raster`) and text extraction happen from that frozen clone outside the lock, guaranteeing PNG and text agree on the same frame.
- New `Grid::glance_text()` (`crates/shux-vt/src/grid.rs`): full-width, untrimmed, `\n`-joined viewport rows — deliberately NOT `capture_text()`'s "drop trailing blank rows + trim_end per row" UX contract, since LENS-R-012 wants byte-stable fixed-shape text.
- New `ErrorCode::PayloadTooLarge` (-32013, `crates/shux-rpc/src/error.rs`) for the §5.2 8 MiB decoded-PNG cap.
- Checkpoint storage (§7 LENS-R-030/031, P2-scoped only): `PaneIoState.checkpoints: HashMap<PaneId, VecDeque<PaneCheckpoint>>`, FIFO cap 4, unique-per-revision no-op, `evicted_revision` on eviction. `pane.checkpoint`/`pane.diff_since` (P4) intentionally NOT added.
- Determinism micro-test (`crates/shux-raster/src/lib.rs::glance_clone_renders_byte_identical_twice`, NOT a frozen `lens_*` file): same clone rendered twice → byte-identical raw RGBA and PNG-encoded bytes.
- Minted PROVISIONAL goldens `.shux/goldens/lens/{g2_f1_80x24,g2w_f5_100x30}.{png,txt}` + `evidence-manifest.json` + `contact-sheet.png` + `BASELINE-APPROVAL.md` (§16.3 — approval still pending; committed goldens are honestly marked not-yet-approved).
- Gate: `make test-lens` → 14 passed / 23 failed (target in the phase brief was 15/22 — see G1 finding). G2/G2w green (CLI+RPC incl. `--png` file-write). All other roots unchanged (`-32601` on still-missing P3/P4/P5 methods). `make test-vt-corpus` byte-exact. `make test` all workspace lanes green. `make lint` clean. Every daemon-backed run under `no_leak_guard.sh`, zero leaks.
- **G1 (`g1_glance_atomicity_under_concurrent_flips`) does NOT pass** — root-caused (not a `pane.glance` bug): F3 (`f3_flip.sh`) draws each flip as 24 unwrapped `printf` writes (no DEC 2026 sync-mode `?2026h`/`?2026l`); under G1's 100-way concurrent load a PTY batch can land mid-repaint, and that batch still gets one Class-A revision bump per §4.2 (revision has no "clean frame" concept). Proved via manual repro: 3 independent glances landing on the same revision returned byte-identical text+PNG (glance's own atomicity holds); the underlying VT content at that revision is itself a genuine A/B mix. `dootsabha council` (`agent_review_guard.sh lens-p2-g1-dispute`) independently confirmed this and recommends a `LENS-TEST-CHANGE` to wrap `f3_flip.sh`'s `draw_frame` in sync-mode (shux-vt's P1-shipped sync support already gives exactly the atomic-batch semantics G1 wants) — NOT a `pane.glance` retry/drain workaround. Left for explicit approval per §16.4; not applied here. Full writeup: `docs/tasks/077-shux-lens.md` P2 notes.
- OSC 10/11/12 finding (§4.2's mandated P2 re-examination): confirmed live (not hypothetical) — `pane.glance` feeds `vt.default_colors()` into `RasterOptions` exactly like `pane.snapshot` already does, so an OSC-10/11/12-only repaint (Class B, no revision bump) DOES change glance's rendered pixels. Not redesigned in P2 per the phase brief; documented for adjudication.

**2026-07-08 — fix(lens): P1 council round 1 — 1 blocker + 4 majors fixed (task 077, P1 In Progress)**
- Blocker: `mark_all_dirty()` was wired into the content tally, so Class-B OSC 10/11/12 (+110/111/112 resets, OSC 4 palette) dynamic-color events bumped ContentRevision. Decoupled: mark_all_dirty is render invalidation only; RIS still bumps via `clear_visible`, alt swaps via the alt-flag compare. Tests: osc_10_11_12_no_bump, osc_110_111_112_no_bump, osc_4_palette_no_bump, ris_full_reset_still_bumps.
- Majors: `monotonic_now_ns()` clamped to ≥1 (LENS-R-002 "never 0" holds even for the epoch-initializing VT); `append_zero_width_scalar` now peeks immutably and takes the tally-bumping row only on committed writes (combining_mark_on_blank_no_bump + on_glyph_bumps); `session.snapshot` never emits `content_revision: 0` — VT-less graph panes are debug_assert + skip-with-log, matching snapshot_window's established VT-less handling; session.snapshot metadata and rendered window now come from ONE `gh.snapshot()` (snapshot_window takes `&SessionGraphSnapshot`), eliminating the torn-metadata race.
- Documented semantics (council addendum): alt-screen enter+leave inside one `process()` batch nets to zero → no bump (batch boundaries compare end states); pinned by alt_screen_double_toggle_one_batch_no_bump.
- L0 lane now 28 content-revision tests; default lanes 1102 pass / 0 fail across 18 test binaries.

**2026-07-08 — feat(lens): P1 ContentRevision substrate (task 077, P1 In Progress)**
- Built the §4 SPEC-A substrate the council proved missing. `VirtualTerminal` now owns `content_revision: u64` (starts at 1, +1 per Class-A mutation BATCH — one per `process()`/`resize()` producing ≥1 Class-A event) and `last_mutation_ns: u64` (monotonic, seeded at pane creation).
- Class-A detection is value-INDEPENDENT (identical repaints still bump — §4.2 "MUST NOT diff to decide"): a new monotonic `Grid::mutations()` write tally, bumped in every cell/scroll/clear/erase/insert/delete write, is compared before/after each `process()` batch alongside cursor pos/visibility and the alt-screen flag. Deliberately NOT `DirtyState` (never drained/coalesced → no lost-edge race vs an attached render client; LENS-R-004/§4.4). Zero render/compositor behavior change.
- LENS-R-003: per-pane `tokio::sync::watch` publisher in `PaneIoState.revisions`, published in the same PTY-task critical section as the grid mutation, once per Class-A batch (`send_if_modified`). Same lifetime as the VT.
- LENS-R-006: `session.snapshot` result gains top-level `session_version` (session structural version) and `panes: [{pane_id, version, content_revision}]`. Only public exposure of the counter until `pane.glance` (P2).
- Gate: `make test-lens` → G3 + G4 GREEN via `session.snapshot`; 12 pass / 25 red, every red rooted in `-32601` (missing pane.glance/wait_settled/checkpoint/lens.run) or missing CLI verb — unchanged roots. L0: 20 shux-vt unit tests map every §4.2 table row. `make test-vt-corpus` byte-exact + `make test` (1102 pass / 0 fail across 18 test binaries; earlier "533" was a miscount from a truncated log scan) untouched; clippy `-D warnings` clean; leak-guard clean, zero orphans.

**2026-07-05 — test(lens): P0 council round-3 micro-fixes (task 077, In Progress)**
- Fixed the count_procs argv false-match found under parallel load (a co-tenant review agent's prompt contained fixture filenames and the substring match counted it, flaking the F2/F7 EOF-exit proofs): fixture spawns now exec the absolute repo-root-anchored path and `count_fixture_procs` counts only processes whose argv BEGINS with `sh <abs>/.shux/fixtures/lens/<script>`.
- Made F4's empty-read-as-EOF handling an explicit normative input contract (a/s/Tab only; bare LF and NUL — which also read back empty through command substitution — are never sent), documented in the fixture header and the smoke test.

**2026-07-05 — test(lens): P0 council round-2 hardening (task 077, In Progress)**
- Applied the P0 phase-diff council round-2 verdict (3 majors — PRD §A1): fixed the EOF busy-spin the PRD itself had prescribed (`while :; do read || :; done` spins at 100% CPU on EOF) — F1/F2/F5 blockers now drain via `cat >/dev/null`, F7 uses the POSIX signal-safe `while read -r _ || [ $? -gt 128 ]; do :; done` (SIGWINCH continues, EOF exits), F4's dd loop breaks on empty read; F2/F7 smoke tests extended to prove WINCH survival and EOF-exit with zero residual processes.
- G1's pump now loops on a shared done-flag stored after all glance threads join (must outlive the slowest glance), with a 10k-token cap + 120s deadline purely as panic bounds; glance joins are collected non-panicking so the flag is always stored.
- R8's CLI spawn-failure twin now repeats the RPC twin's daemon-state assertions (zero residual scratch entries + system health).

**2026-07-05 — test(lens): P0 council round-1 hardening (task 077, In Progress)**
- Applied the P0 phase-diff council verdict (1 blocker, 9 majors, 4 minors — PRD §A1): S3 pump-lifetime race fixed with per-check pump scopes; harness global NO_COLOR removed (T-tier color cases now assert non-grayscale, no-color cases inject per-test); CLI/RPC parity twins completed across G1/G2/G2w/D1/D2/D3/R1/R3/R5 incl. successful-path `pane diff` and `--png`/`--heat` file surfaces; D2 asserts byte-exact FULL-WIDTH rows; G4 checks session AND pane structural versions; three NEW red tests D5 (checkpoint FIFO eviction + same-revision no-op), V1 (settle param validation), R8 (spawn failure rollback + size bounds) — synthetic count 24→27.
- check-lens-frozen.sh hardened: `git interpret-trailers --parse` with non-empty reason required, HEAD-itself shallow fallback, merge commits diffed against first parent (never skipped).
- Hardening exposed a real fixture bug: PTY echo of token newlines scrolled/corrupted token-paced frames. All token-paced fixtures (F2/F3/F8/F9/F10) now set `stty -echo` like F4; F2 drains stdin silently post-READY and its smoke test proves frame stillness; F3 probes validated against exact raster palette RGB values.

**2026-07-05 — feat(lens): P0 red suite + fixtures (task 077, In Progress)**
- Started task 077 (shux lens). Phase P0 delivers ONLY fixtures + the complete RED test suite — zero feature/daemon/RPC code.
- Fixtures `.shux/fixtures/lens/f1..f10` (§11): POSIX sh + printf, token-handshake paced (no sleeps), shellcheck-clean, each carrying truecolor + 256-color + basic-color content. Applied convergence deltas: F4 `s`-before-`a` no-op, F7 SIGWINCH-proof `while :; do read -r _ || :; done`.
- 10 fixture smoke tests (`lens_fixtures_smoke.rs`) GREEN via existing machinery (session/pane CRUD, set_size, send-keys, capture, snapshot, pixel probes) — proving each fixture's contract, incl. F7 live-resize WINCH reprint.
- 24-test red suite `crates/shux/tests/lens_*.rs` (G1,G2,G2w,G3,G4 · S1–S5 · D1–D4 · A1 · R1–R7 · K1 · E1) + RPC twins where marked ⇄. Black-box: drives only `shux rpc call` / CLI. Every lens test fails rooted in `-32601` (missing method) or a missing snapshot field — the red receipt. Pre-P5 tests use ordinary sessions; frozen local helpers under the test paths.
- T-tier scaffolding (§13): `t/make_nidhi_repo.sh` (pinned 2020-01-01 dates, exactly 3 Devanagari/CJK/emoji stashes), `t/demo-app/` (standalone excluded ratatui crate with a seeded border break at col 80), tests T1–T4 with loud skip when `nidhi`/`vivecaka` absent.
- Test integrity: `scripts/check-lens-frozen.sh` (§16.2 `LENS-TEST-CHANGE:` trailer guard) wired into lefthook `commit-msg` + `make check-lens-frozen` (in `check`). Red suite lives in a `test = false` Cargo lane so `make test`/`make check` stay clean while the suite is red; `make test-lens` / `test-lens-t` run it explicitly, serially, under the leak guard.

**2026-06-29 — feat(plugin): add Sightline TUI QA plugin**
- Completed task 076 with first-party local package `plugins/sightline/`; `bin/sightline` is the direct v1 product and `shux plugin install plugins/sightline` is explicit lifecycle smoke via `--plugin-host`.
- Added deterministic Sightline checks for pane capture, PNG validity/dimensions/grid dimensions, nonblank pixels, truecolor/indexed/basic SGR emission and rendered color samples, keyboard delta probes, structured Markdown/JSON reports, and scratch evidence under `.shux/out/sightline/`.
- Added `make test-sightline`, `.shux/scripts/sightline_check.sh`, and a package resolver test for the committed Sightline manifest.
- Updated general TUI QA agent instructions so routine screenshots stay scratch/PR-comment evidence while committed `.shux/qa` manifests remain explicit durable exceptions with strict manifests.
- Updated README/agent/skill discovery guidance so agents can find Sightline for TUI QA without bloating the base skill; added a no-clone helper that caches the minimal package under the user cache instead of duplicating it per repo.
- Dogfooded with real shux automation, `nvim`, and the `Laghudarshi` cold-context Textual TUI gauntlet; DootSabha design/implementation/fix reviews and independent `shux-tui-qa` all passed.

**2026-06-29 — feat(plugin): improve plugin DX foundation**
- Completed task 075 with a local-only plugin DX first pass: `shux plugin scaffold/create/init --runtime sh`, `shux plugin stop`, manifest-directory install validation, canonicalized entrypoints, default package-root cwd, and package name/version handshake checks.
- Extracted plugin command routing into `features::plugin::dispatch`, reducing central `main.rs` churn for future plugin subcommands while preserving existing daemon-backed permission, grant, audit, runtime UUID, and hot-reload paths.
- Added focused package/scaffold/router/CLI coverage plus `make test-plugin-dx`; preserved direct executable and legacy `plugin.sh` directory install compatibility.
- Dogfooded with real shux automation under `.shux/scripts/no_leak_guard.sh`; kept screenshot evidence out of the PR because this task changes plugin CLI/package behavior, not terminal rendering.
- External review: DootSabha implementation review found canonicalization, symlink-escape, identity-binding, TUI-QA, doc-scope, scaffold README/license, and route-boundary gaps; fixes were incorporated before PR.

**2026-06-13 — chore(qa): add general TUI QA gate**
- Added reusable Claude/Codex `shux-tui-qa` subagents for user-visible
  terminal/TUI work outside the stricter VT-specific gate.
- Wired the gate into `CLAUDE.md` with hard requirements for task DoD
  enforcement, real colored shux automation, native screenshot inspection,
  pixel-level verification, cleanup proof, and scoped committed evidence under
  `.shux/qa/<scope>/`.
- Added `scripts/check-tui-qa.sh` and `make check-tui-qa` to validate
  `TUI-QA.md` plus `tui-evidence-manifest.json`, tracked PNG/capture/pixel
  artifacts, `pixel_verify.py`-shaped metric JSON, cleanup status, and required
  scoped evidence via `TUI_QA_REQUIRED=1 TUI_QA_SCOPE=<scope>`.
- External review: DootSabha Claude plus `agy` review identified gaps around
  durable evidence, scoped enforcement, placeholder artifacts, and cleanup
  examples; the gate/checker were tightened accordingly.

**2026-06-13 — feat(vt): add dirty-region tracking and process leak guardrails**
- Completed task 074 by adding opt-in dirty-region tracking to `shux-vt` grid
  mutation paths, exposing `is_dirty` / `take_dirty_regions`, and preserving
  clean cloned snapshots for existing rendering paths.
- Added focused coverage for print, erase, insert/delete, scroll, resize,
  alternate-screen/sync transitions, direct row mutation guards, clone behavior,
  wide-cell repair ranges, and VT byte-fixture dirty sequences.
- Added `make test-vt-dirty-regions` with independent tracking-disabled versus
  tracking-enabled raster parity, 0-diff pixel verification, replay/idle
  performance budgets, and live 80x24 / 120x40 / 200x60 shux screenshots with
  explicit truecolor, indexed-color, and basic-color probes.
- Hardened process hygiene after finding orphan daemon/reviewer processes:
  `.shux/scripts/no_leak_guard.sh` now gates daemon-backed shux automation,
  `.shux/scripts/lib/shux_harness.sh` provides timeout fallback and isolated
  runtime cleanup, and `.shux/scripts/agent_review_guard.sh` bounds external
  reviewer CLIs that may spawn MCP children.
- Verification: focused dirty-region unit tests, `make test-vt-dirty-regions`,
  `make test-shux-leak-guard`, `make test-agent-review-guard`, exact pixel
  JSON, color-pixel report, performance JSON, and no remaining task-owned
  `shux`/Gemini/DootSabha processes after cleanup.

**2026-06-13 — feat(vt): correct origin-mode scroll-region semantics**
- Completed task 072 by making DECOM origin mode address CUP/HVP/VPA relative
  to the active scroll region, clamp to the region bottom, and report CPR/DSR
  rows relative to the origin while keeping columns absolute.
- Corrected DECSET/DECRST `?6`, valid DECSTBM homing, invalid scroll-region
  no-op behavior, DECSC/DECRC origin-mode restore, and CUU/CUD/CNL/CPL/VPR
  margin clamping only when movement starts inside the active region.
- Added focused `shux-vt` unit coverage, a synthetic corpus fixture with
  origin-relative response assertions, and a real shux automation target that
  captures 80x24, 120x40, and 200x60 scroll-region layouts.
- Hardened pane I/O probes with shell readiness markers and CRLF command
  submission, and raised the local test-binary timeout to avoid false failures
  on slow raster and pane I/O binaries under full-suite load.
- Verification: DootSabha design and implementation review with
  `--agents claude,gemini`, `make test-vt`, `make test-vt-origin-mode`,
  `make test-vt-corpus`, `make test-pane-io`, full `make check`,
  full-resolution visual inspection, exact zero-diff pixel JSON at all three
  sizes, and SOLID VT QA PASS.
- Post-review fix (Codex PR finding): relative vertical moves starting outside
  the scroll region are now direction-aware — `move_cursor_up` clamps to the top
  margin only when the cursor is at/below it (else row 0), and `move_cursor_down`
  clamps to the bottom margin only when at/above it (else last row), matching
  xterm `CursorUp`/`CursorDown`. Replaced `relative_vertical_bounds` with
  `upward_vertical_top`/`downward_vertical_bottom` and extended the regression to
  cover above/down and below/up margin clamping. Focused `make test-vt`
  filters and `make test-vt-origin-mode` pass after the fix.

**2026-06-12 — feat(vt): track mutable tab stops**
- Completed task 071 by replacing hardcoded 8-column tab movement with
  `VirtualTerminal`-owned bitmap tab-stop state used by HT, HTS, TBC, CHT, and
  CBT.
- Preserved default stops on local HTS/TBC mutations, excluded column 0 from
  defaults, reset stops on RIS only, kept DECSTR and alternate-screen switches
  from resetting tabs, and handled resize growth/shrink with explicit
  clear-all behavior.
- Added unit coverage for default tabs, custom HTS, TBC current/all, CHT/CBT
  counts, resize growth and shrink, RIS, DECSTR, and alternate-screen
  preservation.
- Added real PTY `pane.capture` integration coverage plus
  `make test-vt-tab-stops`, which snapshots 80x24, 120x40, and return-to-80x24
  tab-alignment fixtures with exact pixel comparison against committed
  `.shux/goldens/071-tab-stops/` baselines.
- Extended the VT corpus with a synthetic mutable tab-stop fixture and refreshed
  the task-073 corpus evidence.
- Verification: DootSabha design and implementation review with
  `--agents claude,gemini`, `make test-vt FILTER=tab`,
  `SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io FILTER=tab_stops`,
  `make test-vt-tab-stops`, `make test-vt-corpus`,
  `make test-vt-wide-invariants`, full-resolution visual inspection, exact
  pixel JSON, and SOLID VT QA PASS.

**2026-06-12 — feat(vt): render DEC special graphics**
- Completed task 070 by adding G0/G1 charset state, DEC special graphics
  designation (`ESC ( 0` / `ESC ) 0`), ASCII redesignation, SO/SI shifts, and
  DECSC/DECRC charset save/restore to `shux-vt`.
- Mapped the standard DEC special graphics set to Unicode box drawing and
  symbols, while unsupported G0/G1 designations safely fall back to ASCII.
- Added unit coverage for full-map translation, cross-chunk state, dynamic
  redesignation, invalid designations, REP, RIS, alternate-screen save/restore,
  and wide Unicode written while DEC graphics is active.
- Added a real PTY `pane.capture` integration test plus
  `make test-vt-dec-special-graphics`, which snapshots 80x24, 120x40, and
  200x60 stress screens with exact pixel comparison against committed
  `.shux/goldens/070-dec-special-graphics/` baselines.
- Verification: DootSabha design and implementation review with
  `--agents claude,gemini`, `make test-vt FILTER=dec_special_graphics`,
  `SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io FILTER=dec_special_graphics`,
  `make test-vt-dec-special-graphics`, `make test-vt-corpus`,
  `make test-vt-wide-invariants`, `SHUX_TEST_BINARY_TIMEOUT_SECONDS=180 make check`,
  full-resolution visual inspection, exact pixel JSON, and SOLID VT QA PASS.

**2026-06-12 — feat(vt): preserve grapheme cell payloads**
- Completed task 069 by adding rare grapheme payload storage to `Cell` extended
  attrs while preserving the compact ASCII path and keeping `Cell` at 24 bytes.
- Preserved combining marks, variation selectors, skin-tone modifiers, ZWJ
  sequences, and regional-indicator flag pairs through `capture_text`, live
  render buffers, copy mode, status-bar extraction, pane capture, and snapshot
  rasterization.
- Added parser anchor clearing and width-invariant coverage for cursor/ESC
  motion, final-column clusters, CJK-adjacent combining marks, ZWJ width
  expansion, flag-pair merging, and REP of grapheme payloads.
- Added `make test-vt-grapheme` and `make test-vt-grapheme-performance`, with
  80x24, 120x40, and 200x60 full-resolution PNG evidence, exact pixel metrics,
  and performance reports under `.shux/qa/069-shux-vt-grapheme-cell-storage/`.
- Verification: `make test-vt`, `make test-vt-corpus`,
  `make test-vt-wide-invariants`, focused shux-ui/shux-raster grapheme tests,
  `SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io FILTER=grapheme`,
  `make test-vt-grapheme-performance`, `make test-vt-grapheme`,
  `make check-vt-qa`, `SHUX_TEST_BINARY_TIMEOUT_SECONDS=180 make check`, and
  SOLID VT QA PASS.

**2026-06-12 — feat(vt): preserve wide-cell invariants**
- Completed task 068 by adding row/grid wide-cell repair primitives, final-column
  width-2 handling, resize-canvas sanitization, saved-cursor clamping, focused
  unit coverage, and a proptest operation-sequence invariant over visible rows
  plus scrollback.
- Added `make test-vt-wide-invariants` and `make test-vt-wide-visual`, with
  shux-driven 80x24, 120x40, and 200x60 PNG captures compared exactly against
  committed `.shux/goldens/068-shux-vt-wide-cell-invariants/` baselines.
- Extended the VT corpus with a mixed CJK/ANSI/DEC/edit/resize synthetic
  fixture and regenerated committed corpus goldens, closing the integration
  fixture coverage gap found by implementation review.
- Verification: `make test-vt FILTER=wide`, `make test-vt-wide-invariants`,
  `make test-vt`, `make test-vt-corpus`, `make test-vt-wide-visual`,
  `SHUX_TEST_BINARY_TIMEOUT_SECONDS=180 make check`, and SOLID VT QA PASS.

**2026-06-12 — start(vt): wide-cell invariants**
- Started task 068 on branch `feat/vt-wide-cell-invariants` after landing task
  067. Scope is hardening every width-2 cell mutation path so orphan
  continuations, missing tails, ghost cells, and duplicate capture output are
  caught by unit, integration, shux automation, visual, pixel, and SOLID QA
  gates.

**2026-06-12 — fix(vt): trim styled blank resize tails**
- Addressed PR review feedback for task 067 by treating trailing visual blanks
  as reflow padding even when erase/reset operations left non-default styling
  on those cells, while still preserving wide-cell continuations.
- Added a focused regression for hard lines with styled blank tails so resize
  no longer wraps invisible padding into extra rows.
- Fixed the pane I/O integration harness shutdown path to terminate and reap
  PTY children on cancellation, preventing orphan login shells from poisoning
  subsequent full-suite runs.
- Verification: `make test-vt FILTER=resize`, `make test-vt-resize-reflow`,
  `make test-vt`, `SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io`,
  `SHUX_TEST_BINARY_TIMEOUT_SECONDS=180 make check`, and SOLID VT QA PASS.

**2026-06-12 — feat(vt): reflow soft-wrapped rows on resize**
- Implemented `shux-vt` column resize reflow over scrollback + visible rows,
  preserving soft-wrapped logical lines, hard line breaks, styles, extended
  attrs, wide-cell pairs, scrollback limits, and cursor anchors.
- Wired `VirtualTerminal::resize()` so primary and synchronized-output
  presentation grids use cursor-aware reflow while alternate-screen buffers
  keep fixed-canvas resize semantics.
- Added `make test-vt-resize-reflow` and `.shux/scripts/resize_reflow_check.sh`
  to drive a real shux pane through 80x24, 120x40, 40x12, and return-to-80x24
  text/PNG proof with exact pixel comparison.
- Hardened pane I/O probe integration timing to avoid false failures under
  full-suite load.
- Verification: `make test-vt FILTER=resize`, `make test-vt`,
  `make test-vt-corpus`, `make test-vt-resize-reflow`,
  `SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io`, and
  `SHUX_TEST_BINARY_TIMEOUT_SECONDS=180 make check`.

**2026-06-11 — test(vt): add corpus regression harness**
- Added the task-073 VT corpus harness with typed synthetic action fixtures,
  committed rich-TUI raw replay fixtures, explicit goldens, exact PNG
  comparison, and machine-readable corpus/pixel reports.
- Wired `make test-vt-corpus`, `make test-vt-corpus-unit`,
  `make promote-vt-corpus-baselines`, and `make record-vt-corpus`; CI now runs
  the exact pixel corpus in the VT QA job before checking the tracked evidence
  contract.
- Saved DootSabha design and implementation reviews under
  `.shux/qa/073-shux-vt-corpus-regression-harness/`, captured full-resolution PNG evidence and pixel
  metric JSON for all 16 replay cases, and ran the SOLID VT QA gate.

**2026-06-11 — docs(vt): harden VT quality gate enforcement**
- Followed up the Claude+Gemini DootSabha council review of
  `docs/shux-vt-quality-track`.
- Tightened the VT quality track from a prose-only gate into a machine-checked
  artifact contract: completed VT tasks must commit `.shux/qa/<task>/SOLID-QA.md`
  with first line `VERDICT: PASS`, `evidence-manifest.json`, full-resolution
  PNG evidence, and pixel metric JSON.
- Updated `scripts/check-progress.sh` and `make check-vt-qa` so Done VT tasks
  fail local progress checks when tracked SOLID QA evidence is missing,
  untracked, or malformed.
- Moved real-TUI replay from optional installed tools to committed raw PTY
  fixtures under `.shux/fixtures/vt-corpus/rich-tui/`.
- Removed the task dependency cycle by making task 073 the first VT Quality
  Track implementation step and task 067 depend on it.
- Tightened tasks 067-074 with explicit baseline provenance, exact pixel
  thresholds, tracked `.shux/qa` evidence paths, and concrete performance
  budgets for grapheme storage and dirty-region tracking.

**2026-06-11 — docs(record): sync lossless recording across public surfaces**
- Swept recent feature-release surfaces after PR #72: README, human/agent
  guides, website copy, PRD/design docs, repo-bundled shux skill, and local
  installed shux skill copies under `.agents` and `.codex`.
- Replaced stale `pane.output.watch`-as-recording guidance with the current
  contract: `pane.output.watch` is sampled live observation; `pane.record.*`
  / `shux pane record --to FILE` is the byte-exact transcript path.
- Checked recent released feature coverage. Status bar/onboarding,
  emoji/font-fallback snapshots, mouse copy, richer TUI rendering, xterm probe
  support, plugin hot reload, and `state.apply` remain represented in the
  website/README/skill surfaces; the missing coverage was the new recorder and
  stale exhaustive/lossless pane-output wording.

**2026-06-11 — fix(record): validate pane record session scope**
- Follow-up from PR review: `shux pane record -s <session>` now verifies the
  target pane belongs to the requested session before calling
  `pane.record.start`, so UUID-only access cannot cross session boundaries.
- Tightened the CLI RPC fixture so successful result payloads can include
  `"error": null` without being misclassified as JSON-RPC errors.
- Verification after the review fix: `make fmt`, focused `pane_record` tests,
  `make test-lossless-record`, `make ci`, `make deny`, and `git diff --check`
  passed. `make check-progress` required this fresh session-log entry.

**2026-06-11 — fix(record): add lossless pane output recording (issue #70)**
- Issue #70 is real: `pane.output.watch` remains intentionally sampled and
  capped at the PTY source, so absence-of-bytes assertions over watch output
  are unsound.
- Added `pane.record.start` / `pane.record.stop` and `shux pane record` as a
  separate raw PTY recorder. It tees bytes immediately after a successful PTY
  read, before VT processing and before sampled `pane.output.watch` coalescing.
- Recorder state is fail-closed: `stop` reports `status`, `lossless`,
  `bytes_written`, and `error`; v1 enforces one active recorder per pane,
  daemon-side `duration_ms`, client-side output-path resolution, create-new by
  default, and Unix `O_NOFOLLOW` on `--force` opens.
- `shux pane watch` now documents sampled/lossy semantics and warns on sampled
  chunks in text/plain output while keeping JSON chunk metadata intact.
- Verification: DootSabha design council, focused `pane_record` tests,
  `make test-lossless-record`, and `make ci` passed. Real-tool proof under
  `.shux/out/issue-70/`: deterministic raw record SHA-256 matched expected,
  and gh-hound, vivecaka, and btop recordings all ended `complete` /
  `lossless=true` with PNG screenshots.

**2026-06-11 — fix(attach): preserve pane colors when daemon inherits NO_COLOR (issue #69)**
- Issue #69 is an attach-renderer bug, not a guest TUI bug: pane PTYs and
  snapshot rasterization can retain color while `session attach` strips pane
  fg/bg color if the shux daemon inherited `NO_COLOR=1`.
- Published release checks disproved `v0.25.0` as the introduction point:
  both `v0.24.3` (May 17, 2026) and `v0.25.0` (May 18, 2026) rendered
  `vivecaka --repo indrasvat/gh-hound` with color in a clean daemon and
  emitted zero attach color SGR when the daemon started under `NO_COLOR=1`.
- Root cause was crossterm's process-global color-disabled gate. The attach
  backend previously serialized pane colors through crossterm
  `SetForegroundColor` / `SetBackgroundColor`, which produce empty color SGR
  when the global gate is set.
- Fixed `shux-ui::RenderBackend` with renderer-local ANSI color commands for
  foreground, background, and underline color. The fix does not mutate
  crossterm global state, so ordinary CLI `NO_COLOR` semantics remain intact.
- Added guardrails: focused `make test-ui`, unit coverage for RGB/256/basic
  color SGR under disabled crossterm global color state, multi-pane attach
  compositor coverage, and `make test-attach-color` for a release-binary PTY
  attach smoke under daemon `NO_COLOR=1`.
- Verification: `make test-ui`, `make test-attach-color`, and `make ci` passed.
  Patched real-`vivecaka` proof under daemon `NO_COLOR=1` emitted
  `truecolor_sgr=182`, `indexed_sgr=2`, `empty_sgr=0`; snapshot proof is
  `.shux/out/issue-69/patched-no-color-vivecaka-pane.png`.

**2026-06-08 — fix(snapshot): broaden PNG text-symbol fallbacks (issues #65/#66)**
- Issues #65/#66 are real: GH-Hound renders common TUI glyphs correctly
  in Ghostty while shux PNG snapshots tofu default-rasterizer gaps such
  as `↻` and braille spinner symbols. Desktop evidence included
  Ghostty screenshots plus shux crops where `↻ rerun` became a missing
  glyph box.
- Bundled three OFL Noto text-symbol fallbacks:
  `NotoSansMath-Regular.ttf` (rerun/arrow/math symbols),
  `NotoSansSymbols2-Regular.ttf` (braille spinners, status symbols,
  geometric UI glyphs), and `NotoSansSymbols-Regular.ttf`
  (Miscellaneous Technical glyphs like `⎇` / `⎈`). Default chain is now
  `[JBM_NF, NotoSansMath, NotoSansSymbols2, NotoSansSymbols, NotoEmoji]`.
- Added `appearance.font_fallbacks: Option<Vec<String>>` for
  snapshot-only ordered fallbacks. Omitted config uses the default
  builtin chain; explicit config can mix builtin tokens with font paths
  without replacing the primary metrics font. Hot-reload key now tracks
  both `appearance.font` and `appearance.font_fallbacks`; bad fallback
  paths keep the last-good rasterizer.
- Coverage expanded beyond the two reported glyphs: deterministic tests
  assert non-empty raster output for arrows/key legends, braille spinner
  frames, status/check/cross symbols, stars/checkboxes, progress blocks,
  box drawing, geometric markers, segmented circles/squares, and common
  Nerd Font icons. A local `fc-scan` pass over the curated common-TUI set
  reported `missing_count=0` for the final chain.
- Config docs, `shux config show` template, strict validator mirror, and
  asset NOTICE provenance updated. `shux config validate` rejects invalid
  `font_fallbacks` types, empty fallback lists, misspelled builtin tokens,
  and missing fallback files. Tests assert custom fallbacks preserve the
  primary metrics cell size; if `appearance.font` is unset, bundled JBM
  remains the metrics anchor and explicit fallbacks only change glyph
  coverage.
- Skill docs and README snapshot notes updated so agent-facing guidance no
  longer claims common scalar glyphs render as tofu; remaining limitations are
  scoped to renderer-v2 work such as shaping, color/composed emoji, CJK/system
  font discovery, and platform font fallback.
- Long-term renderer-v2 scope split into GitHub issue #67: true
  terminal-grade parity needs grapheme-cluster VT storage, shaping,
  platform font discovery/fallback, color glyph support, and optional
  terminal-backed capture. This branch is intentionally the tactical
  #65/#66 compatibility layer, not the final renderer architecture.
- Verification: `make fmt`, `make lint`, `make test-lib`, `make release`.
  `make test-lib` passed; `shux-raster` and `shux-core` each hit the
  known 45s per-binary runner timeout once and passed on retry. Real
  GH-Hound proof captured from
  `/Users/indrasvat/code/github.com/indrasvat-gh-hound/bin/gh-hound`
  inside patched shux:
  `.shux/out/font-fallback-gh-hound-after-review/gh-hound-runs-after-review.png`.
- PR CI found one bad bin-test assertion for the invalid `font_fallbacks`
  type diagnostic context; fixed the assertion and verified the
  `config_validate` bin test module locally.

**2026-05-27 — fix(vt): cursor save/restore + idempotent alt-screen (issue #61)**
- VT parser now handles `CSI s` / `CSI u` (SCOSC/SCORC) cursor save/restore and
  DEC private mode 1048 save/restore, fixing Bubble Tea-style diff redraws that
  left stale cells when apps saved/restored the cursor between frames.
- Alternate-screen enter/leave (1047/1049) made idempotent and split by mode:
  repeated `?1049h` no longer discards the primary grid, 1047 no longer
  restores the primary cursor, and `?1049l` still performs its 1048-style
  cursor restore even if the primary screen is already active.
- Added focused regression tests (truecolor mid-row SGR over box-drawing cells,
  short-redraw EL clearing, CSI s/u redraw, sync-mode Bubble Tea redraw, 1048
  save/restore, repeated alt-screen enter) and a `make test-vt` target.
- Fixed `scripts/run-cargo-test.sh`: a failing test binary now propagates its
  nonzero status (the old `if cmd; then …; fi; status=$?` always captured 0,
  masking failures and disabling the timeout-retry path).
- DootSabha implementation review found VT-mode edge cases. Addressed in this
  branch: parameterized `CSI u` is not treated as cursor restore, 1047 no
  longer restores the primary cursor, 1049 leave restores saved cursor even
  when already on primary, and the Bubble Tea regression now covers multiple
  inner truecolor token transitions.
- Follow-up PR review and manual retest caught a remaining stale-prefix redraw
  case. Added renderer primitive support for `REP` (`CSI Ps b`), cursor
  tabulation (`CSI I`/`CSI Z`), relative movement (`CSI a`/`CSI e`), and
  no-op tab-stop setup/clear handling (`ESC H`, `CSI g`) so optimized renderer
  batches that use repeated spaces and tab-relative cursor movement land on the
  intended cells. Added regressions for `REP` clearing and nested 1049 while
  already on alternate screen.
- Second manual retest showed a stale token from the scanning frame surviving
  into the summary frame. Added `HPA` (`CSI Ps \``) support so absolute
  horizontal movement before `EL` clears the intended range, plus `SU`/`SD`
  hard-scroll primitives from the same renderer capability set. Also wired
  OSC 8 hyperlinks and advanced underline style/color SGR into existing
  extended cell attributes. Added regressions for HPA-before-erase stale text,
  scroll up/down regions, OSC 8 links, and underline style/color.
- DootSabha review caught follow-up extended-attribute propagation gaps. Render
  cells now preserve extended attributes for diffing, emit/clear OSC 8
  hyperlinks, and render advanced underline style/color through crossterm.
  OSC 8 URI parsing now preserves semicolons, and DECRQSS SGR reports advanced
  underline style/color instead of collapsing to plain `4m`.

**2026-05-27 — fix(render): Bubble Tea / Charm coverage audit (issue #63)**
- Audited Bubble Tea and Charm official renderer/VT sources for
  rendering-adjacent behavior that affects shux attach rendering and PNG
  snapshots. Existing coverage already included alternate screen, synchronized
  output, renderer cursor/erase/scroll primitives, OSC 8 links, and advanced
  underline state.
- Closed the remaining small rendering gaps: VT now handles OSC 12/112 cursor
  color set/query/reset; attach emits focused-pane cursor shape/color and
  resets both on teardown; pane/window PNG snapshots render block, underline,
  and bar cursors with OSC 12 color; raster snapshots now respect advanced
  underline style/color instead of drawing every underline as a single fg line.
- Added `.shux/scripts/issue_63_render_matrix.sh`, which generates stale-cell,
  color/meta, cursor, OSC title, synchronized-output, and Vim smoke PNGs under
  `.shux/out/issue-63/`, validates the files, and asserts synchronized output
  does not leak unreleased text into capture.
- Added focused regressions across `shux-vt`, `shux-ui`, and `shux-raster` for
  OSC 12 responses, cursor shape/color emission, cursor shape/color PNG
  rendering, and advanced underline color rasterization.
- Follow-up review tightened colored block cursor snapshots so OSC 12 cursor
  color paints the cursor cell while preserving the underlying glyph; added a
  raster regression and reran the issue #63 visual matrix.
- DootSabha brutal review found additional cross-path rendering gaps before
  merge. Addressed live attach hidden-cursor visibility, synchronized-output
  presentation freezing for OSC defaults/title, OSC 12 query fallback to the
  dynamic foreground, panic-hook cursor presentation reset, and wide/hidden
  cursor raster edge cases.

**2026-05-18 — feat(copy): direct mouse selection and inline copy menu**
- Normal-mode mouse selection is now a first-class attach-layer state,
  separate from modal copy mode. Left-dragging visible pane text selects
  it, release copies it through OSC 52, and the highlighted selection
  stays visible without trapping keyboard input.
- Right-clicking an active selection opens a small inline `Copy` /
  `Clear` menu near the pointer. Typing into the pane clears the
  selection and resumes normal PTY input.
- Copy-mode remains the advanced path for scrollback, search, and
  keyboard-only workflows. The help overlay, README, user guide, website,
  and repo skill now document the mouse-first path so ordinary copy does
  not require prefix-mode knowledge.
- Dogfood automation injects real SGR mouse sequences through a live
  attach session, verifies high-contrast selection bytes, confirms the
  inline menu text, detects OSC 52 output, and keeps the idle repaint
  guard at `0` bytes.
- Verification: `make dogfood-human-copy`, `make ci`, and pre-push
  checks (deny, progress-check, 835 tests, doctests).

**2026-05-20 — fix(xterm): address PR #57 review follow-ups**
- Follow-up for merged PR #57 review feedback: OSC color query replies now
  preserve the query terminator style (`BEL` vs `ST`) so BEL-parsing clients
  do not wait for a terminator shux never sends.
- DECRQSS `$qm` now reports the active SGR state instead of always returning
  `0m`, so applications that query-and-restore rendition do not lose active
  styling.
- Added focused coverage for BEL-terminated OSC replies, dynamic default color
  query/reset behavior, extended 256-color palette queries, failed DCS query
  replies, and active/indexed SGR DECRQSS responses.

**2026-05-20 — feat(xterm): answer application terminal probes**
- Added the first truthful `TERM=xterm-256color` response layer. The VT now
  returns response bytes for DA/DA2/DSR cursor reports, OSC 10/11/4 color
  queries, XTGETTCAP, and DECRQSS, and the pane PTY task writes those bytes
  back to the child process outside the pane I/O lock.
- Added focused unit coverage for the response-producing parser path and a
  PTY integration regression proving a child process that emits CPR (`CSI 6n`)
  receives the expected `ESC[row;colR` reply.
- Added `.shux/scripts/xterm256_rich_tui_check.sh` to dogfood rich TUIs under
  `TERM=xterm-256color`, including `vivecaka --repo=indrasvat/shux` so PR proof
  screenshots show the actual Shux PR when one is open.

**2026-05-20 — feat(xterm): robust mode reports and synchronized output**
- Started task 064 on `feat/xterm256-full-support`. Online research was
  refreshed against XTerm patch #410 / terminfo v1.216, tmux terminal guidance,
  Bubble Tea v2 releases, and Neovim TUI docs as of 2026-05-20.
- `dootsabha council --json` was attempted for the design review but hung with
  no JSON output and was killed; implementation proceeded conservatively with
  source-backed scope and focused tests.
- VT now answers DECRQM for tracked ANSI/private modes, answers XTVERSION, and
  tracks application keypad, focus-event, SGR mouse, and synchronized-output
  mode state.
- Synchronized output mode 2026 now freezes the presented grid/cursor while the
  app is inside a synchronized update block and exposes the accumulated working
  frame on reset. This targets modern Bubble Tea v2-style renderers that query
  and use mode 2026.
- XTGETTCAP now covers additional common color, cursor-shape, alternate-screen,
  keypad, and OSC 52 capabilities.
- Verification so far: `make test-lib`, full `pane_io_integration`, release
  build, and Shux rich-TUI proof under
  `.shux/out/xterm256-rich-tui-20260520-105116/` with `lazygit`, `btop`,
  `nvim`, `vicaya-tui`, `vivecaka --repo=indrasvat/shux`, and a synchronized
  output probe. Launch timing data is stored at
  `.shux/out/xterm256-launch-timing-20260520.json`.

**2026-05-18 — fix(copy): make selection legible and stop idle cursor churn**
- User dogfood findings on the human-interactive branch: copy-mode
  selection used a too-dark overlay that made text hard to read, and an
  attached SHUX pane visibly blinked/janked while idle. Computer Use was
  re-tried against Ghostty after a Codex restart, but the MCP still
  rejected `com.mitchellh.ghostty` as not allowed; verification continued
  with PTY/expect automation plus SHUX snapshots.
- Copy-mode overlay now redraws selected glyphs from the focused
  `VirtualTerminal` with explicit high-contrast foreground/background
  instead of painting a block over text. A regression test asserts the
  selected glyph remains present and styled.
- Attach rendering no longer forces a base-frame redraw every copy-mode
  tick. The render loop tracks a copy overlay stamp and only redraws or
  repaints when the overlay state changes or underlying pane bytes
  arrive.
- The compositor now tracks the terminal cursor state. First render still
  initializes cursor visibility, but idle frames with no dirty cells and
  unchanged cursor emit no hide/show/move bytes; cursor-only movement
  emits only the cursor move. This fixes the visible idle blink.
- PTY child process env now defends interactive panes from degraded
  parent environments by preferring installed `TERM=tmux-256color`
  terminfo with `screen-256color` / `xterm-256color` fallback,
  `COLORTERM=truecolor`, `CLICOLOR=1`, and removing inherited
  `NO_COLOR` unless explicitly restored through `PtyConfig.env`.
- Dogfood automation in `.shux/scripts/human_copy_mode_check.sh` now
  exercises a real attach session through `expect`, enters copy mode,
  searches/selects text, verifies high-contrast selection ANSI, checks
  `NO_COLOR=unset` inside the pane, snapshots PNG evidence, and measures
  idle output volume. Current proof: idle attach delta `0` bytes.
- Verification: focused copy/attach/compositor tests,
  `make dogfood-human-copy`, and `make test` (830/830).

**2026-05-18 — feat(human): scrollback copy, keybinding config, session persistence**
- Built the human-interactive core branch around three daily-driver UX
  features. `dootsabha council --json` was attempted twice per project
  protocol, but the command hung with no output both times and was
  killed; proceeded conservatively after documenting the blocker in the
  working session.
- Scrollback copy mode now uses the VT grid's `scrollback + visible`
  logical row space. Copy mode supports PageUp/PageDown, Ctrl-b/f,
  Ctrl-u/d, gg/G, `/` and `?` search, `n`/`N` repeat search, mouse-wheel
  scroll while active, historical selection/yank, and live overlay
  rendering of scrolled history without changing snapshot paths.
- Configurable attach keybindings landed for root and prefix tables:
  `[keybindings]` TOML overrides, configurable prefix, action alias
  validation, conflict replacement semantics, config-validator
  diagnostics, default config comments, and attach-loop resolution
  through `KeybindingRegistry`. The broader task-031 runtime
  `keybinding.*` RPC/plugin provenance layer remains deferred.
- Session persistence landed as an explicit template round-trip:
  `session.export_template` RPC, `shux session save -s NAME -o FILE`,
  `shux session restore FILE`, and `--dry-run` restore output via the
  existing `state.apply` lowering path. Saved templates preserve session
  name, window titles, pane cwd, command, split direction, and ratio
  where reconstructable.
- Added dogfood automation:
  `.shux/scripts/human_keybindings_check.sh`,
  `.shux/scripts/human_session_persistence_check.sh`, and
  `.shux/scripts/human_copy_mode_check.sh`. Outputs are written under
  `.shux/out/`; verified PNG evidence for a restored split session and
  a scrollback-heavy pane.
- Verification: `make build`; focused copy/keybinding/session tests;
  all three dogfood scripts; `make fmt`; `make lint`; `make test`
  (822/822).

**2026-05-18 — fix(mouse): reduce capture and add copy-mode drag**
- User dogfood finding: Codex and other TUIs look materially different
  inside a SHUX attach than when launched directly, and mouse-drag text
  copy did not work in an attached session.
- Analysis: the visual difference has three layers: expected SHUX chrome
  (border, pane title, status bar, inset PTY size), live attach vs host
  terminal behavior, and headless PNG rasterizer limitations. Task 061
  now documents the current VT/raster gaps and the phased parity plan.
- Fix: `TerminalGuard::enter()` no longer uses crossterm's broad
  `EnableMouseCapture`, which enabled `?1003h` any-motion tracking.
  SHUX now enables only `?1000h` press/release, `?1002h` button-held
  drag, and `?1006h` SGR coordinates. This preserves click focus and
  border resize while avoiding unnecessary any-motion capture.
- Copy-mode improvement: while `Prefix + [` copy mode is active,
  left-drag inside the focused pane updates the existing
  `CopyModeState`; mouse-up yanks via the same OSC 52 path as keyboard
  `y` only when the cursor actually moved, then exits copy mode.
  Click-only down/up does not copy a stray single cell. Zoomed panes use
  the full visible pane rect for coordinate mapping.
- Verification: `make fmt`, `make fmt-check`, two full `make test`
  runs (803/803 each, triggered while attempting focused filters), and
  `make lint` / clippy.

**2026-05-17 — fix(session): add initial pane title flag**
- User dogfood finding: `shux session create -s aww-shux --cmd "codex
  --yolo"` correctly named the session, but the top pane border still
  showed the cwd-derived/OSC title (`shux-demo`). That is technically
  consistent with pane title priority, but it is confusing for
  one-pane interactive sessions where the visible border should be
  easy to pin at creation time.
- Fix: `shux session create --title TITLE` now sends `pane_title` on
  `session.create` / `session.ensure`. The daemon applies it as the
  initial pane's manual title before spawning the PTY, so later OSC
  titles emitted by shells or agent apps do not overwrite it.
- Regression coverage: CLI parsing/param-building asserts `--title`
  becomes `pane_title`; the M0 RPC integration test creates a session
  with `pane_title` and verifies the initial pane has both `title` and
  `manual_title` set while preserving its command metadata. A follow-up
  regression test covers the review-found race where a watcher/plugin
  changes the active pane before the helper pins the initial title.
- Verification: focused CLI and M0 tests, `cargo check -p shux`, `make
  fmt`, `make fmt-check`, `make test` (799/799), and `git diff --check`.

**2026-05-17 — fix(statusbar): default Starship segments to raw ANSI**
- User hit literal `\[\]23:46\[\]` around the right statusbar clock
  when a `[[statusbar.segment]]` ran `starship prompt`. Root cause:
  Starship emitted Bash PS1 non-printing guards around ANSI escapes,
  and shux correctly treated segment stdout as terminal bytes rather
  than shell prompt metadata.
- Fix: when a segment has inline `starship_config`, the runner now
  defaults the spawn env to `STARSHIP_SHELL=cmd` and
  `TERM=xterm-256color`, preserving explicit user overrides. Generated
  `shux config init/show` output, user docs, configuration docs, and
  runtime config comments now document the raw-ANSI default.
- Regression coverage: runner unit tests assert the raw-ANSI defaults
  are applied and explicit env values are preserved; config-validator
  regression asserts the emitted default Starship segment carries those
  env defaults.
- Verification: `make fmt`, `make fmt-check`, `make lint`, `make test`
  (795/795), `make release`, plus isolated `XDG_CONFIG_HOME` /
  `XDG_RUNTIME_DIR` shux smoke captures. PR #50 has two uploaded Chrome
  comment screenshots: generated default raw-ANSI statusbar and an
  interactive `/tmp/shux-demo` session showing a clean clock.

**2026-05-17 — fix(cli): make raw RPC cwd help copy-safe**
- Review follow-up on PR #50: the raw `session.create` RPC example in
  `shux --help` showed `"cwd":"$PWD"` inside single-quoted inline JSON,
  which copy/pasted as a literal `$PWD` instead of the caller's directory.
- Fix: the help now uses double-quoted inline JSON with escaped inner
  quotes and `"cwd":"$(pwd)"`, so common POSIX shells expand the cwd
  before passing JSON to shux. Added a regression test pinning the
  copy-safe example and rejecting the literal `$PWD` form.
- Verification: focused `cargo test -p shux
  cli::tests::test_agent_help_raw_rpc_cwd_example_is_copy_safe`, `make
  fmt-check`, `make test` (796/796), and `git diff --check`.

**2026-05-15 — feat(snapshot): emoji glyph fallback in PNG rasterizer (issue #46)**
- Issue #46: PNG snapshots dropped emoji glyphs (rendered as tofu /
  blank). Bug surfaced repeatedly across shux dev work — user
  explicitly asked for a *proper* fix, not a stopgap.
- `dootsabha council` review on the design proposal (codex + gemini,
  chair = claude per `~/.config/dootsabha/config.yaml`). Convergence:
  swap to swash for colour emoji is the wrong v1 — `shux-vt::Cell`
  stores one `char` per cell, so the parser already splits ZWJ
  sequences (`👨‍💻`), VS16 (`🛠️`), regional-indicator flag pairs, and
  skin-tone modifiers BEFORE the rasterizer sees them. Even with the
  best COLRv1 rasterizer you can't reconstruct what was split. v1
  lands monochrome standalone emoji via fontdue + bundled Noto Emoji;
  colour + composed emoji deferred to a future `shux-vt`
  grapheme-cluster PR.
- Bundled `crates/shux-raster/assets/NotoEmoji-Regular.ttf` (Noto Emoji
  Version 3.005, monochrome variable-weight, ~860 KB, SIL OFL-1.1).
  Append as final entry in every rasterizer's font chain:
  `Rasterizer::new()` → `[JBM_NF, NotoEmoji]`,
  `Rasterizer::with_primary_font(p)` → `[primary, JBM_NF, NotoEmoji]`.
- Wide-cell math fix: when the glyph is from a fallback font (not the
  primary text font), re-rasterize at a font size that fits inside
  `cell_w * (is_wide ? 2 : 1)` (never enlarging, floored at 6pt) and
  center within the cell box. Without this, Noto Emoji's wider native
  advance spilled the emoji bitmap into the adjacent column.
- Hot-reload via `Arc<arc_swap::ArcSwap<Rasterizer>>`. Spawned task
  subscribes to `ConfigHandle::change_notify()`, rebuilds the
  rasterizer on `appearance.font` change, keeps the last-good on
  rebuild failure. Snapshot RPC handlers `.load_full()` per call.
  Closes "font config requires daemon restart" UX gap.
- Validator strict-mirror audit: `strict::Appearance` was missing
  `nerd_fonts` + `font`; same audit surfaced `strict::Theme` missing
  `status_muted` + `status_branch`. Added all four. New regression
  test `validate_emitted_default_config_is_ok` round-trips
  `cli::DEFAULT_CONFIG_TOML` so any future template field that lands
  in runtime but not the strict mirror trips a hard test failure.
- Tests added: `default_chain_has_emoji_fallback`,
  `bundled_emoji_font_covers_important_emoji_glyphs` (curated 15-emoji
  tofu-free set, parallel to existing NF set),
  `fallback_emoji_glyph_stays_inside_wide_cell_bounds` (renders
  `"🍺 "` into a 1×3 grid and asserts zero non-bg pixels in column 3),
  `validate_maximal_appearance_block_is_ok`,
  `validate_maximal_theme_block_is_ok`,
  `validate_rejects_nerd_fonts_type_error`. Updated
  `with_primary_font_keeps_bundled_fallback` (chain length 2 → 3).
- Visual evidence at `.claude/screenshots/font-fallback/`:
  5 cells (pane/window/session snapshot at default config, plus
  pane snapshot at malformed-font-path and hot-reload states).
  All show emoji rendering legibly; malformed state confirms
  graceful fallback to bundled chain.
- Learning entry added to `CLAUDE.md` (will migrate to
  `docs/agents/learnings.md` once PR #47 merges).

**2026-05-15 — fix(statusbar): include script segments in PNG snapshots (post-#43 followup)**
- PR #43 (v0.23.0) wired `populate_bar(&mut bar, &config, &segments)`
  into the attach render loop but the snapshot path
  (`window.snapshot` / `session.snapshot`) only called the first
  half of the bar assembly. Result: user-defined
  `[[statusbar.segment]]` entries (starship prompt, kubectl context,
  AWS profile, disk, battery, …) fired correctly on a live attach
  but silently vanished from PNG snapshots — a parity gap, since
  the rasterizer is a defining shux capability ("pixel-perfect
  snapshots = terminal as artifact").
- Threaded `SegmentCache` through
  `register_pane_io_methods` → `snapshot_window` →
  `build_snapshot_status_bar` and called
  `statusbar_runner::populate_bar` after the OOTB bar is built, matching
  what the attach loop already does. Three callsites updated:
  `run_rpc_server` (the `register_pane_io_methods` invocation),
  `window.snapshot` RPC, `session.snapshot` RPC.
- `pane.snapshot` is single-pane rasterize with no status-bar
  chrome by design — not a render path for this feature.
- Tests: new `#[cfg(test)]` regression test
  `snapshot_statusbar_includes_script_segments` in `main.rs` pre-populates
  the cache via a `#[cfg(test)] pub set_for_test` setter and asserts
  segment text reaches the rendered StatusBar. Production `set` stays
  module-private (single writer property preserved). 771/771 tests
  pass via `make test`.
- Visual matrix under `.claude/screenshots/oob_bar/`:
  `v23_post_fix_no_segments_200x28.png` (OOTB-only control),
  `v23_post_fix_200x28.png` (window.snapshot × rich segments — starship +
  kubectl visible), `v23_post_fix_session_snapshot_200x28.png`
  (session.snapshot × rich segments — same as window).
- HANDOFF.md moved from repo root to `.local/HANDOFF.md` (gitignored).
- Local dootsabha council ran on the implementation diff per
  `feedback_full_feature_protocol.md` before pushing. Three rounds
  converged: P1 (env-var leak in test) → fixed via
  `OnboardingHandle::from_state_for_test`; final verdict
  `approve_with_nits` zero findings.
- PR #45 opened, CI green 7/7. GitHub Codex bot flagged P2 cold-start
  race: a snapshot fired immediately after daemon start (or config
  reload) could observe an empty `SegmentCache` because the runner
  tasks hadn't completed their first tick yet — `populate_bar` would
  then silently emit no segments and the one-shot PNG had no later
  redraw to recover. Fix: added
  `SegmentCache::wait_for_first_outputs(expected_count, timeout)`
  (polls 25 ms until cache has ≥ expected_count entries) and call it
  from `build_snapshot_status_bar` with a 1.2 s budget (slightly
  above the runner's 1 s per-command timeout so fallback writes have
  room to land) before `populate_bar`. Exact-key check (not `len()`)
  matches what `populate_bar` actually reads. 5 unit tests on the
  wait helper.
- User reported tofu in PNG snapshots of the rich-config bar. Deep
  diagnosis: the OOTB rasterizer bundled `JetBrainsMono-Regular.ttf`
  (270 KB) plus a hand-curated 4.8 KB `SymbolsNerdFontSubset.ttf`.
  The subset covered only the ~20 codepoints shux's own statusbar
  builder emits — NOT the much wider set users' script segments
  (starship rust/node/python/go, kubectl, etc.) actually emit.
  Hidden defect that had been silently failing since PR #43; the
  rust 🦀 emoji and nodejs  glyph and most others were all tofu,
  just visually small enough to look like part of the design.
- Decision: scrap the subset, bundle the full 2.4 MB
  `JetBrainsMonoNerdFontMono-Regular.ttf` (upstream Nerd Fonts
  patched JetBrains Mono Mono Regular, OFL). One asset, complete NF
  coverage, no subset-regen ritual. Net +2.1 MB on the release
  binary (~11.8 MB → previously ~9.7 MB) is an acceptable trade
  for "no tofu OOTB on the rasterizer, which is shux's defining
  feature." Deleted `assets/SymbolsNerdFontSubset.ttf`,
  `assets/JetBrainsMono-Regular.ttf`, and
  `REGENERATE_SYMBOLS_SUBSET.md`. Simplified `Rasterizer::new` to
  a single-font chain. `Rasterizer::with_primary_font(primary)`
  keeps the bundled NF JBM as a fallback so user-supplied
  plain (non-patched) fonts still get NF coverage via the chain.
- Deterministic verification (no vision dependence):
  - `Rasterizer::has_glyph(ch)` — exposes the fontdue `cmap` lookup.
  - `Rasterizer::glyph_pixel_count(ch)` — rasterizes the glyph and
    counts non-zero coverage pixels. Catches "font has the codepoint
    but the outline is empty" (visually tofu even though
    `glyph_id != 0`).
  - New tests in `crates/shux-raster/src/lib.rs`:
    `bundled_font_covers_ascii`,
    `bundled_font_covers_important_nf_and_unicode_glyphs`,
    `bundled_font_renders_important_glyphs_as_non_empty_bitmaps`,
    `with_primary_font_keeps_bundled_fallback`,
    `alt_nf_fonts_load_and_resolve_important_glyphs_when_staged`
    (local-only: skipped when `.local/fonts/` not staged).
  - "Important glyphs" contract pins 16 codepoints: shux's own
    NF chrome (terminal/branch/home), starship language modules
    (rust/node/python/go/ruby), kubectl/cluster (NF kubernetes /
    ship-wheel / docker), plus the Unicode fallback set used when
    `nerd_fonts=false`.
  - Deliberately excluded from contract (documented in source):
    obscure BMP `⎈` U+2388 (helm) and `⎇` U+2387 (alt-branch) —
    NEITHER JetBrains Mono nor the upstream Symbols Nerd Font has
    them. Steered users to NF equivalents (`nf-md-kubernetes` etc.)
    in `shux config init`'s template comments. Color emoji (🦀)
    also out — steered users to `[rust] symbol = ""` (NF rust logo).
- Visual matrix captured under
  `.claude/screenshots/oob_bar/fonts_<font>_window_<width>.png` for
  {default JBM-NF, FiraCode NF Mono, Hack NF Mono} × {200×24, 120×24}.
  Alt fonts staged under `.local/fonts/` (gitignored).
- 12/12 shux-raster tests pass, 777/777 total. Cargo.toml license
  updated to reflect new asset filename.

**2026-05-15 — feat(statusbar): delightful OOB experience + onboarding**
- Bare `shux` (no config, no `shux config init`) used to show a
  3-segment hardcoded bar: `◆ <session>` / `[i/n] <window>` / clock.
  No onboarding hint, no project context, looked indistinguishable
  from a "started but not configured" state. User flagged this as
  "bare bones" and asked for delight OOTB.
- Council-first design (dootsabha → codex). Codex's pushbacks shaped
  the design: ship in two releases (not one) — R1 = better bar +
  onboarding, R2 = async project intelligence. No imaginary REC
  indicator. Nerd fonts default OFF (the embedded rasterizer doesn't
  have NF glyphs → tofu = trust-killer for first PNG). Static hint,
  not animated. Big first-attach toast does more for discoverability
  than tweaking the bar. Progressive disclosure by width tier.
- Built R1 in this PR:
  - **LEFT zone**: `◆ <session>  ± <branch>  @ ssh` — session identity
    plus auto-detected git branch from the session's spawn cwd plus
    SSH host indicator when daemon is over SSH. Cached per-session
    in a new `SessionMetaCache`; populated once at session-create
    via spawn_blocking, no synchronous IO in the render path. No
    refresh loop (Codex was clear: timeouts > cadence; punt to R2).
  - **CENTER zone**: `Z Y ▶ #i/n <title> · N panes` — mode flags
    (zoom/copy) then window navigation + pane count. Pane count
    omitted at <80 cols.
  - **CENTER transient overlay**: ~1.5s post-action feedback —
    `[pane split]`, `[zoom toggled]`, `[pane closed]` etc. Only
    fires for actions whose effect is otherwise ambiguous (no
    `[focused right]` flash on every nudge — that'd be noisy).
  - **RIGHT zone**: `^Sp ?  help · ^Sp d  detach` until the user
    taps the prefix even once → permanently dismissed; replaced by
    `up Nh Nm` daemon-uptime at ≥120 cols. Dismissal state persists
    to `$XDG_STATE_HOME/shux/onboarding.json` (single boolean).
  - **First-attach welcome toast**: centered Catppuccin-bordered box
    for 3s on the user's very first attach, showing `prefix is ^Sp`
    plus three core shortcuts. Auto-dismisses, marks
    `welcome_toast_seen: true` in the state file, never shows again.
  - **Progressive width tiers**: 60 cols (identity + hint only),
    80 cols (+ branch + pane count), 120 cols (+ uptime
    post-dismissal), 160 cols (room for future multi-client signal).
  - **Distinct from starship**: bar shows session-level
    (multiplexer) signal; starship shows per-pane (PS1) signal. No
    duplicated dir / git / lang / clock.
- Two new modules: `crates/shux/src/session_meta.rs` (git-branch
  detection + SSH detection + cache) and `crates/shux/src/onboarding.rs`
  (state-file load/save with spawn_blocking persistence).
- Shared builder in `crates/shux/src/statusbar_build.rs` used by both
  the live attach renderer AND `window.snapshot` / `session.snapshot`
  so PNGs faithfully reflect the OOTB experience. Earlier the snapshot
  path had drifted to render the v0.22.0 hardcoded bar; consolidated.
- New `appearance.nerd_fonts` config field (default false). Unicode
  fallback uses `◆ ± ▶ @` — clean Catppuccin glyphs. `shux config
  init`'s template enables NF since users opting into the config file
  almost certainly run a modern dev terminal.
- Five Catppuccin color tokens in the theme schema: status_bg /
  status_fg / status_accent / **status_muted** (new) /
  **status_branch** (new). All overridable in `[theme]`.
- Tests: 7 new unit tests in `statusbar_build` covering width tiers,
  prefix label rendering, uptime formatting, action labels, post-
  dismissal right-zone. Plus iTerm-driver live test asserting toast +
  dismissal lifecycle + flag rendering.
- Dogfooded via shux's own pixel-perfect rasterizer (no iTerm
  needed for most evidence): captured 8 snapshots at widths
  60/80/120/160/200, pre+post dismissal, single+multi+zoomed pane.
  Live iTerm capture of the welcome toast confirmed the Catppuccin-
  bordered box renders cleanly with the prefix + 3 shortcuts.
- 768 Rust tests pass, clippy + fmt clean.

**2026-05-15 — fix(attach): pane kill cascade + recursive-shux guard**
- Two interactive bugs caught while dogfooding shux.
- (1) `Ctrl+Space x` was a silent no-op on a fresh `shux` session
  (single pane, single window). `graph.destroy_pane` returns `LastPane`
  on the only pane, the attach handler logged a `warn!` and returned
  `Ok(())` — the user saw nothing. Now does tmux-style cascade in
  `crates/shux/src/attach.rs`: try `destroy_pane`; on `LastPane`, try
  `destroy_window`; on `LastWindow`, `destroy_session`. The render loop
  notices `sessions.contains_key` flips false on next tick, sends
  `SessionEnded`, client detaches cleanly. Graph API stays strict for
  programmatic clients (pinned LastPane/LastWindow semantics); only
  the human-interactive `Ctrl+Space x` cascades.
- (2) Bare `shux` inside an existing shux pane → infinite render-loop
  recursion ("hall of mirrors"). Every pane shux spawns already gets
  `SHUX=1` injected (mirrors tmux's `TMUX`); now bare-shux invocations
  check it and refuse with the tmux-identical terse error:
  `sessions should be nested with care, unset $SHUX to force` (exit 1).
- Auditied every other keyboard shortcut for silent-swallow regressions:
  splits, focus, zoom, resize, window cycle, Alt+1..9 bare bindings,
  help overlay, copy mode, redraw, detach. Only `KillPane` had the
  user-impactful bug. Others either propagate errors via `map_err`
  (split, focus_dir, zoom, new_window) or swallow on benign no-ops
  (focus_window on single-pane, resize on already-correct ratio).
- Tests: 3 new graph-level cascade invariant tests in
  `crates/shux-core/src/graph.rs::tests` proving the underlying
  destroy-pane/window/session sequence the attach handler relies on
  (deterministic, 0ms, CI-friendly). Existing iTerm visual audit script
  (`.claude/automations/test_kill_pane_cascade_all_shortcuts.py`)
  exercises 18 distinct shortcuts end-to-end including the cascade
  itself — all 18 functional asserts pass against the new binary.
- 754 Rust tests pass, clippy + fmt clean.

**2026-05-14 — fix: pane.capture after exit + --text/--regex hyphen values**
- Two friction points flagged by Codex while using shux to verify a
  Textual TUI (independently confirmed in a separate LLM review):
  1. `pane wait-for --text '--search'` failed — clap was treating
     `--search` as an unknown flag. Workaround was `--text=--search`,
     which isn't obvious. Same issue would have hit `--regex` and
     `pane send-keys --text`.
  2. Short-lived commands inside a pane exited before the agent could
     call `pane.capture` — the VT was evicted from `io_state` the
     moment the PTY task observed EOF, so capture returned
     `not_found: pane VT` for a pane the user could still see in
     `pane list`. Codex's workaround was wrapping commands in `sleep`.
- Fixes:
  1. Added `allow_hyphen_values = true` to `wait-for --text/--regex`
     and `send-keys --text`. Documented the tradeoff in code comments
     — these args require values, so the ambiguity surface is bounded.
  2. PTY task cleanup at `main.rs:323` now drops only `writers` and
     `resizers` (PTY-bound). The `VT` lingers until pane is explicitly
     destroyed via `pane.kill` / `window.kill` cascade / `session.kill`
     cascade — same model tmux uses (`remain-on-exit`). `Pane.exit_status`
     is the "dead" flag. `send_keys` / `set_size` to an exited pane now
     fail with `not_found: pane writer` instead of silently working.
- Red→green TDD: 3 new clap parser tests for hyphen values + 1 new
  integration test (`test_capture_works_after_pane_process_exits`)
  that spawns a shell, runs `echo X && exit 0`, waits, then asserts
  `pane.capture` still returns the marker. All four failed pre-fix.
- 751 tests pass, clippy + fmt clean.
- Files: `crates/shux/src/cli.rs` (3 args + 3 tests),
  `crates/shux/src/main.rs` (PTY-exit cleanup),
  `crates/shux/tests/pane_io_integration.rs` (scaffold mirror + 1 test).
- Bundled installer fix (same PR): the landing page advertised
  `curl ... | sh` but `install.sh` was bash-only (`[[ ]]`,
  `$'\033[...'` ANSI-C quoting, `set -o pipefail`). On
  Debian/Ubuntu/Alpine where `/bin/sh` is dash or busybox ash, the
  script blew up early. First instinct was to gate the script to bash;
  on reflection, shux itself has no shell restriction (fish, zsh,
  dash, nu all work fine as pane shells), so the installer shouldn't
  either. Ported `install.sh` to POSIX `#!/bin/sh`:
  - `[[ ]]` → `[ ]`, `==` → `=` (~19 sites)
  - `$'\033[...'` → `ESC=$(printf '\033')` then `${ESC}[...]` (~10 sites)
  - `set -euo pipefail` → `set -eu` (script's error handling is
    already explicit; no pipefail needed)
  - Hardened `mktemp` for portability: `mktemp -d -t shux-install.XXXXXX
    2>/dev/null || mktemp -d` (busybox `mktemp -t` differs from GNU)
  - Kept `local` (de-facto-universal in dash/ash/ksh/bash/zsh)
  Canonical URL now `/install.sh` instead of `/install` — the redirect
  hid the extension which mattered when piping to `sh`. Old `/install`
  alias kept as a backward-compat 200-rewrite in `pages/_redirects`.
- Verification: `shellcheck --shell=sh install.sh` clean. End-to-end
  `--help` smoke test green in 6 shells: macOS bash, macOS dash,
  macOS zsh, Debian dash (Docker), Ubuntu dash (Docker), Alpine
  busybox ash (Docker). Full install path (`--version v0.22.0
  --no-skill --dir /tmp/shux`) green under Debian dash AND Alpine
  busybox ash, exercising dependency probe → platform detect →
  GitHub release download → SHA-256 verify → file install → PATH
  check. Error path (`GitHub API rate-limited`) also renders cleanly
  under dash.

**2026-05-13 — PR #33: default-deny plugin permission model (v0.20.0)**
- Goal: third (and last) plugin daemon FR identified in the parked
  conductor design review. Predecessors landed earlier today
  (#31 `event.publish` → v0.18, #32 `plugin.state.*` → v0.19).
- Council-first pass: wrote `docs/designs/permissions/README.md`,
  ran `dootsabha council --agents claude,codex` (gemini parked on
  `indrasvat/dootsabha#14`), iterated. Locked deltas captured in §9.
  Council caught the **predecessor-inheritance attack** before any
  code was written: keying grants/ownership on plugin name lets a
  reinstall under the same name inherit the predecessor's authority.
  Fix landed: per-install UUID is the primary key; name is a
  display-only by-name link to the UUID. `Pane.created_by_plugin`
  typed `Option<PluginId>` (UUID), not `Option<String>`.
- Five-tier sensitivity classification declared per-route:
  `Public` / `ContentRead` / `OwnedMutation` / `Grantable` /
  `PluginsForbidden`. New `RouterBuilder::register_with_policy` +
  startup `assert_every_route_has_policy()` panic if any route
  dodges classification. `events.watch` is parameter-aware
  (self-namespaced filters Public, broader filters ContentRead).
- Ownership auto-grant for `ContentRead`/`OwnedMutation`; explicit
  `shux plugin grant <name> <method>[--target <id>]` for the rest.
  `state.apply` Grantable (council carved it out of flat-deny so
  conductor v0.7 atomic multi-pane updates remain possible).
  `plugin.install|kill|reload|grant|revoke|grants|audit` flat-deny.
- Manifest `subscribes:` locked after first install — hot reload
  fails handshake if it widens the set without
  `shux plugin grant <name> <filter> --subscribe` first.
- Audit log: append-only NDJSON per plugin
  (`.shux/plugins/by-id/<uuid>/audit.log`), atomic writes, symlink
  rejection, 1 MiB rotation × 5 files. Audits BOTH router-bound
  RPCs AND plugin-only intercepts (`event.publish`,
  `plugin.state.*`) — the latter with reason `plugin_self_namespace`.
- Four new CLI verbs (`grant`/`revoke`/`grants`/`audit`) + matching
  RPC methods. Atomic `temp + rename` writes for grants.toml mirror
  the pattern from `plugin.state.set`.
- 6 new integration tests in `crates/shux-plugin/tests/permissions.rs`
  exercising deny→grant→allow, `plugins_forbidden` denial even with
  blanket grant, audit recording for plugin-only methods,
  manifest-subscribes lock across reload, UUID stability across
  kill+reinstall. Full suite: 747/747 green.
- Visual proof: `pages/screenshots/plugin-permissions-demo.png`
  shows a real audit entry from the `hello` plugin's `window.rename`
  call being denied with reason `no_grant_and_not_owned`.
- Doc-sync in lock-step: SKILL.md, references/plugins.md, PRD §13
  (security mitigations table + new T7–T10 threats), pages/index.html
  (plugins section trust-model paragraph).
- **All three plugin daemon FRs now landed. Conductor v0.1–v0.4 are
  unblocked.** The parked design lives at
  `feat/conductor-plugin-design`; v0.1–v0.3 (VT-poll watchdog,
  settle-snapshot archive, multi-pane notifications) don't need the
  permission model; v0.4 (worktree-per-pane) does and now can ship.

**2026-05-12 — PR #24: plugin DX paper cuts from the codex-exec dogfood loop (task 044a phase 0 follow-up)**
- Goal: close every practical gap codex-exec surfaces across repeated cold-context plugin-authoring dogfoods of PR #23 until a run completes with no perceivable friction. Loop = fix → manually verify → re-run codex with a different plugin idea & no hints → repeat.
- **Round 1 fixes (from v1 dogfood, score 7/10, gap = "had to grep Rust source"):**
  - `shux new <NAME>` positional form. Every doc shows `shux new my-session` but clap insisted on `-s <name>`. Added an `Option<String>` positional `name` field on the `New` subcommand; main.rs resolves `name.or(session)`. Tests: `test_m0_cli_new_positional_name` (e2e via daemon), `test_cli_parse_new_positional_name` + `test_cli_parse_new_positional_wins_over_flag`. Backwards-compatible — `-s` still works.
  - "Common plugin RPC calls" table added to `skills/shux/references/plugins.md`. Initial pass covered 14 methods.
- **Round 2 fixes (from v2 dogfood, score 4/10, three real findings):**
  - **Real product bug — `pane.exited.exit_status` was always `null`.** `run_pane_pty_task` in `crates/shux/src/main.rs` was breaking out of the read/write/resize loop and calling `handle.kill()` but **never reaping the child or propagating the exit code into the graph**. Fix: after the loop exits, wait the child with a bounded timeout (2s, then SIGTERM + 1s) and call `graph.set_pane_exit_status(pane_id, code).await` so the daemon's `PaneExited` event carries the real code. Verified manually: `sh -c "exit 42"` now fires `pane.exited` with `"exit_status":42`. The previous null was a silent correctness bug — plugins reacting to non-zero exits literally could not work.
  - **Schema table contained an inaccuracy.** Codex caught it: `window.list` returns a bare array of `{id, title, …}` objects, not the `{windows: [{window_id, …}]}` shape I'd documented. Audited every row of the table against the live daemon (`shux api <method> '<json>' | jq '.result | keys'`), corrected drift, and expanded each row to show the actual **result** shape (not just params) since codex needed both. Verified shapes: window.{list,rename,focus}, pane.{send_keys,set_size,snapshot,capture,split,kill}, session.{create,list,kill}, state.apply, events.history.
  - **New "Common gotchas plugins hit" subsection** in `references/plugins.md`: single-pane session auto-collapse on pane exit, `pane.send_keys` to an exited pane returning "pane VT not found", `pane.kill` rejecting the last pane in a window, exit_status being null vs numeric depending on destruction path, and the event payload double-envelope rule (`params.type` for routing, `params.data.data.*` for payload).
  - Codex skill registry synced at `~/.codex/skills/shux/` (cp -R of the updated tree).
- **Round 3 fixes (from v3 dogfood, score 8/10, verification passed, no Rust grep — two findings):**
  - **`window.created` event was missing `index`.** Plugin had to call `window.list` to recover position info that the bus already knew. Added `index: u32` to the `EventData::WindowCreated` variant; populated from `session.windows.len() - 1` in `create_session`, `create_window`, and both `stage_*` helpers. Verified the field appears in `events.history`.
  - **`shux window new` CLI was missing `--cwd` and `--cmd`.** The `window.create` RPC accepts both but the CLI didn't expose them, forcing plugin authors to drop to `shux api`. Added `--cwd <PATH>`, `--cmd <STRING>`, and trailing `-- <argv>` (mirroring `shux new`'s pattern). Three new parse tests.
- **Round 3.5 — hot reload (third dogfood in a row flagged `hot_reload_missed: yes` — the manual `kill+install` loop became the dominant friction once doc gaps closed):**
  - Added `notify`-based filesystem watcher to `shux-plugin`. `PluginSource.watch` (default true via `shux plugin install`, opt out with `--no-watch`) spawns a watcher on the source's parent directory (atomic-rename-tolerant for vim). On any modify/create/remove event hitting the watched filename, the watcher queues a debounced (250ms) reload that calls `PluginManager::reload(name)` — equivalent to `kill + install` of the same source. New cancellation flag (`watch_cancel: Arc<AtomicBool>`) ensures `kill` stops the watcher before a racing save spawns a respawn on an already-removed entry. `plugin.reload` RPC + `shux plugin reload <name>` CLI for manual ticks. `PluginInfo.watching` surfaces in `plugin.list`. Smoke test verified: edit `v1` → `v2` in source → daemon respawns in <1s with new manifest.
  - **Real product bug — `pane.exited.exit_status` was always `null`.** `run_pane_pty_task` in `crates/shux/src/main.rs` exited its select! loop and called `handle.kill()` but never reaped the child or propagated the code. Fix: bounded `handle.wait()` (2s, then SIGTERM + 1s) followed by `graph.set_pane_exit_status(pane_id, code).await`. Verified `sh -c "exit 42"` now fires `pane.exited` with `"exit_status": 42`.
- **Round 4 dogfood (score 8/10, verification passed, no Rust grep, no hot reload missed — one remaining friction):**
  - Codex flagged that "the docs should explicitly map session.create/session.rename/session.kill to shux new/shux rename/shux kill" because the CLI doesn't mirror the RPC namespace for sessions (top-level verbs vs `session.*`). Added a "CLI ↔ RPC namespace mapping" table to `references/plugins.md` covering every method plugins reach for. Lowest-effort fix possible — the structural CLI choice (sessions at top level, windows/panes nested) is deliberate ergonomics for humans.
- Wire-shape flatten (`data.data.*` → `data.*`) remains deferred to its own breaking-change PR — needs `test_036_events_watch.py` and any other event consumer updated in lockstep.
- **Round 5 — CLI consistency overhaul (greenfield, no back-compat) — LANDED.** Codex council May 2026 established the invariant `RPC namespace.method → CLI noun verb` (dots become spaces, no top-level shortcuts). Implementation:
  - Removed top-level `shux new/ls/kill/rename/attach/api/apply/wait-for/snapshot` from the `Command` enum. Replaced with namespaced forms: `shux session create/list/kill/rename/attach`, `shux rpc call <method> --params <JSON|@FILE|->`, `shux state apply <template>`, `shux pane wait-for`, `shux window snapshot`.
  - `WindowCommand::New` → `WindowCommand::Create` for symmetry with `window.create` RPC.
  - `shux` no-args TTY-guard: piped/non-TTY now prints structured JSON help instead of trying to attach (Codex council finding — piped scripts must not deadlock).
  - `shux session` accepts `ses` / `sess` aliases.
  - Updated agent_help (the "COMMAND → RPC METHOD MAP" + "TYPICAL AGENT WORKFLOW" + "DECLARATIVE WORKSPACES" + "REPLACES THESE TOOLS" sections in `crates/shux/src/cli.rs`) to use the new shape end-to-end.
  - Tests rewritten: 14 new clap parse tests for the namespaced forms + a rejection test that every removed top-level verb errors out cleanly. Integration tests in `m0_integration.rs` and `cli_integration.rs` updated to call the new verbs.
  - Docs synced: `README.md`, `skills/shux/SKILL.md`, `skills/shux/references/{api,plugins,templates}.md`, `skills/shux/examples/replace-tmux-workflow.md`, `docs/PRD.md` (§8.6 CLI ↔ API mapping rewritten), `pages/index.html` (every code-panel + replaces-table updated). Codex global skill at `~/.codex/skills/shux/` synced.
  - All CI green locally.
- **Round 6+ planned**: re-run cold-context codex dogfood with the new CLI shape to verify the friction is gone. Extensive automation tests using shux itself (not iTerm2) to expand the suite is queued as a follow-up (user request).

**2026-05-10 — PR 3a: `state.apply` + `shux apply <template.toml>` (task 030)**
- Goal: close the second-biggest agent-relevant gap vs tmux. Combined with PR 2a (events.watch), agents can now declare a workspace in one call and watch the entire lifecycle stream in. The "spawn an Agent Conductor workspace" use case that every tmux-wrapping orchestrator implements as shell scripts is now a single typed RPC.
- Council review (codex + gemini, log `/tmp/shux_pr3_council.json`) consumed before implementation. Codex's review reshaped the plan substantially:
  - **P0** Original plan had `state.apply { template: ... }` baking CLI grammar into the daemon API. Wrong shape. Daemon takes a generic `Op` delta; CLI lowers TOML into ops. Future SDKs / MCP servers / raw curl all use the same primitive.
  - **P0** Naive impl would call existing public mutation methods one-by-one and partial-commit on failure. Right shape: clone snapshot once, validate every op against the staged copy, commit ONCE if all validate, then publish all events with shared correlation_id.
  - **P0** Atomicity boundary needs to be honest. Graph mutation is all-or-nothing via ArcSwap. PTY spawn is OUTSIDE the transaction; rolling back launched subprocesses has its own side effects. Spawn outcomes per-pane in `BatchResult::spawn_results`; spawn failure does not roll back the graph.
  - **P1** Subscribers need to attribute event bursts to specific apply calls. Add `EventMetadata.correlation_id`.
  - **P1** `events.watch --session` would have to dereference the live graph (wrong for historical events). Add `session_id` + `window_id` to every pane-scoped EventData variant.
  - **P1** Folding 030 + 037 (optimistic concurrency surface) is "PR 2 all over again" — too much surface. Split: PR 3a = templates + apply; PR 3b = expected_version on every mutating RPC + bounded VersionConflict.
  - **P2** Existing bug: `create_session` emits `PaneCreated{command: Vec::new()}` while RPC then spawns PTY with the real command. Event lies. Fix: graph create APIs accept initial_command.
- Foundational commits (in order):
  - `ca64ead` — every pane-scoped EventData variant carries `session_id` + `window_id`.
  - `d66fded` — `EventMetadata.correlation_id: Option<String>` + `EventBus::publish_with_correlation()`.
  - `c03bd41` — staged-snapshot transaction primitive (`SessionGraph::apply_batch` + free `stage_*` helpers + `Op` enum + `BatchResult` + `BatchError` in new `crates/shux-core/src/apply.rs`). 5 unit tests including atomicity rollback.
  - (this commit) — `state.apply` RPC handler + `Direction` lowercase serde + event_to_json includes correlation_id when set.
  - `shux apply <template.toml>` CLI + new `crates/shux/src/template.rs` parsing PRD §10.3 TOML shape (`[session]` + `[[windows]]` + `[[windows.panes]]`) and lowering to ops with positional back-refs. `--dry-run` and `--watch` flags.
- Visual + headless test (`.claude/automations/test_030_apply.py`): 5/5 PASS.
  - A1 — `--dry-run` lowers TOML to ops without touching the daemon.
  - A2 — apply returns `✓ Applied apply-<uuid>`.
  - A3 — all batch events share the correlation_id.
  - A4 — `shux ls` confirms session committed.
  - A5 — apply with name conflict returns BatchError, NO partial commit, NO events.
  - Visual demo: 4 fresh screenshots in `.claude/screenshots/030_v1..v4_*.png` showing watcher + apply side-by-side; correlation_id rendered visibly with `jq`.
- 161 → 166 shux-core tests (5 new); 642 workspace tests pass via `cargo nextest run -E 'not test(test_tcp_auth_required)'`. Lint clean.
- Dash updated with new "PR 3a — task 030" section pointing at the screenshots.
- PR 3b (optimistic concurrency surface) and PR 2c (sampled pane.output with data-plane separation) remain queued.

**2026-05-10 — PR 2a: `events.watch` + `events.history` RPC + CLI (control-plane events)**
- Goal: ship task 036, the single highest-value gap vs tmux for AI agent orchestration. External agents can now subscribe to the daemon's typed event bus and react to lifecycle events in real time, replacing the polling-on-`pane.capture` pattern they were forced into.
- Council review (codex + gemini) consumed before any implementation, log at `/tmp/shux_pr2a_council.json`. Convergent findings drove the architecture:
  - **Drop `PaneOutput` from PR 2a entirely.** Putting raw PTY bytes in the main `EventBus` is a secret leak (replayable via `events.history`) AND a DoS vector for control events (high-volume `cat large.log` saturates the 4096-cap broadcast in milliseconds, drops lifecycle events for everyone). Deferred to PR 2c with proper data-plane separation.
  - **Subscribe FIRST, history SECOND, dedup by seq.** The race where a publish lands between snapshot and subscribe IS load-bearing, not overkill. Encoded in the daemon-side `events.watch` handler.
  - **Publish from inside `SessionGraph` methods**, not the `run_graph_loop` dispatcher. Otherwise non-RPC mutations later silently miss publishing. Adopted from day one.
  - Long-poll OK if the CLI immediately reissues without artificial sleep; surface `lagged: true` so clients know the stream dropped events.
  - **Include event seq in metadata from day one** (free side effect of `EventBus::publish` returning seq) — PR 2b (optimistic concurrency) can now ship without forcing clients to migrate the `events.watch` payload shape.
- Renamed `SessionGraph::publish` (snapshot swap) → `commit_snapshot` to free the verb for `EventBus::publish`. 1 fn def + 20 call sites.
- `SessionGraph::new_with_event_bus()` constructor; `fire(EventData)` helper. `fire(...)` calls land at the end of every successful mutation in: `create_session` (SessionCreated + WindowCreated + PaneCreated for the implicit window+pane — initial bug missed this until E3 visual test caught it), `destroy_session` (SessionKilled), `rename_session` (SessionRenamed), `create_window` (WindowCreated + PaneCreated for implicit pane), `destroy_window` (WindowKilled), `create_pane` (PaneCreated), `destroy_pane` (PaneExited with no exit_status), `set_pane_exit_status` (PaneExited with exit_status), `split_pane` (PaneCreated), `focus_pane` + `focus_pane_direction` (PaneFocused), `zoom_pane` (PaneZoomed). 14 fire sites total covering the agent-relevant lifecycle.
- New RPC handlers (`register_events_methods` in `crates/shux/src/main.rs`):
  - `events.watch { filter?, from_seq?, max_events?, timeout_ms? } → { events, next_seq, gap, lagged }`. Subscribe → history snapshot → drain subscription with timeout → dedup by seq.
  - `events.history { filter?, count? } → { events, current_seq }`. Wraps `bus.history_filtered`.
- New CLI: `shux events watch [--filter ...] [--from-seq N] [--limit N] [--timeout-ms N]` reuses one UDS connection in a long-poll loop, prints JSON Lines on stdout, prints `[STREAM_DEGRADED]` to stderr on `lagged: true` and `[GAP n]` on first-call gap. `shux events history [--filter ...] [-n N]` for backfill.
- 2 new shux-core unit tests (`test_lifecycle_events_fire`, `test_event_seq_consistency`); the latter caught a follow-on bug in my own test when I added the implicit fire calls (had to filter the subscription to compare like-for-like with history).
- L4 visual + headless test (`.claude/automations/test_036_events_watch.py`): 4/4 PASS.
  - E1 — `events.history` returns SessionCreated after `shux new`.
  - E2 — `events.watch --limit 2` blocks until both create + kill events arrive (real live stream).
  - E3 — `--filter pane.` correctly excludes session events; replays from history with `--from-seq 0` to dodge live-timing flakes.
  - E4 — Sequence numbers strictly monotonic across multiple mutations.
  - Visual demo: 5 screenshots showing iTerm window with `shux events watch` on the left and a driver shell on the right; events stream live as the driver does session.create → window.create → pane.split → kill (`036_v1` through `036_v5` in `.claude/screenshots/`).
- 631 tests pass via `cargo nextest run -E 'not test(test_tcp_auth_required)'` (the 1 skipped test is a pre-existing TOCTOU port-bind race in `shux-rpc::server::tests::test_tcp_auth_required` — unrelated to PR 2a, file an issue separately).
- Dash updated with new "PR 2a — `events.watch`" section pointing at the 5 fresh screenshots.

**2026-05-10 — PROGRESS.md sweep + dash retitle ("Road to shux 1.0.0")**
- Task table was significantly behind reality after the recent PR train (#3–#7). Audited every M1 task against the codebase and recent commits, then updated:
  - Marked Done: **018** (Tier-1 keybindings, shipped with 017 attach), **019** (Prefix system, shipped with 017), **020** (mouse, 2026-05-08 spike), **021** (copy mode, PR #7), **022** (TOML config + `config validate` PR #4), **023** (live reload, 2026-05-08), **026** (status bar + script-driven segments, 2026-05-09), **029** (Mode 2026 sync output, in compositor since task 009), **033** (help overlay, PR #6).
  - Marked Partial: **024** (theme — only border + status-bar overrides shipped per PR #5; full token cascade pending), **028** (cap negotiation — TERM_PROGRAM/COLORTERM/SHUX env vars set, but no real DA2/XTVERSION query).
  - Still Pending: **025** (per-pane theming), **027** (pane titles), **030** (session templates + `shux apply`), **031** (keybinding config + conflict detection), **032** (command palette), **034** (M1 quality gate).
- Updated each affected task file's `Status:` field with concrete evidence (PR # / spike date / verification script).
- Refreshed Current Phase header to reflect the M1 ~75% state and to call out the M2 agent-relevant tasks (036 events.watch, 037 optimistic concurrency / state.apply) as the next focus.
- Renamed `.claude/screenshots/index.html` from "shux task 017 — visual test screenshots" → **"Road to shux 1.0.0"**. Added a roadmap section at the top of the dash that mirrors the milestone state, so the live page (http://indrasvat.tailc7ec16.ts.net:8721/) tracks the journey to v1.0.0 instead of one task. Existing 017 screenshot sections preserved beneath as "M1 milestone evidence".
- No code changes. PR plan: this sweep ships first, followed by focused PRs for events.watch + optimistic concurrency (036+037), session templates + `shux apply` (030), pane titles (027), and command palette + keybinding config + M1 quality gate (032+031+034) — in that order, prioritised for "agent-first multiplexer that beats tmux".

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
| 018 | Tier 1 keybindings (bare keys) | M1 | **Partial** ³ | 017 |
| 019 | Prefix key system (Tier 2) | M1 | **Done** | 018 |
| 020 | Mouse support | M1 | **Done** | 017 |
| 021 | Copy mode | M1 | **Done** | 019 |
| 022 | TOML config system | M1 | **Done** | 012 |
| 023 | Live config reload | M1 | **Done** | 022 |
| 024 | Theme engine and token system | M1 | **Partial** ¹ | 022 |
| 025 | Per-pane theming | M1 | Pending | 024, 017 |
| 026 | Status bar (hardcoded, pre-plugin) | M1 | **Done** | 025 |
| 027 | Pane titles (manual + auto) | M1 | Pending | 015 |
| 028 | Capability negotiation (ClientCaps) | M1 | **Partial** ² | 010 |
| 029 | Synchronized output (Mode 2026) | M1 | **Done** | 028 |
| 030 | Session templates | M1 | **Done** ⁵ | 022, 015 |
| 031 | Keybinding configuration and conflict detection | M1 | **Partial** ⁶ | 019, 022 |
| 032 | Command palette | M1 | Pending | 019, 031 |
| 033 | Help overlay (keybinding cheat sheet) | M1 | **Done** | 032 |
| 034 | M1 integration and quality gate | M1 | Pending | 013–033 |
| 035 | Complete JSON-RPC API surface | M2 | Pending | 034 |
| 036 | Event stream (events.watch) | M2 | **Done** ⁴ | 035, 007 |
| 037 | Optimistic concurrency and ensure operations | M2 | Pending | 035 |
| 038 | Plugin host: wasmtime integration | M2 | Pending | 034 |
| 039 | Plugin permissions and sandbox | M2 | Pending | 038 |
| 040 | Plugin WIT host functions | M2 | Pending | 039 |
| 041 | Plugin lifecycle and hot reload | M2 | Pending | 040 |
| 042 | Event interception chain | M2 | Pending | 041, 036 |
| 043 | Command override system | M2 | Pending | 041 |
| 044 | Process plugin protocol | M2 | Pending | 041 |
| 044a | Process plugins v0 (Pi-style DX, WASM-free) — phase 0 done | M2 | **Done (phase 0)** | 035, 036 |
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
| 061 | Render parity and mouse copy UX | M1/M3 | **Done** | 017, 020, 021 |
| 062 | Scrollback-backed copy mode | M1 | **Done** | 005, 021, 061 |
| 063 | Session save and restore | M1/M3 | **Done** | 013, 014, 015, 030 |
| 066 | Lossless pane output recording | M2 | **Done** | 036 |
| 067 | shux-vt resize reflow | VT Quality | **Done** | 005, 016, 066, 073 |
| 068 | shux-vt wide-cell invariants | VT Quality | **Done** | 005, 067, 073 |
| 069 | shux-vt grapheme-aware cell storage | VT Quality | **Done** | 005, 068, 073 |
| 070 | shux-vt DEC special graphics charset | VT Quality | **Done** | 005, 068, 073 |
| 071 | shux-vt real tab-stop state | VT Quality | **Done** | 005, 073 |
| 072 | shux-vt origin mode and scroll-region semantics | VT Quality | **Done** | 005, 029, 073 |
| 073 | shux-vt corpus regression harness | VT Quality | **Done** | 066 |
| 074 | shux-vt dirty-region tracking | VT Quality | **Done** | 005, 073 |
| 075 | Plugin DX v0.5 and OCP extraction | M2 | **Done** | 044a |
| 076 | Sightline TUI QA plugin | M2 | **Done** | 075 |
| 077 | shux lens — give every agent eyes (P0: fixtures + red suite; P1: ContentRevision substrate; P2: pane.glance; P3: pane.wait_settled) | M3 | **Partial** (P0, P1, P2 done; P3 implemented — gate 21/16, S1–S5/V1 green; `s1_ready.png` golden PROVISIONAL pending QA/council ratification; P4–P6 pending) | 016, 017, 060, 064, 074 |

---

¹ **Task 024 Partial:** `[theme]` config section overrides border colors (focused/unfocused) and status-bar fg/bg colors with hot reload. The full PRD §6.1 token cascade (per-pane themes, theme files in `~/.config/shux/themes/`, ANSI palette overrides, named theme references) is still pending — close-out lives with task 025 and the M1 quality gate (034).

² **Task 028 Partial:** daemon claims the best installed multiplexer-compatible `TERM` (`tmux-256color` preferred, then `screen-256color`, then `xterm-256color`), `TERM_PROGRAM=shux`, `TERM_PROGRAM_VERSION=<pkg ver>`, `COLORTERM=truecolor`, `SHUX=1` on every PTY spawn. Real cap negotiation (DA2 / XTVERSION / Kitty keyboard query / OSC 4 palette probe stored as a per-client `ClientCaps` and gating Mode 2026, OSC 8, OSC 52, true color) is still pending.

⁶ **Task 031 Partial:** attach-time keybinding config is implemented: configurable prefix, root/prefix override tables, action validation, and config-validator diagnostics. The runtime `keybinding.list/set/reset` RPC surface, reserved-key policy, and plugin provenance/conflict model are intentionally deferred.

³ **Task 018 Partial** (caught by Codex review of PR #8): `attach.rs::key_to_bare_action` ships Alt+Enter, Alt+|/\\, Alt+-, Alt+arrows, Alt+z, Alt+x, Alt+Tab. Per PRD §9.1, **bare Alt+h/j/k/l, bare Alt+n/p, and Alt+1..9 are still missing** as Tier-1 bindings (the hjkl/n/p variants today only work after the Ctrl+Space prefix). Small follow-up — one match arm in `key_to_bare_action`. Lives with the M1 quality gate (034) or a focused 018-followup PR.

⁴ **Task 036 Done (control plane only):** `events.watch` and `events.history` RPC methods + `shux events watch` / `shux events history` CLI subcommands + 14 lifecycle event publish sites in `SessionGraph`. **`PaneOutput` events are explicitly out of scope for this task** — Codex+Gemini council review identified that putting raw PTY bytes on the main `EventBus` is both a secret leak (replayable via `events.history`) and a DoS vector for control events (saturates the broadcast channel under `cat large.log`). Sampled `pane.output` notifications with proper data-plane separation are queued as PR 2c (a follow-up "036b"). Optimistic concurrency on mutating RPCs lives with task 037 (PR 2b — version stamps already in event metadata so PR 2b is small).

⁵ **Task 030 Done:** generic `state.apply` RPC + `shux apply <template.toml>` CLI + the foundational pieces — every pane-scoped EventData variant carries `session_id` + `window_id`; `EventMetadata.correlation_id` for batch attribution; staged-snapshot transaction primitive (`SessionGraph::apply_batch`) with graph-level all-or-nothing atomicity; `Op` enum with `SessionRef::BackRef` / `PaneRef::BackRef`; PRD §10.3 TOML template parser. Includes the codex P2 #10 fix: `create_session` / `create_window` accept `initial_command` so `PaneCreated` events stop lying about empty command. **Optimistic concurrency surface across all RPCs** is split into task 037 / PR 3b per codex's "PR 2 all over again" warning. Atomicity boundary documented: graph mutations all-or-nothing; PTY spawn happens after commit and surfaces per-pane outcomes in `BatchResult::spawn_results` (codex P0 #1: spawn failure does not roll back the graph because killing already-launched subprocesses has its own side effects).
