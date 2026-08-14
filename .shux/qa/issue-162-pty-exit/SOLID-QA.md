VERDICT: PASS

# issue #162 — a finished pane's output must be readable when its exit status is

Scope: the PTY output path (`crates/shux-pty/src/handle.rs`, `crates/shux/src/pane_spawn.rs`).
`make check-vt-qa`'s guard does not fire for this diff — it watches `shux-vt/`, `shux-raster/`
and `capture.rs` — but the gate table names "PTY output", so the evidence is owed anyway.

## Provenance — read this first

This audit was run **inline by the implementing agent**, not by the `shux-vt-solid-qa`
subagent, because subagent spawning is disabled in this session. The gate is designed to be
independent and this run was not. Every number below is reproducible from the commands
named; nothing here should be trusted more than those commands.

## What the change is

Two defects, both on the last read of a pane's life:

1. `drain_read` returned `Err` while holding bytes it had already read, and `EIO` — the PTY
   master's EOF on every unix — was only recognised as EOF on Linux, so only macOS reached
   that arm.
2. A tty discards whatever is still queued when its **last** slave fd closes. The parent
   dropped its slave at the end of `spawn`, so a child that wrote once and exited could have
   its bytes destroyed before the reader ran at all. The parent now holds that fd and
   releases it inside `read()` once the child is reaped and the master's queue is empty.

## Testing matrix this PR states, enforced

| claim | how it was checked | result |
|---|---|---|
| exit status never precedes readable output | `.shux/scripts/issue_162_evidence.sh`, 30 reps | 30/30 captures carried the probe |
| defect reproduces before the fix | same script, `EXPECT_DEFECT=1`, against HEAD built with the macOS EOF branch forced | 7/10 empty — baseline fails if the defect does *not* reproduce |
| Linux masks defect 1 | same script against unmodified HEAD | 10/10 — no Linux runner could have caught it |
| either half of the drain fix suffices | drain guard only, macOS branch forced | 10/10 |
| defect 2 is macOS-only in practice | macOS CI failed `the_focus_rescue_...` with an empty grid on 9bbb6dc (drain fix present) | fixed by 838768e — macOS CI green with the 20-rep test in it |
| colour survives the exit path | truecolor + 256-indexed + basic ANSI probe on every daemon-backed check | present in text and in pixels |
| wide cells and graphemes survive it | CJK, ZWJ emoji (`❤️‍🔥`), keycap (`1️⃣`) and a combining mark in the final chunk, 8 reps | 8/8 intact |
| rich TUIs do not regress | real `vim`, syntax on, line numbers, on a real file | renders; alt-screen restore on quit byte-identical to base |
| no leaked daemons | `.shux/scripts/no_leak_guard.sh` on every suite run; `/proc` argv scan by basename after each evidence script | none |

## Pixels

`pane snapshot` of an exited pane, 720×456, full resolution, opened and inspected:

- before (macOS branch forced): **2 distinct colours, 171 non-background pixels** — the
  cursor block alone, i.e. an empty pane.
- after: **342 distinct colours**, including `#78DCB4` (the truecolor the probe asked for)
  and `#FF8700` (256-indexed 208), read back out of the PNG.

There is no committed golden for this pane, so these metrics stand alone and no PNG is
committed with them — a screenshot nothing can be diffed against is not a baseline. The
images are in `.shux/out/issue-162/` (gitignored) and in the PR's proof artifact.

## Gates

`make check` (lint + 2129 tests), `make shellcheck` (95 scripts), `make check-vt-qa`
(self-test first, then "no evidence required" for this diff), `make check-test-groups`,
`make check-test-inventory`, `make deny`. Full suite run 5× sequentially on Linux with no
failure; base ran 3× for comparison. One earlier failure of
`a_failed_attach_split_or_new_window_leaves_no_phantom` was traced to two full suites
overlapping on a 4-core host, not to this diff.

## Why one green macOS run counts here

It would not have, before. The defect ran at roughly 1 in 5 per pane, so the earlier green
macOS run on 5745f76 proved nothing — and indeed the defect was still present. The
regression test now drives 20 panes per run, which puts a fully-present defect surviving a
green run at about 0.8²⁰ ≈ 1%. The `the_focus_rescue_...` pane is a 21st sample of the same
shape.

## Limits of this audit

- macOS behaviour is evidenced by CI runs, not by a machine this agent can drive. Defect 2
  was diagnosed from a CI failure and the platform's documented last-close semantics.
- Defect 1's reproduction on Linux uses a build with the macOS branch forced. That is the
  same code macOS compiles, but it is not macOS.
- The exit poll (20ms young, 200ms after 5s) was reasoned about, not profiled. If idle
  wakeups ever matter, a SIGCHLD-driven wait removes the poll entirely.
