# Council substitution — `dootsabha` unavailable

`dootsabha` is not installed in this environment (`command -v dootsabha` → not
found). CLAUDE.md *Tooling fallbacks* requires the step not be skipped:

> Spawn context-appropriate **parallel adversarial agents on disjoint surfaces**
> that drive the real system. Reproduce every finding before believing it; fix
> with a test seen failing first.

Both council steps were run that way. This file is the record the manifest names
for `dootsabha_design` and `dootsabha_implementation`.

## Step 1 — design council, before coding

Two agents on disjoint surfaces.

**Agent A — is the slice right?** Attacked the proposal to split work item 3
into a scanner-only PR plus a parser PR. Verdict: the split was wrong. A
scanner-only PR would ship a production build that heap-copies attacker bytes to
feed a no-op, with a neutrality suite whose own header said it breaks on the
next PR. Counter-proposal, adopted: **cut where the risk changes, not where the
layers do** — scanner + control parse + refusals in one change, pixels in the
next.

Also found, and confirmed by measurement before acting: a shipping comment
claimed the five rich-TUI fixtures contain none of the scanner's hostile bytes.
They contain 1,902 / 1,010 / 760 / 747 / 87 `ESC`. Every real read takes the
slow path — the opposite of what the comment said.

**Agent B — does the ported design still fit today's `main`?** Built the port in
a scratch tree and ran it. Found that copying the branch's `parser.rs` would
silently revert 47 lines of #176's mouse-mode tracking; that the branch's
`shux-pty` env scrub and `daemon_boot` changes are already on `main` in strictly
better form; and that `Cargo.lock`'s real delta is one line. All acted on: the
port is additive-only, and `parser.rs` receives a five-line comment and nothing
else.

**A finding refuted rather than applied.** Agent B reported that shux and vte
disagree about the 8-bit ST (`0x9C`), silently dropping commands. Reproduced with
a positive control: vte does not terminate the string there either — `AFTER`
does not render on either side. They agree. Teaching the scanner to treat `0x9C`
as ST would have **created** the divergence the finding claimed to remove.
Pinned by `only_the_7bit_st_terminates_an_apc_and_vte_agrees`.

## Step 5 — adversarial review, on the real system

Two agents, disjoint surfaces, each in its own git worktree.

**Scanner state machine.** Could not falsify neutrality: ~39.7M compared
observable dumps, 5,466 randomised corpus runs, 125 real rich-TUI replays,
exhaustive 1- and 2-way splits, and a differential oracle against a build with
no scanner at all. Its two findings were about the verification machinery, and
both are fixed: `Observable` compared characters only, so a one-line pen reset
at the dispatch seam survived all 641 tests; and `dispatch_graphics` took
`&mut self`, giving it unwrapped access to presented state.

**Parser and refusals.** Could not falsify the two claims that matter: no input
gets a file-backed transport accepted (full 0–255 sweep of `t=`, unicode
lookalikes, duplicate keys, keys after the separator, a sweep of all 256
single-byte keys for an alias), and no default-constructed terminal emits a
byte. It falsified the kitty-parity claim in six places, all fixed or
deliberately declined with the divergence documented.

## Step 7 — implementation-diff council, before push

One agent on the merged diff, static-only (a hard QA gate was running
daemon-backed suites concurrently, and those must run serially).

Returned **"not ready to push"** and was right: `make check-vt-qa` was red and
the previous commit's gate list quietly omitted it; the branch superseded
`docs/PRD.md` §6.1 without saying so; the security paragraph was written in the
present tense about a pipeline that does not exist; three kitty citations were
wrong, including one that turned out to strengthen the argument
(`graphics.c:701` gates on `transmission_type != 's'`, so kitty applies no
permission check to shared memory at all); and a regression row had been
defanged by the same commit that added it. All fixed in `3870b19` and after.

## Step 6 — simplification, twice

`shux-simplify-architect` at `b8b7a26` and again at `bc75fc8` once the ~200-line
threshold re-armed. The first pass's headline — delete the whole reply subsystem
— converged with the adversarial parser findings from the opposite direction and
became the spine of the change. The second measured that the first's remediation
was itself 51% comment, three times the house norm.
