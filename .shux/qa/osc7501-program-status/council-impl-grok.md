I'll review the OSC 7501 P1 slice on the current tree, including the uncommitted fixes, and check it against the design. Starting with the diff and the publication path.**APPROVE-WITH-CHANGES.** Three P1s are merge blockers. The publication shape is right; these are holes in the ordering and the wait loop.

## Findings

**1. P1 — `pane.wait_status` misses its deadline while any other pane publishes status.** `crates/shux/src/rpc/program_status.rs:226-246`

The subscription is every `pane.status_changed` and `pane.exited`, not this pane. Events for other panes hit `_ => {}` and the inner loop starts over. Each pass recomputes `wake = now + 250ms`, so the re-poll arm never fires if another pane changes status faster than that. A progress tick changes `StatusSummary` (`progress` is part of the equality check) and the publisher's pace is 100 ms, so one busy pane is enough.

`timeout_at` polls `recv` before the timer. After `deadline`, a steady stream keeps an event ready, the `Err` arm never runs, and the handler neither re-reads nor returns `timeout`. The 250 ms re-read (the disabled-terminal fix) only works while the bus is quiet. The new test is quiet, so it stays green.

Fix: if `now >= deadline`, `decide()` once more and return `timeout`. Keep the re-poll on a fixed instant that other panes' events do not push forward.

**2. P1 — A config reload in the exit RPC window publishes after `pane.exited`, or drops the final status.** `crates/shux/src/pane_spawn.rs:433-468`, `crates/shux/src/program_status.rs:143-157`, `crates/shux-core/src/graph.rs:1653-1671`

The epilogue publishes the final generation and releases the io lock, then awaits `set_pane_exit_status_owned`. `writers` is removed only after that await. `apply_enabled` treats "writer present" as "still alive", mints a newer generation, and publishes it. `set_pane_program_status` does not look at `exit_status`.

The exit command already holds the older snapshot. If that command is queued first, the actor emits `pane.status_changed` and `pane.exited`, then the reload, so a status event follows `pane.exited` and can clear the summary a waiter would have matched. If the reload is queued first, the watermark moves past the exit snapshot, the final `done`/`error` is dropped as stale, and `pane.exited` fires with the cleared summary. `wait_status` for that state returns `pane_gone`.

The new skip runs only once the writer is gone. `a_pane_that_exited_publishes_nothing_after_its_exit` applies the reload after `exit_status` is set, so it never enters this window.

Fix: seal the pane against later publishes in the same lock hold as `note_process_exit`. In `set_pane_program_status`, return without writing when `exit_status` is already `Some`. The exit command applies status before it sets that field, so the final snapshot still lands.

**3. P1 — A respawn while program status is off keeps the dead program's summary.** `crates/shux/src/pane_spawn.rs:630` and `:685-710`, `crates/shux-vt/src/lib.rs:270` and `:547-573`

`VirtualTerminal::new` mints `status_disabled_generation` before `claim_pane_pty` is awaited. `set_program_status_enabled(false)` on a terminal that is already off does not mint again (`(false, false) => {}`). The value published for the new terminal is that pre-await generation.

During the claim await the old task can still take the io lock, run `note_process_exit` or another report, and store a higher generation. `send_if_modified` then rejects the new terminal. `Flush` acks as soon as `committed >= min_generation` (`program_status.rs:215-220`), and the higher stale generation satisfies it. With the feature off, nothing later mints a generation, so the old summary stays for the life of the pane. The new PTY is already allowed to start.

An enabled respawn is fine: `VtProgramStatus::new` mints under the io lock.

Fix: mint the disabled generation inside that lock hold, immediately before `publish`.

**4. P2 — The shutdown spawn check still claims the pane.** `crates/shux/src/pane_spawn.rs:648-672`, `crates/shux-core/src/graph.rs:1373-1397`

The token is checked, then `claim_pane_pty` is awaited, and only then is the token checked again. The claim clears `exit_status`, fires `pane.respawned`, and replaces `pty_owners` before the second check. The refusal kills the child and returns. It does not undo the claim. A cancel that arrives during that await still announces a respawn that never installs, or steals the owner from a live PTY whose later exit is ignored. `a_replaced_pty_task_refused_at_shutdown_leaves_the_exit_alone` only covers a token that is already cancelled.

Fix: a shutting-down flag that `claim_pane_pty` checks on the actor, set before the token is cancelled.

**5. P2 — `enabled = false` is not applied before the first spawn.** `crates/shux/src/daemon_boot.rs:412-416`, `crates/shux/src/pane_io.rs:159`

`PaneIoState` defaults the flag to `true`. The walker is spawned and not awaited; the attach server is spawned immediately after. Spawn reads the flag, not the config. Until the walker takes the lock, a pane is born enabled and will answer an OSC 7501 probe. The walker corrects it on its first pass. This only matters when the config turns the feature off.

Fix: set the flag from `ConfigHandle::current()` before either server starts.

## What is fine

The watch slot and the graph watermark both ignore an older generation. The watermark moves before the equality check, so a same-summary update cannot reopen an old generation. Enabled respawn mints under the lock, and the claim completes before the clear is published.

The publisher does not hold the io lock across the graph call. A flush abandons that call and the pace sleep. Every wait also selects the pane lifetime. `PaneNotFound` and `Shutdown` end the task. The exit RPC is outside the lock, and on one actor turn it applies status and then `pane.exited`. A later send of that same generation is dropped. The graph actor never takes the io lock, so the publisher cannot deadlock against it.

`wait_status` subscribes before the first read. An exited pane returns `matched` or `pane_gone` before `disabled`. `disabled` comes from the terminal. On a quiet bus the 250 ms re-read does see a disabled terminal that produced no event. CLI transport failure exits 3, permission denied exits 4, and text mode runs `title`, `msg`, and `app` through `sanitize_for_display`. `kind` and record ids are not free text. JSON goes through `json_safe`.

Shutdown collects VT and status keys and awaits the publisher joins after teardown queues them. Once the writer is gone, the walker does not publish for that pane. Default-on does not write the PTY unless the program sends a 7501 query. Programs that never speak 7501 still get one `pane.status_changed` per spawn, which the plan accepts, and no grid, title, or exit change.

Traced on the current tree (`a9eb205` plus the uncommitted diff), not executed in a scratch build.
