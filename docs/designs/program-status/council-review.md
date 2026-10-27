# Council review — program status plan

## Round 1 (plan r1 → r3), 2026-10-27

Reviewers ran with `dootsabha council --agents codex,grok`.

- **codex** returned a full review through dootsabha.
- **grok** failed under dootsabha every time with a sandbox refusal:
  `could not apply the 'read-only' sandbox profile … /var/run/docker.sock
  is a symlink`. The same binary, with the same arguments, model,
  reasoning effort, read-only sandbox and prompt, runs fine when invoked
  directly. So grok's review was run that way; it is the same reviewer and
  the same prompt, through a different launcher.

Every claim citing a file or line was checked against the tree before it
was adopted. All held, except one: grok's "tcgetpgrp needs the slave fd".
tmux calls `tcgetpgrp` on the master (`osdep-darwin.c` / `osdep-linux.c`).
P2 measures it on both platforms.

### codex — REJECT

| # | Sev | Finding | Disposition (r3 §) |
|---|---|---|---|
| F1 | P1 | Deferred commit accepts aborted OSC (`ESC [`, `ESC _` take no callback) | Reproduced in vte. Replaced with raw-byte scanner §5.1 |
| F2 | P1 | Truncation guard ≠ whole-sequence limit (vte drops C0 uncounted) | Scanner counts raw length incl. framing §5.1 |
| F3 | P1 | Summary with `RecordView` leaks text to Public/event | Crate summary is text-free (landed); plus D13 ContentRead |
| F4 | P1 | Equality over `revision` defeats dedupe | Summary has no revision (landed); generation is separate §5.1 |
| F5 | P1 | Reload can resurrect cleared status | Global monotonic generation, graph rejects stale §5.2 |
| F6 | P1 | `notify_waiters` misses reloads; VT creation race | notified-before-read walker; flag in PaneIoState under lock §5.4 |
| F7 | P1 | Destroy fires `PaneExited` before IO teardown | Contract: exited ends status; graph order unchanged §5.3 |
| F8 | P1 | All exit paths + incarnation guard | Common epilogue, incarnation id §5.3 |
| F9 | P1 | pgrp sample ≠ authorship | Per-entry origin tags; heuristic, fails safe §5.9 |
| F10 | P2 | `kill(-pg,0)` semantics; biased select starves timer | ESRCH only, pgid>1, separate task §5.9 |
| F11 | P1 | Probe during hello runs before raw mode | Scoped raw guard before hello §5.8 |
| F12 | P1 | No client→daemon capability seam | `AttachHello.program_status` §5.8 |
| F13 | P1 | `clear:id=shux` too broad / root confusion | Per-pane clears; aggregate is an explicit namespaced record §5.8 |
| F14 | P1 | IDs don't isolate attach owners | Per-attach token namespace, full pane hex §5.8 |
| F15 | P1 | `wait-status` lost-wakeup / initial state | Subscribe-before-check RPC, explicit outcomes §5.5 |
| F16 | P1 | Capture grant ≠ status grant | Explicit grants + permission tests §5.7 |
| F17 | P1 | Advisory blocked must not trigger auto-dismiss | Observe-only; `seen` for fallback §5.7 |
| F18 | P2 | String title helper can't colour the badge | `PaneChrome` descriptor in both renderers §5.6 |
| F19 | P2 | Title-event sync barrier can't fire under DEC 2026 | Generation polling as the test sync point §7 |
| F20 | P2 | One commit per read ≠ rate bound | Publisher task ≤10 commits/s §5.2 |
| F21 | P2 | Gate table under-specified | Per-PR gates; P4 gets VT gate; web preview declared absent §6 |
| F22 | P2 | Removal inventory incomplete | Rewritten §8 |

### grok — APPROVE-WITH-CHANGES

| # | Sev | Finding | Disposition (r3 §) |
|---|---|---|---|
| F1 | P0 | Deferred commit (also a 133;A variant) | Scanner owns 7501 **and** 133;A framing §5.1 |
| F2 | P0 | Outer probe vs handshake; per-client gating | Same as codex F11/F12 §5.8 |
| F3 | P1 | D8 sampled on the wrong signal; `on_process_exit` too broad; no publish; D8 needed with chrome | Origin tags, timer publishes, moved to P2 with default on §5.9 |
| F4 | P1 | Slave-fd tcgetpgrp; `kill(0,0)` hazard | pgid ≤ 1 → never swept. Master fd kept (tmux precedent), measured in P2 |
| F5 | P1 | Reload under lock; strict validator lacks the table; whole-file reset on bad key | Graph sends outside lock; strict schema entry; **pre-existing reload reset fixed in P1** §5.4 |
| F6 | P1 | Summary on Public `pane.list` leaks `kind=auth` | D13: off `pane.list`; RPCs ContentRead; `EXPECTED_SURFACE` §5.5 |
| F7 | P1 | Plan sketch vs landed crate; `state=nope:state=working` | Plan uses landed types; last-wins pinned §3, §7 |
| F8 | P1 | VT replacement leaves stale graph summary | Replacement publishes `None` under lock §5.3 |
| F9 | P2 | Glyph table and width budget | Explicit table, width charged before clamp §5.6 |
| F10 | P2 | `pane8` collisions; forwarded text not disarmed | Full pane hex; no program text forwarded §5.8 |
| F11 | P2 | Events are post-batch, ring pressure | Documented sampled projection at 100 ms §5.2 |
| F12 | P2 | P1 skips CLI/TUI gate; DECSTR pin | P1 also runs `shux-tui-qa`, dogfoods in P1; DECSTR test §6, §7 |
| F13 | P3 | Citation fixes | Applied throughout r3 |

### Open questions

| Q | codex | grok | Resolution |
|---|---|---|---|
| Q1 leaf crate | yes | yes | kept |
| Q2 summary in graph, records in VT | yes, with incarnation/epoch | yes, not on `pane.list` | both applied |
| Q3 pgrp | P3, optional heuristic | P2, with chrome | P2, default on, fails safe |
| Q4 dismiss on typing | no delete; per-client dim OK | no delete, no dim | no dim (cross-path equivalence) |
| Q5 probe in hello | unsafe; guarded raw before reader | unsafe; after raw, before reader | scoped raw guard before hello |

### Found while answering the council

- **The hot reload resets the whole config on any parse error.** This is
  pre-existing (`config.rs:366`). It is fixed in P1 as a correctness defect.
- **The fuzzer's first deep run** found the decoder accepting incomplete
  base64 padding (`MA=`). Fixed in `44f487b`, with a red-first test.

## Round 2 (plan r3 → r4), 2026-10-27

Same reviewers. Grok again had to be run directly (same sandbox
refusal under dootsabha). Every cited line was checked before a finding
was adopted. Codex F6 was reproduced as a pre-existing defect before it
was fixed.

### codex — REJECT

| # | Sev | Finding | Disposition |
|---|---|---|---|
| F1 | P1 | Plan grammar omits Escape stay-set / reprocess | Plan text rewritten to the landed scanner (§5.1); positive `ESC ESC ]`, `ESC 0x80 ]` tests added (`9ee5736`) |
| F2 | P2 | `133;A` prefix match | Code was already exact (`classify`); plan text fixed; `133;AB` pinned |
| F3 | P1 | Differential oracle "every dispatch" | Plan states the landed byte-at-a-time oracle (§7); oracle self-test rejects `ESC [`, `ESC _`, CAN |
| F4 | P1 | Equal-state dedupe must advance watermark | Already so in code; plan states it; window resolved first (`9ee5736`) |
| F5 | P1 | Watch can't provide lifecycle barriers | §5.2: per-pane-lifetime sender, `Flush{min_generation, ack}` channel, interruptible pacing, bounded waits; replacement flushes before the new task spawns |
| F6 | P1 | Stale task tears down successor | **Reproduced on main** (`exit_status = Some(-1)`); fixed with incarnation guard (`d132105`), standalone PR |
| F7 | P1 | `disabled` not representable | `enabled` on publication/pane/event; disable is an event (`9ee5736`) |
| F8 | P1 | D8 false deletion | D8 now **opt-in**, stability-sampled, false deletion documented, tests for both settings (§5.9) |
| F9 | P1 | Probe reads stdin, crossterm reads /dev/tty | Probe resolves crossterm's input fd; requires same tty for in/out; skipped otherwise (§5.8) |
| F10 | P2 | Raw restore / signals / late replies | Single raw owner from before probe through session; signal future before raw; late replies swallowed by input parser (§5.8) |
| F11 | P2 | Forwarded title exceeds byte limits | Title truncated at UTF-8 boundary ≤192 B; every report round-trips through `decode` before emission (§5.8) |
| F12 | P2 | P2 gate conditional; matrix cells missing | Every PR carries full protocol; P2 VT gate unconditional; feature-maxed in P1; P4 config states enumerated (§6) |

### grok — APPROVE-WITH-CHANGES

| # | Sev | Finding | Disposition |
|---|---|---|---|
| F1 | P1 | Plan grammar wrong (code right) | = codex F1 |
| F2 | P1 | Oracle sentence wrong | = codex F3 |
| F3 | P1 | `133;A` prefix | = codex F2 |
| F4 | P1 | BEL-cut neutrality / tie rule | §5.1 states tie order and BEL-cut argument; enabled-vs-disabled neutrality property (hostile alphabet incl. BEL, APC, UTF-8) is the proof |
| F5 | P1 | No ack channel; replacing sender drops clear | = codex F5 |
| F6 | P1 | Read arm writes new VT | Incarnation checked in read and resize arms (`d132105`) |
| F7 | P1 | Publisher shutdown; WindowNotFound consumes generation | Publisher selects on shutdown, teardown drops sender (§5.2); window resolved first (`9ee5736`) |
| F8 | P1 | `disabled` unrepresentable; subscription not pane-filtered | = codex F7; subscription filtered by pane id (§5.5) |
| F9 | P1 | Read-time pgrp sample tags the shell | Before/after stability sampling; unstable → tag 0 (§5.9) |
| F10 | P1 | Raw guard restoration; non-tty stdin; pending bytes | = codex F9/F10; pending bytes injected into the real input stream |
| F11 | P2 | P1 gates miss neutrality + rich-TUI corpus; reload test already green | Rich-TUI pass named in P1; neutrality property named; reload item struck |
| F12 | P2 | Reload fix already landed; NotFound transient | §5.4 updated; NotFound re-checked once after debounce (config PR) |
| F13 | P2 | 7501 reply dropped when 512 budget full | **Declined** with reason: shared budget is the anti-amplification bound; a missed reply means "unsupported", the safe fallback (§5.1) |
| F14 | P2 | 64-cap eviction could wipe all | Evict LRU pane record by its own id; never clear the aggregate; 64-bit attach token (§5.8) |
| F15 | P3 | BEL payload off-by-one | Documented (spec permits lower limits) (§5.1) |
| F16 | P2 | Plan said `serde(default)` | Plan says `serde(skip)`, matching code and test |
