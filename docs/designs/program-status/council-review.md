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
