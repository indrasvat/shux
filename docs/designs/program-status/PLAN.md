# Program status (OSC 7501) — implementation plan

Status: **r3**, revised after council round 1.
- codex: REJECT, 22 findings.
- grok: APPROVE-WITH-CHANGES, 13 findings.
- Every finding's disposition is in `council-review.md`.

Spec: Program Status Protocol v0.4 (2026-10-10),
<https://superlogical.com/rex/docs/build/program-status>

Landed on this branch:
- `4fba7d1` — the protocol crate.
- `576fd90` — tier-1 fuzzers.
- `44f487b` — a padding bug the fuzzer found.

## 1. Problem

Programs can say what they are doing with one OSC on the pty they already
write to. The states are `idle`, `working`, `done`, `blocked` (with a
permission / question / auth kind), and `error`.

Who emits it:
- Claude Code ≥ 2.1.295, Pi, Crush and Grok Build emit it today.
- Codex has it on main and emits it without probing.

shux breaks it for every pane:
- A sender probes with `OSC 7501 ; ? ST` followed by `CSI c`.
- shux answers only the DA1 (`parser.rs:1593`) and drops 7501 as an
  unhandled OSC (`parser.rs:1918`).
- The sender concludes "unsupported" and stays silent.
- Codex, which does not probe, has its reports discarded.

Measured on this machine (2026-10-26):

| Where the probe runs | Reply | Result |
|---|---|---|
| Orca 1.4.241 (outer) | `ESC]7501;?:states=…:kinds=… ESC\` then DA1 | Claude Code 2.1.311 emits idle → working → done(msg) → clear |
| Ghostty 1.3.1 (outer) | DA1 only | silent |
| any pane inside shux | DA1 only | silent; the outer terminal never hears either |

shux's own agent tooling guesses state by scraping the screen. The conductor
polls `pane.capture` every 2 s (`plugin.sh:49`) and greps for spinners
(`plugin.sh:103`).

## 2. Goals / non-goals

Goals
- **G1** Answer the probe and hold per-pane records exactly as the spec
  requires.
- **G2** Expose status as typed, sequenced shux state:
  - the `pane.status` and `pane.wait_status` RPCs (ContentRead);
  - the `pane.status_changed` event (text-free summary);
  - CLI verbs.
- **G3** Show it as a border badge and a status-bar count, identical on live
  attach and on every snapshot path.
- **G4** Let agents wait on status. The conductor prefers it over scraping,
  only observes, and never auto-answers.
- **G5** Report each pane's status to the terminal the user attached from.
- **G6** Keep it one isolated module behind a config kill-switch. Disabled
  means today's behaviour: no probe reply, no records, no chrome, no
  forwarding.

Non-goals
- OSC 9;4. shux does not implement it.
- Desktop notifications.
- Injecting OSC 133 into user shells.
- Authenticating which process wrote a report. Nothing on a pty can do that;
  D8 is a heuristic.

## 3. Prior art (code read, not docs)

Implementations read:
- Ghostty / libghostty-vt `program_status.zig` @68640f3
- kitty `program_status.py` @02dd29a
- tuios `progstatus/` and `host_program_status.go` @aaaa128. tuios is a
  multiplexer.
- tmux fork 4a77824ede68
- cmux
- SwiftTerm `osc7501-delta.md`
- senders: Claude Code 2.1.311, Codex #52725/#54114, pi

| # | Decision | From | Where |
|---|---|---|---|
| D1 | Parse the whole report, validate, then apply. Limits are checked on every occurrence of a duplicate key. | Ghostty, kitty, SwiftTerm | crate ✅ |
| D2 | A malformed `id` discards the report; a later valid id does not rescue it. | Ghostty #14790 | crate ✅ |
| D3 | Strict base64: padding optional but complete if present, zero trailing bits; length checked before decode; UTF-8, C0/DEL/C1 checked after. | tuios, Ghostty | crate ✅ (padding fix found by fuzzing) |
| D4 | Fixed reply bytes with the states/kinds lists, mirroring the query's terminator, in stream order with DA1, only when enabled. | Ghostty, tmux-fork | §5.1 |
| D5 | Least-recently-*reported* eviction. | kitty | crate ✅ |
| D6 | blocked > error > done > working > idle, then root, then newest. | kitty, tuios | crate ✅ |
| D7 | Prompt start / process exit drop working, blocked and idle. RIS clears; DECSTR does not. | kitty, tuios | crate ✅ |
| D8 | A foreground-process-group heuristic drops a dead reporter's records. It fails safe. | tuios | §5.9 (P2) |
| D9 | Exit cleanup runs after the final output has been parsed. | SwiftTerm | §5.3 |
| D10 | Toward the outer terminal shux builds its own per-pane reports under a per-attach namespace. It never passes program reports through. | tuios | §5.8 |
| D11 | Disarm bidi and invisible characters before shux shows text. | tuios | crate ✅ |
| D12 | A sticky `seen` flag, reset by RIS, tells "never reported" apart from "reported, then cleared". | tuios | §5.1 |
| D13 | Status is ContentRead, never Public: `kind=auth` alone tells a plugin the user is at a credential prompt. | grok F6 | §5.5 |

Spec ambiguities, resolved:
- **Invalid earlier duplicate:** discard if it breaks a limit. The unknown
  `state` rule applies to the last value only (`state=nope:state=working` →
  working).
- **`id` total:** 128 bytes.
- **`idle` on prompt or exit:** dropped.
- **Prompt marks:** only `133;A`.
- **Reply terminator:** mirrors the query's.
- **No reply when disabled.**
- **Case:** lowercase only.
- **Empty text or app:** treated as absent.

## 4. vte 0.15 facts (verified in the vendored source)

1. **OSC buffer.** It is shux's `MAX_OSC_PAYLOAD_BYTES` = 4096 const generic
   (`parser.rs:134`, `VtParser` at `:150`; vte's own default is 1024). It is
   enforced only because the workspace builds vte without `std`
   (`Cargo.toml:40-44`).
   - It counts *retained* bytes.
   - C0 bytes inside an OSC are dropped and not counted
     (`advance_osc_string`), so the retained length is **not** the wire
     length (codex F2).
2. **Termination.** CAN, SUB and *any* ESC dispatch the OSC with
   `bell_terminated = false`.
   - A clean `ESC \` then calls `esc_dispatch(0x5C)`.
   - `ESC [`, `ESC ]`, `ESC P`, `ESC X`, `ESC ^` and `ESC _` move state
     **without any callback**.
   - C0 bytes in `Escape` execute and stay in `Escape`.
   - So `ESC]7501;… ESC]0;t ESC\`, `… ESC _G… ESC\` and `… ESC[ ESC\` all
     look properly terminated to `Perform` (codex F1, grok F1, reproduced
     against vte `lib.rs:340-446`).
3. **Params.** `;` splits params, and only 16 are kept.

Consequence: OSC 7501 and OSC 133 framing is decided from **raw bytes**, the
same way shux already decides APC framing (`graphics/apc.rs`).

## 5. Architecture

### 5.1 Shape and seams

```
                  pure, no I/O
 ┌──────────────────────────────────────┐
 │ crates/shux-program-status    ✅      │  wire · model · table · rank · summary · display
 └──────────────────────────────────────┘
        ▲            ▲              ▲             ▲
 shux-vt│     shux-core│         shux-ui│        shux│
 raw-byte scanner  Pane fields    PaneChrome     publisher task, RPCs, CLI,
 seen, generation  setter+event   badge          config, reload, forward, D8
```

The leaf crate is confirmed by both reviewers (Q1). `shux-core` and `shux-ui`
need `StatusSummary` without a `shux-core → shux-vt` edge.

**VT seam** (`crates/shux-vt/src/program_status.rs`, new):
- **`Osc7501Scanner`**, a sibling of `ApcScanner`, runs over each raw batch
  in `process_with_responses`.
  - States: `Ground` / `Esc` / `Osc { body, raw_len, poisoned }` / `OscEsc`.
  - It mirrors vte's string grammar: `ESC ]` opens an OSC from any state; BEL
    or `ESC \` finishes it; CAN, SUB, or ESC followed by anything else aborts
    it; C0 bytes count toward `raw_len` but are not stored.
  - `raw_len` counts the whole sequence, introducer and terminator included.
    Past 4096 the sequence is poisoned and dropped, and scanning continues
    so it resynchronises.
  - State persists across reads.
- **Cuts.** A finished body that starts `7501;` or `133;A` becomes a cut one
  past its terminator.
  - Cuts merge with APC cuts by `end`, and the batch is fed to vte in slices,
    exactly as for APC.
  - At a 7501 cut, `wire::decode` runs, then either
    `push_response(query_reply(term))` or `table.apply`.
  - At a 133;A cut, `table.on_prompt_start()`. An aborted 133;A therefore
    never drops live records.
- **Ordering.** Replies come in stream order against DA1: bytes before the cut
  reach vte first, and `CSI c` after the cut is answered after the 7501
  reply. Tested in one batch and split at every byte boundary.
- **vte dispatch.** vte's own `osc_dispatch` for `7501` and `133` returns
  immediately.
- **RIS** (`esc_dispatch 'c'`) → `on_full_reset()`, `seen = false`.
- **State.** `VirtualTerminal.program_status: Option<VtProgramStatus { table,
  seen, generation }>`, where `None` means disabled.
  - The scanner tracks framing even while disabled, so enabling it in the
    middle of a sequence cannot misparse.
  - Its cuts are inert while disabled.
- **Generation.** A process-global monotonic `u64` (one `AtomicU64`),
  stamped on every observable change: table revision, enable, disable,
  `seen`, VT replacement. It never resets, so stale publications from older
  incarnations, or from before a disable, are recognisable.
- **Not presented state.**
  - It is outside `Presented` and DEC 2026, and never bumps
    `content_revision` (`osc_4_palette_no_bump` precedent).
  - It stays live across alt-screen switches and synchronized output.
    Asserted.
- **API.**
  - `set_program_status_enabled(bool)`
  - `program_status_publication() -> StatusPublication { generation, summary,
    seen }`
  - `program_status_records()`
  - `note_process_exit()`
  - `set_status_origin(u64)` (D8)

**Graph seam** (`shux-core`):
- **Pane fields.** `Pane.program_status: Option<StatusSummary>` and
  `program_status_seen: bool`, both `#[serde(default)]`. Templates never carry
  them, because `session_persist` exports `ExportPane`
  (`session_persist.rs:43-52`).
- **`set_pane_program_status(pane, StatusPublication)`:**
  - rejects generations older than the pane's last one;
  - fast-paths an equal `(summary, seen)`;
  - fires `PaneStatusChanged { pane_id, window_id, session_id, old, new,
    seen }`, wired as `pane.status_changed`. This adds the variant, the
    name and the JSON mapping (`event.rs:453-475`).
- **Text-free summary.** `StatusSummary` (crate, landed) has no `title`,
  `msg` or revision (crate test `summary_never_carries_program_text`).
- **Event visibility.** `events.watch` broad subscriptions are already
  ContentRead (`surface_tests.rs:26`), so the event does not widen exposure.

### 5.2 Publication path (codex F5, F6, F20; grok F11)

```
pane PTY task ──(io lock)──► per-pane watch::Sender<StatusPublication>
reload walker ──(io lock)──┘           │ send_replace: latest wins, never awaits
VT replacement ─(io lock)──┘           ▼
                       publisher task (one per pane)
                         loop { rx.changed().await;
                                graph.set_pane_program_status(latest).await;
                                sleep(100 ms) }        ← ≤ 10 commits/s/pane
                                       ▼
             graph: reject stale generation · dedupe equal · fire event
```

- **Sampled, not every transition.** An event is the post-batch projection,
  sampled at the existing 100 ms output cadence (`pane_spawn.rs:67`).
  - A `blocked` that lasts less than one interval inside one read may never
    be published. The event and `wait-status` docs say so.
  - Lifecycle publications (exit, VT replacement, disable) skip the sleep.
- **Ordering.** Every publication carries a generation, so arrival order does
  not matter and stale state cannot resurrect.
- **Locking.** `send_replace` is synchronous, so nothing awaits under the io
  lock. This is the PR #7 rule (`pane_spawn.rs:217-220`).
- **Lifetime.** The publisher ends when its sender drops. The sender lives in
  `PaneIoState.status_tx`, next to `revisions`.

### 5.3 Lifecycle (codex F7, F8; grok F8)

- **Natural termination.** This covers every PTY-task exit except
  `RequestedTeardown`: EOF, read error, response-write failure, and flush
  failure.
  - In the common epilogue, after the final batch is parsed (D9), the task
    calls `note_process_exit()` and `send_replace`. It then awaits the
    publisher's ack before `set_pane_exit_status`.
  - So `pane.status_changed` precedes `pane.exited`.
  - The task captures the VT's incarnation id at spawn and never touches a
    replaced VT.
- **Destroy** (`pane.kill`, window/session destroy).
  - The graph removes the pane and fires `PaneExited` first
    (`rpc/pane.rs:384-389`, `graph.rs:783/1005/1305`). That order is
    unchanged.
  - Contract: `pane.exited` ends the pane's status, and no
    `pane.status_changed` follows. Late publications hit `PaneNotFound` and
    are dropped.
- **VT replacement / respawn.** Every `vts.insert` that replaces a VT
  (`pane_spawn.rs:605`, the `pane_io.rs` sites) publishes `{generation, None,
  seen: false}` under the same lock, before the new task runs.
  - The graph never shows a dead program's summary next to an empty
    `pane.status`.
- **Kept VT after exit.** After EOF the VT stays for capture
  (`pane_spawn.rs:413-418`). `done` and `error` records stay with it, which
  is the spec's intent.

### 5.4 Config and reload (codex F6; grok F5)

```toml
[program_status]
enabled     = true    # master: false = no reply, no records, no chrome, no forward
border      = true    # badge in pane borders            (P2)
status_bar  = true    # "N waiting" segment              (P2)
stale_guard = true    # D8 heuristic, fails safe         (P2)
forward     = "auto"  # outer terminal: auto | off       (P4)
```

- **Enable flag.**
  - `PaneIoState.program_status_enabled` is authoritative.
  - VT insertion applies it under the same lock, so there is no
    create-before-walk race.
  - The walker seeds it from `DAEMON_CONFIG` (`pane_spawn.rs:445`). Where
    that static is unset, outside the daemon, the schema default applies.
- **Walker loop.**
  - It arms `notified()` before reading `current()`, because the handle uses
    `notify_waiters` (`config.rs:242`).
  - Under the io lock it flips the flag, toggles each VT (bumping the
    generation) and calls `send_replace`.
  - No graph await happens under the lock.
  - Re-enabling starts empty, with `seen = false`.
- **Strict validator.** `config_validate.rs` has its own
  `deny_unknown_fields` schema (`:37`). The table goes there too; otherwise a
  valid block fails `shux config validate`. `shux config init` emits the
  block.
- **Pre-existing defect, fixed in P1.**
  - The hot reload calls `read_config`. On *any* parse error that replaces the
    live config with `Config::default()` (`config.rs:270-279`, used at
    `:366`).
  - So one typo while the daemon runs silently resets the prefix key, the
    theme, and everything else.
  - The fix: a hot-reload parse failure keeps the previous config and logs.
    Startup keeps the default-on-failure behaviour. Red-first test.
- **No cargo feature.** The runtime switch covers "disable", and §8 makes
  removal mechanical.

### 5.5 RPC and CLI (D13)

- **`pane.status`** (`ContentRead`):
  - returns the full `RecordView` list, plus the generation and `seen`;
  - reads atomically from the VT under the io lock;
  - is the **only** RPC that returns `title` or `msg`.
- **`pane.wait_status`** (`ContentRead`):
  - Params: `pane`, `state: [..]`, `match: top|any` (default `top`),
    `timeout_ms`.
  - It subscribes first, then reads the graph summary, then waits. This is
    the subscribe-before-history pattern (`rpc/events.rs:90-106`): no lost
    wakeup, and no hang when the pane already matches.
  - On a `gap` or `lagged` signal it re-reads.
  - Outcomes: `matched {summary}` | `timeout` | `pane_gone` | `disabled`.
    Error data carries `data.replacement`.
- **`pane.list` does not change.** No status on any Public surface (grok F6).
  Chrome reads the graph inside the daemon.
- **Surface pin.** Both methods go into `EXPECTED_SURFACE`
  (`surface_tests.rs:24`).
- **CLI.**
  - `shux pane status [--pane P] [--json]`
  - `shux pane wait-status --state blocked,done [--match any]
    [--timeout 30s]`
  - Both are thin wrappers using `style::print_pane_status`. Text output
    goes through `sanitize_for_display`.

### 5.6 Presentation (codex F18; grok F9, Q4)

- **`PaneChrome`.** A shared descriptor, `PaneChrome { title, badge:
  Option<StatusBadge { glyph, role }> }`.
  - It is built once from `&Pane` and config.
  - Both title renderers consume it: `shux-ui/src/compositor.rs:530` and
    `shux-ui/src/composed.rs:116`.
  - It replaces the duplicated title maps in `attach.rs:1373` and
    `snapshot.rs:193`.
  - Live attach and every snapshot path are identical by construction. The
    cross-path test compares cells, colours included.
- **Badge table.**

  | state | Unicode | Nerd Font | role |
  |---|---|---|---|
  | blocked | `◆` | U+F071 | accent |
  | error | `✕` | U+F00D | error |
  | done | `✓` | U+F00C | ok |
  | working | `◐` | U+F110 | muted |
  | idle | `○` | `○` | muted |

  - Set selection follows `appearance.nerd_fonts`.
  - The glyph's `unicode-width` plus one space is charged to the 64-column
    title budget (`model.rs:642`) *before* clamping, so the shux-owned name
    is never pushed off.
  - Every glyph is checked for presence in the bundled JetBrains Mono NF.
  - The badge is coloured by its role. The title keeps the focused or
    unfocused border colour. No ANSI goes into title strings.
- **Status bar.** One segment in `statusbar_build::build`, which attach and
  snapshot share: "◆ 2 waiting", shown only while a pane is `blocked`, and
  dropped first at narrow widths.
- **No acknowledgement dimming.** Snapshots have no focused client, so dimming
  would break attach/snapshot equivalence. Records leave only by replace,
  clear, the D7 lifetimes, D8, or RIS.

### 5.7 Conductor (codex F16, F17; grok F6)

- It reads `pane.status` and `pane.wait_status`. Grants match by method
  (`permissions.rs:171`), so it needs `shux plugin grant conductor
  pane.status` and `pane.wait_status`. These go into the `plugin.sh` setup.
  A permission test covers: denied with only `pane.capture` granted; allowed
  on owned panes.
- A pane with `seen == false` falls back to scraping. A pane with `seen ==
  true` is never scraped.
- A reported `blocked`, of any kind, only notifies and badges. It **never**
  goes through `AUTO_DISMISS`.

### 5.8 Outer terminal (P4) (codex F11–F14; grok F2, F10)

- **Probe before hello.**
  - Raw mode currently starts only after `AttachReady`
    (`shux-ui/src/attach.rs:93-156`). So the client enters a scoped raw guard
    first.
  - It writes `OSC 7501;? ST` and `CSI c`, then polls stdin with its own
    300 ms deadline; DA1 is the sentinel.
  - Non-reply bytes are kept as pending input. The guard then restores the
    terminal.
  - `forward = "off"` skips the probe entirely.
- **Capability.** `AttachHello.program_status: bool`, `#[serde(default)]`,
  mirroring `graphics` (`shux-rpc/src/attach.rs:39`).
- **Namespace.** `shux-<attach8>`, with a random token per attach connection.
  - Pane records are `shux-<attach8>/<pane32>`, where `<pane32>` is the
    pane UUID's 32 hex digits.
  - The session aggregate is the `shux-<attach8>` record itself.
  - At most 64 records, the aggregate included.
- **No program text leaves shux.** Records carry state, kind, progress and
  app. `title` is the shux pane display title after `sanitize_for_display`.
  `msg` is never forwarded.
- **Emission.**
  - Changed records only, batched every 250 ms, through a per-connection
    emitter appended to the shipped frame (the `kitty_emit` model).
  - Pane removal is learned from graph `PaneExited` events and sends
    `clear:id=shux-<attach8>/<pane32>`.
  - Detach or disable sends `clear:id=shux-<attach8>`.
  - After each removal the emitter reconciles against the desired set.

### 5.9 Stale guard (D8, P2, default on, fails safe) (codex F9, F10; grok F3, F4)

- **Origin tags in the crate.** An opaque per-entry `origin: u64`.
  - `set_origin(tag)` applies to subsequent reports.
  - An identical re-report refreshes the tag.
  - `drop_origin(tag)` removes only the working, blocked and idle entries
    carrying that tag. `on_process_exit` stays PTY-EOF only.
- **Sampling.** Before each read batch is fed to the VT, the daemon samples
  the foreground process group with `tcgetpgrp` on the master fd. tmux does
  the same in `osdep-darwin.c` and `osdep-linux.c`; this is measured on both
  platforms in tests. It then calls `vt.set_status_origin`.
  - pgid ≤ 1 → tag 0, never swept.
  - The shell as foreground group (no job control) → tag 0.
  - Sampling happens only while `stale_guard` is on.
- **Sweep.** A separate timer task, not a branch of the biased `select!`,
  runs every 2 s.
  - It collects the distinct non-zero tags of ephemeral entries.
  - Only `ESRCH` from `kill(-pgid, 0)` counts as gone; it then calls
    `drop_origin` and publishes.
  - `EPERM` keeps the records.
- **Documented gaps.** Background writers, shells without job control,
  `setsid` children, and pgid reuse within 2 s. All fail safe: records are
  kept.
- **Why default on.** Without it, the P2 badge sticks for every shell that
  emits no OSC 133, which is the default.

## 6. Phases → PRs

Every PR carries:
- `make check`;
- a red-first test for every defect;
- the applicable QA gate with `VERDICT: PASS`;
- adversarial review, `shux-simplify-architect` (more than 200 added lines),
  and a council on the diff before the first push;
- `gh-ghent` after the push.

| PR | Scope | QA gates | Matrix |
|---|---|---|---|
| **P1 core** | scanner (7501 + 133;A) + VT seam, RIS, `seen`, generation; `enabled` + walker + strict schema + hot-reload keep-previous fix; publisher; lifecycle epilogue + VT-replacement clear; graph fields + event; `pane.status` / `pane.wait_status`; CLI; tier-2 fuzz crate | `shux-vt-solid-qa` (parser, terminal responses) with tracked `SOLID-QA.md` and `make check-vt-qa`; **and** `shux-tui-qa` for the CLI text/`--json`/event surface | `events.watch` + RPC + CLI × default · init · disabled · malformed · hot-reload on real colour-probed panes; dogfood with Claude Code + Codex. No chrome: border and snapshot cells are declared unaffected, and pixel-identity with `main` is asserted |
| **P2 chrome** | `PaneChrome` + both renderers, badge table, status-bar segment, `border` / `status_bar`, D8 (origin tags, `shux-pty` pgrp fn) | `shux-tui-qa` (+ VT gate if raster is touched) | attach · pane/window/session.snapshot × default · init · feature-maxed · disabled · malformed · hot-reload, both font sets. Web preview: not present in the tree, declared |
| **P3 agents** | conductor on `pane.status` with grants, observe-only; skill docs | `shux-tui-qa` (agent workflows) | dogfood: Claude Code + Codex under conductor |
| **P4 forward** | client probe, hello bit, namespaced emitter, clear rules | `shux-tui-qa` **and** `shux-vt-solid-qa` | outer: Orca (supports it) and Ghostty 1.3.1 (does not, and must stay clean); `make test-gui-terminal` for the unsupported receiver |

## 7. Verification

- **Crate** ✅
  - Unit tests for every D-rule and limit.
  - Tier-1 fuzzing (§7.1).
- **Scanner.**
  - BEL and ST finish a sequence.
  - These interruptions abort, each one also split at every byte boundary:
    CAN; SUB; `ESC [`; `ESC _`; `ESC P`; `ESC X`; `ESC ]`; ESC-ESC; C0 in
    `Escape`; `ESC [ ESC \`.
  - A sequence over the limit made entirely of NULs is poisoned, and the
    scanner resyncs after poison.
  - Non-7501 OSCs are untouched.
- **VT.**
  - Reply bytes are checked for ST and for BEL.
  - Probe then DA1: the 7501 reply comes first, both in one batch and at
    every split point.
  - Disabled: only DA1, byte-identical to `main`.
  - RIS clears records and resets `seen`.
  - DECSTR (`CSI ! p`) keeps records. This is pinned even though no DECSTR
    handler exists.
  - `state=nope:state=working` → working.
  - 133;A drops working, blocked and idle.
  - Status survives the alt screen and DEC 2026 while the title and grid stay
    frozen.
  - No `content_revision` bump.
- **Reload defect.** A malformed edit during a live session keeps the
  previous config. Red against `main` first.
- **Daemon integration.** Real panes, colour-probed output, an isolated
  `XDG_RUNTIME_DIR`, and `no_leak_guard`. The sync point is `pane.status`
  generation polling with a timeout, never a title barrier (codex F19).
  - printf reports → `pane.status` and `pane.status_changed`.
  - Natural exit → `status_changed` then `exited`.
  - `pane.kill` → `exited` only.
  - Respawn → summary cleared.
  - A python probe inside a pane sees `?…` before DA1.
  - Rapid hot-reload off/on/off: the final state wins, with no resurrection
    under an injected publisher delay.
  - Concurrent spawn during a reload.
  - A 10k tiny-write flood stays at or under 10 commits/s while other RPCs
    stay responsive.
  - `wait_status`: already matching / transition during subscribe / timeout
    / pane gone / disabled.
  - Permission tests for both RPCs.
- **Cross-path (P2).** Attach-frame cells equal `window.snapshot` cells,
  colours included.
- **Kill-switch.** Feature off is byte-identical to `main` for replies and
  pixel-identical for snapshots.
- **Dogfood.**
  - Real Claude Code in shux: `shux pane status` shows working → done(msg)
    → clear.
  - Codex, which does not probe, shows its states.
  - P4: Orca's tab shows the forwarded state, and Ghostty shows nothing and
    no garbage.

### 7.1 Fuzzing

Pane output is attacker-controlled. Fuzzing is therefore a gate for every PR
that touches the parser, the scanner or the table.

**Tier 1 — always on.** Stable proptest inside `make test`.
- ✅ **Landed:**
  - The decoder versus an independent spec-model decoder, on grammar-built
    reports with mutations and on raw bytes.
  - An encode/decode round-trip.
  - `StatusTable` versus a naive reference model over op sequences up to
    600 long, past the cap.
  - All of the above are proven to fail: 8 reintroduced defects are all
    caught. On an unmodified tree they also found a real bug, `44f487b`.
- **P1 adds:**

| Target | Oracle |
|---|---|
| `Osc7501Scanner` vs vte (random streams of text, CSI, OSC, DCS, APC, CAN/SUB, ESC-ESC and NULs, in random chunks) | finished BEL/ST bodies equal vte's dispatched bodies; cuts are chunking-invariant (one byte at a time equals the whole batch) |
| `VirtualTerminal` stream with status on | never panics; replies keep stream order; table ≤ cap; grid, `content_revision` and title are identical to a status-disabled VT fed the same bytes, minus the 7501 reply bytes |
| Publication ordering (random permutations and delays) | the graph ends at the highest generation; no stale resurrection |

**Tier 2 — deep, libFuzzer.**
- **Crate.** A `fuzz/` cargo-fuzz crate. It wires up the `make fuzz`
  placeholder (`Makefile:780`) as `make fuzz-program-status
  FUZZ_SECONDS=600`. Nightly is pinned for that crate only.
- **Targets.** `program_status_decode`, `program_status_table`,
  `vt_program_status_stream` (the input chooses the chunk splits), and
  `scanner_vs_vte`.
- **Seeds.** Ghostty's limit table, kitty's eviction vectors, the real
  Claude Code 2.1.311 byte stream captured under Orca, and a token
  dictionary.
- **Per PR.** P1 and P4 each record a run of at least 10 minutes per target.
  Every crash becomes a tier-1 regression test, seen failing first.

## 8. Removal inventory

1. `crates/shux-program-status/` and its workspace member; `fuzz/` targets.
2. `shux-vt`:
   - `program_status.rs` (scanner + adapter);
   - the cut merge in `process_with_responses`;
   - the 7501, 133 and RIS arms;
   - the `VirtualTerminal` field and its API.
3. `shux-core`: the two `Pane` fields, `set_pane_program_status`, and the
   event variant with its name and JSON mapping.
4. `shux`:
   - `status_tx`, `program_status_enabled`, the publisher, the epilogue lines
     and the replacement clear;
   - the walker;
   - both RPCs and their `EXPECTED_SURFACE` lines;
   - the CLI verbs and printers;
   - the config section and its strict-schema twin;
   - forwarding;
   - D8.
5. `shux-ui`: `PaneChrome.badge` in both renderers, and the status-bar
   segment.
6. `shux-rpc`: `AttachHello.program_status`.
7. `shux-pty`: `foreground_pgrp()`.

The hot-reload keep-previous fix is a standalone correctness fix and stays.

## 9. Risks

| Risk | Mitigation |
|---|---|
| Draft spec churn | protocol knowledge in one crate; limits as consts |
| Senders lie or go stale | advisory only; D7, D8, D9; conductor observes only |
| Codex's bare `state=clear` wipes other programs' records | inherent to the protocol; documented |
| Flooding | raw-length poison; bounded table; ≤ 10 commits/s/pane; 512-reply budget |
| Program-authored text | never in summaries, events, Public RPCs or forwarded reports; D11 for display |
| Exposure of status itself | ContentRead only (D13) |
| Scanner/vte divergence | scanner mirrors the vte grammar; differential fuzzer; re-check on any vte bump |
| Outer probe latency | 300 ms cap, only with `forward = auto` |
