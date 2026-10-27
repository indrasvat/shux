# Program status (OSC 7501) — implementation plan

Status: **r6**, revised after four council rounds (codex + grok). Every
finding and its disposition is in `council-review.md`.

- **Round 1:** codex REJECT (22 findings), grok APPROVE-WITH-CHANGES (13).
- **Round 2:** codex REJECT (12 new), grok APPROVE-WITH-CHANGES (16 new).
- **Round 3:** codex REJECT (10 new, no P0), grok APPROVE-WITH-CHANGES (9
  new, no P0).
- **Round 4:** codex REJECT (7 new, no P0), grok REJECT (6 new, no P0).

Spec: Program Status Protocol v0.4 (2026-10-10),
<https://superlogical.com/rex/docs/build/program-status>

Landed on this branch (each change has a test seen red first, or a
mutation-checked oracle):

| Commit | What |
|---|---|
| `4fba7d1` | protocol crate |
| `576fd90` | tier-1 fuzzers for the crate |
| `44f487b` | incomplete-padding bug found by the fuzzer |
| `7fa4d70` | hot reload keeps the running config on a parse error (pre-existing defect) |
| `b7248d5` | VT seam: raw-byte scanner, probe, records |
| `e0c816b` | graph seam |
| `d132105` | PTY-task incarnation guard (pre-existing defect, codex r2 F6) |
| `9ee5736` | `enabled` observable; watermark ordering |
| `bbbcc5c` | config: grace window for a vanished file |
| `8275784` | tier-2 libFuzzer crate, `make fuzz` |
| `53ce29d` | events.watch classified on the key it subscribes with (pre-existing permission bypass, codex r3 F1) |
| `7b5330b` | incarnation guard completed: graph-side PTY ownership, owner-checked recorders, title re-check (codex r3 F2, grok r3 F1/F2) |
| `a9047f0` | leak-guard self-test runs on current macOS (pre-existing) |
| *(next)* | ownership is monotonic on both sides; refused and orphaned spawns clean up; side maps pruned on every commit (codex r4 F1/F2/F7, grok r4 F1/F2/F6) |
| *(next)* | config: one read decides, and an empty file gets the grace too (codex r4 F4, grok r4 F5) |
| *(next)* | leak-guard self-test: fixture must be named by the guard; wedge cleanup on every exit (codex r4 F5/F6) |

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
- **Malformed values, two tiers.** A pair whose value has a byte outside the
  value grammar is unparseable and skipped (`title=Q!` applies the rest). A
  grammar-valid value that fails its own validation discards the report
  (`title=QQ=`: strict base64, D3; `id`: D2). Both follow D1; the adversarial
  pass flagged the split as surprising and it was kept deliberately.
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
 shux-vt│ ✅   shux-core│ ✅      shux-ui│        shux│
 raw-byte scanner  Pane fields    PaneChrome     publisher, RPCs, CLI, config,
 seen, generation  setter+event   badge          reload walker, forward, D8
```

**VT seam** (landed, `crates/shux-vt/src/program_status.rs`). The plan text
now matches the code.

- **Scanner grammar.** `OscStatusScanner` mirrors vte 0.15
  (`advance_esc`, `advance_osc_string`).
  - **`Ground`** — `memchr` for ESC → `Esc`.
  - **`Esc`** (vte is in Escape):
    - `]` opens an OSC.
    - The *stay set* keeps the scanner in `Esc`, because vte executes or
      ignores these without leaving Escape: C0 except CAN/SUB, a second ESC,
      DEL, and `0x80..=0xFF`. So `ESC LF ]`, `ESC ESC ]`, `ESC DEL ]` and
      `ESC 0x80 ]` **do** open an OSC.
    - Any other byte returns to `Ground`. That includes `ESC SP …`
      (escape-intermediate), `ESC [`, `ESC P`, `ESC X/^/_`, and CAN/SUB.
  - **`Osc`**:
    - BEL finishes the sequence.
    - CAN and SUB abort it.
    - ESC → `OscEsc`.
    - The C0 bytes vte drops (`0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1C..=0x1F`)
      count toward wire length but are not stored. DEL and 8-bit bytes are
      stored, exactly as vte stores them.
  - **`OscEsc`**:
    - only an **immediate** `\` finishes the sequence (ST).
    - Any other byte aborts the OSC and is **reprocessed** in `Esc`, without
      being consumed. So `ESC ]` opens the next OSC, and `ESC c` both drops
      the report and still runs RIS through vte.
  - **Out of scope, as in `graphics/apc.rs`:** 8-bit C1. `0x9D` is not an
    OSC introducer and `0x9C` is not ST, and vte agrees.
- **Wire length.** It is counted from `ESC ]` through the terminator. Past
  4096 the sequence is poisoned and dropped, then the scanner resyncs.
  - BEL is one byte shorter than ST, so a BEL-terminated payload of exactly
    4088 bytes passes the scanner but fails `wire::decode` (cap 4087). The
    spec allows a terminal to choose lower limits; this is documented and
    harmless.
- **What gets cut** (`classify`):
  - A finished body of `7501;…` is a report or a query.
  - `133;A` counts as a prompt start only when followed by end-of-body or
    `;`. `133;AB` is a non-event.
- **Cuts merge with APC cuts by `end`.** Ends cannot coincide, since two
  sequences cannot share a terminator byte. The merge still defines a tie as
  APC first, then status, with both side effects running.
  - Every byte still reaches vte.
  - ST cuts inherit the `graphics/apc.rs` neutrality argument.
  - BEL cuts land where vte has just left `OscString` for `Ground` with no
    partial UTF-8 pending, so the next `advance` begins a fresh ground run.
    This is proved by the enabled-vs-disabled neutrality property below, not
    asserted.
- **Disabled.** The scanner still runs, so enabling mid-sequence cannot
  misparse, but the cuts are discarded and no slicing happens.
- **Reply budget.** A 7501 reply shares the existing 512-reply budget with
  DA1/DSR. A batch that exhausts it drops later replies of every kind alike.
  Reserving a slot was considered and declined: it would make 7501 the one
  query a reply flood cannot silence, and a sender that misses its reply
  falls back to "unsupported", the safe outcome (grok r2 F13).
- **State and generation.** These are unchanged from r3, plus
  `StatusPublication.enabled` (§5.5). Every observable change, including
  enable, disable and terminal construction, draws a new process-global
  generation.

**Graph seam** (landed, with two r2 corrections):
- **Pane fields.** `Pane.program_status`, `program_status_seen` and
  `program_status_enabled` are all **`#[serde(skip)]`** (r3 said
  `serde(default)`, which would have serialized status; grok r2 F16).
  `program_status_is_never_serialized_with_the_pane` pins it.
- **Setter order** (codex r2 F4, grok r2 F7). `set_pane_program_status`
  resolves the pane and window *first*. Only then does it advance the
  generation watermark, and it does so **before** the equality dedupe: an
  equal publication suppresses the event, never the watermark. A
  `WindowNotFound` therefore never consumes a generation.
- **Event.** `PaneStatusChanged { …, old, new, seen, enabled }`, wire name
  `pane.status_changed`. The summaries are boxed to keep `Event` small.

### 5.2 Publication path (codex r1 F5/F6/F20, r2 F5, r3 F3/F5–F7; grok r1 F11, r2 F5/F7, r3 F3/F5/F7)

**Lifetime.** There is one `watch::Sender<StatusPublication>` and one
publisher task per **pane**.
- The task is created when the pane first gets a PTY, and it gets its own
  **pane-lifetime** cancellation token. That token is distinct from the PTY
  task's, which a replacement cancels. VT replacement therefore never stops
  the publisher (codex r3 F5).
- Teardown (`teardown_panes_collecting`) and daemon shutdown cancel the
  pane-lifetime token, drop the sender, and `await` the publisher's join
  handle, bounded at 500 ms.

**Generation-ordered slot** (grok r3 F5). Writers use
`send_if_modified(|cur| new.generation > cur.generation)`, under the same
io-lock hold that read the publication from the VT. An older publication
never overwrites a newer one.

```
publisher (receiver subscribed before the sender is published)
  committed = 0; pending: Vec<Flush>; retry_at: Option<Instant>
  loop {
    publish_latest()        // ALWAYS first: the initial value is published (codex r3 F6)
    ack every pending flush with min_generation <= committed; keep the rest
    select! {               // one select; nothing waits in a second park (r3 F3)
      _ = lifetime.cancelled()  => break
      r = rx.changed()          => if r.is_err() { break }
      Some(f) = flush_rx.recv() => pending.push(f)
      _ = sleep_until(retry_at), if retry_at.is_some() => {}   // codex r4 F3
    }
    if pacing still running: sleep(remaining of 100 ms), interruptible by
      lifetime / flush (a flush pushes and skips the rest of the wait)
  }
  on exit: drop every pending ack (callers see Err and proceed)
```

- **`publish_latest`** re-borrows the slot. It skips the call if
  `generation <= committed`. Otherwise it calls the graph inside
  `tokio::time::timeout(250 ms, …)` and a `select!` on `lifetime` **and on
  `flush_rx`**, so neither shutdown nor a flush is stuck behind a graph
  await (codex r3 F7, grok r4 F4).
  - `PaneNotFound` or `GraphError::Shutdown` → exit the loop.
  - Timeout → log, keep the value, set `retry_at = now + 100 ms`. Without
    that deadline nothing wakes the loop again once the watch notification
    is consumed, and a graph that recovers would never see the value
    (codex r4 F3). Any successful publish clears `retry_at`.
  - A flush that arrives mid-call is pushed to `pending` and the in-flight
    send is abandoned. The loop goes round, re-borrows the slot, and
    publishes its **current** value, which is at least the flush's
    `min_generation`. An older value can therefore never be the last thing
    committed before an ack. An abandoned send that the actor had already
    applied is harmless: the newer one supersedes it by generation.
  - The 250 ms per-call budget sits under the flush's 500 ms, so one slow
    call leaves room for the retry that acks.
- **Flush** is for VT replacement and disable only: bounded 500 ms,
  best-effort. Natural exit does **not** use it (next item).
- **Initial event.** Every pane's first publication flips `enabled` from the
  `Pane::new` default, so it fires one `pane.status_changed`. This follows
  from making `enabled` observable (codex r3 F6). Grok r3 F7 proposed seeding
  the pane from config to suppress it. That was declined: `Pane::new` takes
  no config, threading it through every pane-creation path buys nothing a
  consumer can use, and one event per pane spawn is the same cardinality as
  `pane.created`. Documented. (r3 said "shux-core has no config", which is
  wrong; codex and grok r4 both caught it. The decision stands.)

### 5.3 Lifecycle (codex r1 F7/F8, r2 F5/F6, r3 F2/F4; grok r1 F8, r2 F5/F6, r3 F1/F2)

**PTY ownership.** These pre-existing defects are fixed and landed.
- **Incarnation number.** Every spawn draws a process-global incarnation and
  claims the pane in **the graph** (`claim_pane_pty`) before installing
  handles.
- **Monotonic on both sides** (codex r4 F1, grok r4 F1). A claim and an
  install are separate steps, so two spawns can interleave them. Both sides
  therefore refuse anything older than what they hold:
  - `claim_pane_pty` returns `Ok(false)` and changes nothing for an
    incarnation lower than the owner's. No exit is cleared and no
    `pane.respawned` fires.
  - The IO install refuses an incarnation lower than `incarnations[pane]`.
  - Either refusal kills and reaps the child and returns `Ok`: a newer spawn
    owns the pane and runs there.
  Both sides converge on the highest incarnation that got that far.
- **Pane gone or daemon stopping** (codex r4 F2, grok r4 F2).
  - A claim error kills the child and fails the spawn; nothing is installed.
  - Under the IO lock, a cancelled daemon token refuses the install. Daemon
    shutdown cancels that token before it collects the panes it waits for.
  - After installing, the spawn reads the graph snapshot. Every destroy path
    commits to the graph **before** it tears down IO (`scratch_reap` was the
    exception and now follows suit). So either the pane is still there, or
    that teardown already ran and the spawn tears itself down.
- **Exit commit.** Exit goes through `set_pane_exit_status_owned(pane,
  status, incarnation)`. The graph actor applies it only when **that**
  incarnation owns the pane, at the instant it commits, so no window opens
  between check and write (grok r3 F2). An unclaimed pane ignores it.
- **Premature exit.** A newer claim that finds the pane already marked
  exited by a replaced predecessor clears `exit_status` and fires
  `pane.respawned`.
- **Side maps.** `pty_owners` and `status_generations` are pruned to the
  live panes on every snapshot commit, so a destroyed pane leaves nothing
  behind (codex r4 F7, grok r4 F6).
- **What else checks the incarnation:**
  - recorder tee, resize tee and finish, all inside the lock that selects the
    sinks (codex r3 F2, grok r3 F1);
  - the read and resize arms;
  - handle teardown;
  - the OSC title forward, re-checked just before the graph send.
- **Documented residuals.**
  - One sampled `pane.output` broadcast from a read already past its check.
    It is transient and never stored.
  - A title already in flight to the graph. It is overwritten by the
    successor's first title.
- **Tests, red first:**
  - `a_replaced_pty_task_leaves_its_successor_alone`;
  - `a_replaced_pty_task_does_not_finish_its_successors_recorder`;
  - `a_replaced_pty_task_that_lost_the_claim_installs_nothing`;
  - `a_replaced_pty_task_older_than_the_installed_one_installs_nothing`;
  - `a_replaced_pty_task_for_a_missing_pane_fails`;
  - `a_replaced_pty_task_whose_pane_died_mid_spawn_cleans_up`;
  - graph `an_exit_reported_by_a_replaced_pty_incarnation_is_ignored`;
  - graph `claiming_an_exited_pane_brings_it_back`;
  - graph `a_claim_older_than_the_owner_changes_nothing`;
  - graph `an_exit_from_an_unclaimed_pane_is_ignored`;
  - graph `destroying_a_pane_forgets_its_owner_and_generation`.

**Natural exit: final status and exit in one graph command** (codex r3 F4,
grok r4 F3).
- In **one** io-lock hold, the epilogue checks `is_current(pane,
  incarnation)`. If it is not current it touches no VT; the successor's
  records are not the predecessor's to end. If it is current, it calls
  `note_process_exit()` and reads the final `StatusPublication`. Check and
  mutation are never split across an await.
- It then sends `set_pane_exit_status_owned(pane, status, incarnation,
  Some(final_status))`.
- The actor does nothing unless `pty_owners[pane] == incarnation`. That
  gate covers **both** halves: a successor's newer generation would drop
  only the status half, so the exit half needs the owner check of its own.
  If owned, it commits the status (if its generation is newer), then the
  exit, firing `pane.status_changed` and then `pane.exited` from one
  command.
- The ordering therefore holds unconditionally. A later publisher send for
  any generation up to the committed one is stale and dropped.
- Nothing waits on the publisher, so a slow or dead publisher cannot delay
  `pane.exited`.

**Requested teardown and destroy.** The graph removes the pane and fires
`PaneExited` first, which is unchanged. `pane.exited` ends the pane's
status; the publisher exits on `PaneNotFound` or cancellation.

**VT replacement.**
- Under the io lock: insert the new VT (which draws a newer generation), then
  `send_if_modified` the clear on the pane's existing sender.
- Drop the lock, `Flush` (best-effort, 500 ms), then spawn the new PTY task.

### 5.4 Config and reload (codex r1 F6; grok r1 F5, r2 F12)

```toml
[program_status]
enabled     = true    # master: false = no reply, no records, no chrome, no forward
border      = true    # badge in pane borders             (P2)
status_bar  = true    # "N waiting" segment               (P2)
stale_guard = false   # D8 heuristic, opt-in              (P2)
forward     = "auto"  # outer terminal: auto | off        (P4)
```

- **Enable flag.** `PaneIoState.program_status_enabled` is authoritative.
  VT insertion applies it under the same lock. The walker seeds it from
  `DAEMON_CONFIG`; outside the daemon the default applies.
- **Walker.**
  - It arms `notified()` before `current()`.
  - Under the io lock it flips the flag, toggles each VT and calls
    `send_replace`.
  - It never awaits the graph under the lock.
  - A disable then `Flush`es every pane (bounded), so a `wait_status` waiter
    sees `enabled = false` promptly.
- **Strict validator.** `config_validate.rs` gains the `[program_status]`
  table; without it a valid block fails `shux config validate`. `shux
  config init` emits the block.
- **Hot reload keeps the running config on a parse error.** This **landed in
  `7fa4d70`** and is no longer a P1 work item (grok r2 F12).
  - A file that *stays* missing or empty still installs defaults, by design
    and tested.
  - **Grace window** (grok r2 F12; landed `bbbcc5c`, completed after codex
    r4 F4 and grok r4 F5). Editors save by delete-then-write or
    truncate-then-write, so the file can be briefly missing *or empty*.
    - One read decides; there is no separate `exists()` check for an unlink
      to slip past.
    - A missing or zero-byte read waits 150 ms and reads again, and only
      that second read can install defaults.
    - Residual: a write still in progress that happens to parse (a valid
      prefix of the new file) is applied. The debounce makes that rare, and
      the next write event corrects it.

### 5.5 RPC and CLI (D13; codex r2 F7; grok r2 F8)

- **`enabled` is first-class.** `StatusPublication` carries `enabled` as
  well as `seen`, so do `Pane.program_status_enabled` and the event. Disabled
  and "enabled, never reported" are therefore distinguishable, and disabling
  an empty pane *does* change `(summary, seen, enabled)`, so it fires an
  event.
- **Plugins and the event.** A manifest `subscribes` filter is not a
  `ContentRead` grant. `pane.status_changed` reaches a plugin only under a
  `pane.status` grant covering that pane, read from the plugin's grants per
  event (so a revoke takes effect at once). Found by the adversarial pass: a
  grant-less plugin subscribed to `pane.` received `kind=auth`.
- **`pane.status`** (`ContentRead`): full `RecordView`s plus `generation`,
  `seen` and `enabled`, read atomically from the VT under the io lock. It is
  the only RPC that returns `title` or `msg`.
- **`pane.wait_status`** (`ContentRead`):
  - **Params:** `pane_id` (as every pane RPC), `state: [..]`,
    `match: "top"|"any"` (any other value is INVALID_PARAMS), `timeout_ms`.
  - **Subscription:** `events.watch`-style, **filtered to
    `pane.status_changed` / `pane.exited` for that pane id**. It is taken
    *before* reading the graph's `(summary, seen, enabled)`. It re-reads on
    `gap` or `lagged`.
  - **Outcomes:**
    - `matched {summary}`;
    - `timeout`;
    - `pane_gone` (`pane.exited`, or the pane vanished mid-wait; a pane that
      never existed is a NOT_FOUND error, as for every pane RPC);
    - `disabled`, returned only when `enabled == false`, never inferred from
      `None` + `!seen`.
- **`pane.list` does not change** (D13).
- **Surface pin.** Both methods are added to `EXPECTED_SURFACE`.
- **CLI:** `shux pane status`, `shux pane wait-status`; output goes through
  `style.rs` printers and `sanitize_for_display`. `--format plain` is
  tab-separated without colour. `wait-status` exits 0 matched, 1 timeout,
  2 bad arguments, 3 otherwise (pane gone, disabled, daemon lost, any other
  error).
- **Terminal replies are bounded.** A program that sends probes without
  reading its input used to wedge its pane (a pre-existing defect: replies
  were written inside the read loop with no bound; 7501's longer reply made
  it ~4x easier to hit). Replies now get 200 ms; a program that does not
  drain them gets none for 1 s.
- **Known residual.** A daemon shutdown that lands during a respawn's claim
  await can still announce a respawn of an exited pane: in-memory, during
  shutdown only. The common case (already stopping) is refused before the
  claim.

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

### 5.8 Outer terminal (P4) (codex r1 F11–F14, r2 F9–F11, r3 F8/F10; grok r1 F2/F10, r2 F10/F14, r3 F4)

**One owner of the tty.**
- The attach client resolves crossterm's input descriptor exactly as
  crossterm does: stdin if it is a tty, else `/dev/tty`. It compares
  `st_dev`/`st_ino` with stdout.
- The probe runs only when they are the same terminal and `forward = "auto"`.
  Otherwise there is no probe and no forwarding.

**Raw mode from probe to detach** (codex r3 F8, grok r3 F4).
1. Signal handlers for SIGINT, SIGTERM and SIGHUP are registered
   *synchronously* before raw mode, through `signal-hook`'s self-pipe.
2. One `TerminalGuard` enters raw mode before the probe and is the only thing
   that leaves it. That happens on connect failure, `AttachReady::Error`, a
   handshake error, panic (the existing hook), a signal, or detach.
3. The probe never re-enters cooked mode. `enable_raw_mode` is process-global,
   so a nested guard would clear the session's raw mode.

**The probe itself.** It reads that fd *before* the crossterm event thread
exists.
- It writes `OSC 7501;? ST` and `CSI c`, then reads until the **DA1 reply**
  or a 2 s deadline. Claude Code uses the same sentinel and deadline.
- A terminal answers queries in order, so once DA1 arrives no 7501 reply can
  follow. That removes "late replies" by construction.
- If DA1 never arrives, forwarding is off for this attach. The residual risk,
  that a terminal answering neither DA1 nor in order could later leak reply
  bytes as keys, is documented.
- Any byte that is not part of the two replies was typed by the user during
  the probe. It is sent to the daemon **verbatim as `AttachClientFrame::Input
  { data }`** (raw key bytes) right after `AttachReady`, before the crossterm
  thread starts.
  - No crossterm injection API is needed.
  - Bytes typed in that window bypass client-side prefix handling, which is
    documented.

**Capability and namespace.**
- `AttachHello.program_status: bool`, `#[serde(default)]`.
- The namespace is `shux-<attach16>`, a 64-bit random token per attach.
  Pane records are `shux-<attach16>/<pane32>`; the aggregate is
  `shux-<attach16>`.

**Record cap** (codex r3 F10). At most **63 pane records plus the aggregate**
(64 total). Before inserting pane record 64, the least-recently-changed pane
record is cleared by its own id. The aggregate is never cleared to make room.

**No program text leaves shux.** Records carry state, kind, progress and app.
`title` is the sanitized shux pane title, truncated at a UTF-8 boundary to
≤ 192 bytes. Every report round-trips through `wire::decode` before
emission. `msg` is never forwarded.

**Emission.** Changed records only, batched every 250 ms per connection.
Pane removal comes from graph `PaneExited`. Detach or disable clears
`shux-<attach16>`.

### 5.9 Stale guard (D8, P2, **opt-in**, documented false-deletion risk) (codex r1 F9/F10, r2 F8, r3 F9; grok r1 F3/F4, r2 F9, r3 F6)

**Origin tags.** The crate keeps an opaque per-entry `origin: u64`.
`drop_origin(tag)` removes only working, blocked and idle entries with that
tag.

**Provenance** (codex r3 F9, grok r3 F6).
- The scanner records `tcgetpgrp(master)` when each OSC *opens* and again when
  it *finishes*. Sequences can span reads, so the samples travel with the
  scanner state.
- The daemon samples once more before and once after the batch.
- The report is tagged only if all those samples agree. Otherwise, or on any
  error, it gets tag 0, which is never swept.
- Retagging happens **before** the io lock is released, so no entry ever
  carries an unconfirmed tag.
- pgid ≤ 1 and the shell's own pgid → tag 0.

**Sweep.** A separate 2 s task, off the io lock. Only `ESRCH` from
`kill(-pgid, 0)` removes records.

**What sampling cannot do.** It cannot establish authorship.
- A background writer, or a foreground job's queued output read after
  another job took the foreground, can be misattributed and later
  **falsely deleted**.
- r4's claim that sampling closes the handoff case is withdrawn.
- That is why `stale_guard` is **off by default**.
- Tests document the queued-output and background-writer misattribution
  with the guard on, and prove no deletion with it off.

## 6. Phases → PRs

Every PR carries the full CLAUDE.md feature protocol. Nothing is
conditional; an unfillable cell is called out explicitly.

- **Before the first push:**
  - `make check`;
  - a red-first test for every defect;
  - adversarial review (`adversarial-review` skill);
  - `shux-simplify-architect` (re-armed after every ~200 more added lines,
    with the reviewed SHA named);
  - a council on the diff.
- **After the push:**
  - `gh-ghent` from PR creation onward, re-entered after every push, every
    thread answered and resolved;
  - visual proof as a PR comment (or a Claude Artifact), or the one-line
    non-visual exemption;
  - after merge, a `curl | sh` smoke of the published binary.

| PR | Scope | QA gates | Config states × render paths |
|---|---|---|---|
| **P0 fixes** | `7fa4d70` hot-reload keep-previous + NotFound re-check; incarnation guard. Two standalone correctness fixes, one PR each | `shux-tui-qa` | non-visual: exemption stated |
| **P1 core** | crate, VT seam, graph seam (landed); publisher + flush; lifecycle epilogue; VT-replacement clear; `enabled`; walker; strict schema; `pane.status` / `pane.wait_status`; CLI; tier-2 fuzz crate | `shux-vt-solid-qa` (parser, terminal responses; tracked `SOLID-QA.md`, `make check-vt-qa`) **and** `shux-tui-qa` (CLI text/`--json`, events) | `events.watch` + RPC + CLI × default · `config init` · **feature-maxed** · disabled · malformed · hot-reload, on real colour-probed panes. **Rich-TUI pass:** vim/nvim, lazygit, btop/htop, vicaya, vivecaka render unchanged with status on (the parser now slices `advance`). Dogfood Claude Code + Codex. No chrome: border and snapshot cells are declared unaffected, and the enabled-vs-disabled neutrality property is the pixel-identity proof |
| **P2 chrome + D8** | `PaneChrome` in both renderers, badge, status-bar segment, toggles, D8 opt-in | `shux-tui-qa` **and** `shux-vt-solid-qa` (D8 adds VT origin handling; unconditional) | attach · pane/window/session.snapshot × default · init · feature-maxed · disabled · malformed · hot-reload, both font sets; cross-path cell test. Web preview: not present in the tree, declared |
| **P3 agents** | conductor on `pane.status` with explicit grants, observe-only; skill docs | `shux-tui-qa` (agent workflows) | dogfood: Claude Code + Codex under conductor, × default · disabled |
| **P4 forward** | probe (same tty), single raw owner, hello bit, namespaced emitter, eviction, title clamp | `shux-tui-qa` **and** `shux-vt-solid-qa` (terminal probing and replies) | outer Orca (supports) and Ghostty 1.3.1 (does not, must stay clean) × `forward = auto` · `off` · feature disabled · malformed; piped stdin; SSH with and without a pty; `make test-gui-terminal` for the unsupported receiver |

## 7. Verification

- **Crate** ✅: a unit test for every D-rule and limit, plus tier-1 fuzzing.
- **Scanner** ✅, landed in `b7248d5`; the r4 additions are marked *new*.
  - BEL and ST finish a sequence.
  - These interrupt an *open* body and abort it, each at every split:
    CAN; SUB; `ESC [`; `ESC _`; `ESC P`; `ESC X`; `ESC ]`; ESC then C0;
    ESC-ESC; `ESC [ ESC \`.
  - *new* The following **open** an OSC and commit a later BEL/ST sequence:
    `ESC LF ]`, `ESC ESC ]`, `ESC DEL ]`, `ESC 0x80 ]`.
  - *new* `ESC SP ]` opens nothing.
  - *new* DEL inside a body is stored.
  - `133;AB` is not a prompt start.
  - A sequence over the limit made entirely of NULs is poisoned; the scanner
    resyncs.
- **Differential oracle** (codex r2 F3, grok r2 F2). The reference feeds vte
  **one byte at a time**. A body counts as finished only if one of these
  holds:
  - the dispatch was BEL-terminated; or
  - the dispatching byte was ESC, the very next byte produced
    `esc_dispatch('\\')` with no intermediates, and no other callback ran
    in between.

  The oracle is itself proven to reject `ESC [`, `ESC _` and CAN
  (`the_reference_itself_rejects_aborted_strings`). It is compared against
  the scanner whole, split once, and bytewise.
- **VT** ✅:
  - Reply bytes are checked for ST and BEL.
  - Replies keep **stream order** across random interleavings of probes and
    DA1 queries at random splits.
  - Disabled answers only DA1.
  - RIS clears and resets `seen`; DECSTR keeps.
  - 133;A drops working, blocked and idle.
  - Status stays live under DEC 2026 and the alt screen.
  - No `content_revision` bump.
  - **Neutrality:** an enabled terminal equals a disabled one, apart from the
    7501 replies, on frame, scrollback, cursor, title, `content_revision` and
    responses. Streams come from a hostile alphabet including BEL
    terminators, APC strings and UTF-8 lead and continuation bytes. This
    neutrality is the BEL-cut proof.
- **Incarnation guard:** `a_replaced_pty_task_leaves_its_successor_alone`,
  red on the unfixed tree (`exit_status = Some(-1)`).
- **Daemon integration** (real panes, colour-probed, isolated
  `XDG_RUNTIME_DIR`, `no_leak_guard`). The sync point is `pane.status`
  generation polling.
  - printf reports → RPC and event.
  - Natural exit → `status_changed` before `exited`.
  - `pane.kill` → `exited` only, and the publisher exits (no leak).
  - VT replacement → the clear is committed before the new program's first
    report.
  - A python probe sees `?…` before DA1.
  - Rapid hot-reload off/on/off: the final state wins, with no resurrection
    under an injected publisher delay.
  - Disabling an empty pane with a waiter pending → `disabled`.
  - A 10k tiny-write flood stays at ≤ 10 commits/s while other RPCs stay
    responsive.
  - `wait_status`: already matching / transition during subscribe /
    timeout / pane gone / disabled.
  - Permission tests for both RPCs.
  - Daemon shutdown leaves no publisher behind.
- **Publication ordering property:** random permutations and delays of
  publications, including equal-summary ones, land on the
  highest-generation state.
- **Cross-path (P2):** attach-frame cells equal `window.snapshot` cells,
  colours included.
- **Dogfood:** Claude Code and Codex inside shux (P1); Orca and Ghostty as
  outer terminals (P4).

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
  FUZZ_SECONDS=600` (landed, `8275784`). `-s none` gives libFuzzer coverage on the pinned stable toolchain; no nightly.
- **Targets.** `program_status_decode`, `program_status_table`,
  `vt_program_status_stream` (the input chooses the chunk splits), and
  `scanner_vs_vte`.
- **Seeds.** Ghostty's limit table, kitty's eviction vectors, the real
  Claude Code 2.1.311 byte stream captured under Orca, and a token
  dictionary.
- **Per PR.** P1 and P4 each record a run of at least 10 minutes per target.
  Every crash becomes a tier-1 regression test, seen failing first.

## 8. Removal inventory

1. `crates/shux-program-status/` and its workspace member; the `fuzz/`
   targets.
2. `shux-vt`:
   - `program_status.rs` (scanner + state);
   - the cut merge in `process_with_responses`;
   - the 7501/133 early return and the RIS hook in `parser.rs`;
   - the three `VirtualTerminal` fields and their API;
   - the `VtHandler.program_status` field.
3. `shux-core`:
   - three `Pane` fields;
   - `set_pane_program_status` and its `GraphCommand`;
   - the `status_generations` side map;
   - the event variant and its name.
4. `shux`:
   - `PaneIoState.status_tx` and the flush sender (and their removal in
     `teardown_panes_collecting`), plus `program_status_enabled`;
   - the publisher task;
   - the epilogue lines and the replacement clear;
   - the walker;
   - both RPCs and their `EXPECTED_SURFACE` lines;
   - the CLI verbs and printers;
   - the config section and its strict-schema twin;
   - forwarding;
   - D8.
5. `shux-ui`: `PaneChrome.badge` in both renderers; the status-bar segment;
   the attach-input swallowing of probe replies.
6. `shux-rpc`: `AttachHello.program_status`.
7. `shux-pty`: `foreground_pgrp()`.

These stay if the feature is removed, because they are standalone fixes:
- the hot-reload keep-previous fix;
- the PTY-task incarnation guard.

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
