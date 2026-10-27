# Council record — OSC 7501 program status

What the VT gate checked for DootSabha compliance. Pointers are to tracked
files at `cf96627`, or to the raw outputs copied next to this file.

## Design council (`dootsabha_design`)

`docs/designs/program-status/council-review.md`, rounds 1–4 (plan r1 → r6,
last edited in `cc9a29f`). Reviewers were codex through `dootsabha council`.
grok ran directly with the same prompt, because dootsabha's read-only sandbox
refused it (the refusal is recorded in round 1). Every round has a
per-finding disposition table. Rounds 3 and 4 have no P0. Each round-4 P1
on landed code has a named test that was seen failing first.

## Implementation-diff council (`dootsabha_implementation`)

Both reviews cover the P1 slice (`a9eb205`) and how it uses earlier commits.

- `council-impl-codex.json`: raw `dootsabha` JSON output, codex.
  **APPROVE-WITH-CHANGES**, four reproduced P2s and no P0/P1. Dispositions
  are in `e40af2a`, one test per finding.
- `council-impl-grok.md`: raw grok output on `a9eb205` plus the uncommitted
  `e40af2a` work. **APPROVE-WITH-CHANGES** with three P1s. The two that touch
  VT state were fixed:
  - P1 #3 (an already-off terminal keeps its generation) was fixed in
    `f02af0e` (`reasserting_off_moves_the_generation_forward`).
  - P1 #1 and #2 are daemon-side. `a84f650`'s message attributes its
    fixes for them to this review. This gate did not verify those fixes,
    because they are not VT scope.

## What no council reviewed

The commits after these reviews are `f02af0e`, `a84f650`, `77c5fc2`,
`4245bed`, `ba60310`, `12ca631` and `cf96627`. No council output on that
range is recorded. Their messages cite adversarial-review agents, but their
raw output is not in the tree. For the VT-gated part (`f02af0e`), this gate
substitutes its own review: it read the 13-line diff, and the linearity and
generation tests fail on `a9eb205` and pass on `cf96627`. It also ran the
14001-check vte-parity differential, a 120 s libFuzzer `scanner_vs_vte` run
and the replay pixels (see `SOLID-QA.md`). The daemon commits are outside VT
scope. They belong to `shux-tui-qa` and to the PR's step-7 row.

The final run at `398a8ab` adds two commits, and no council output is recorded
for either: `cafe577` (the `parse_hex_color` ASCII-hex guard) and `398a8ab`
(`impl Drop for PtyHandle`). This gate substitutes its own review for both.
- It read the full +58/−1 diff.
- Both new tests fail on `cf96627` for the right reason and pass on `398a8ab`.
- A VT replay of the crash and its variants, and a cross-tree check of 17 valid
  colour set/query cases, both pass.
- `make fuzz` ran 600 s per target, clean.
- Live panes were driven on `398a8ab` and on an A/B tree.

See `SOLID-QA.md` §4. The PR's step-7 row still owes a council or
adversarial-agent pass on `f02af0e..398a8ab` before push.
