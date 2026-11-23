# Vision

Direction, not commitment. Features live in [`docs/roadmap.md`](docs/roadmap.md),
requirements in [`docs/PRD.md`](docs/PRD.md), working rules in
[`CLAUDE.md`](CLAUDE.md). Nothing here restates them.

## Thesis

tmux multiplexes byte streams for one human. The scarce resources now are
**attention** (many programs working, one human deciding) and **evidence** (what
each pane actually did, and which command produced it). A multiplexer that owns
its terminal model (`shux-vt`, `shux-raster`) can serve both. One that forwards
bytes cannot.

## Beliefs

1. **Unknown beats a guess.** A program that does not say what it is doing
   through a protocol is reported as unknown. shux never infers state from a
   vendor's spinner or prompt text.
2. **Bytes remember their author.** Every cell on screen was written by some
   command. The model keeps that link instead of discarding it at the grid.
3. **Authority only narrows.** Whoever hands power to another program hands
   less than it holds.

## Bets

### 1. Scrollback that remembers who wrote it

Each row binds to the command that produced it (OSC 133 boundary). Scrollback
becomes a queryable log: the output of command 7, or of the last three.
Nothing in shux exposes scrollback programmatically today
(`skills/shux/references/lens.md`: "v1 is viewport-only everywhere"), so agents
scrape the viewport.

*Landed when:* an agent fetches any earlier command's exact output by id after
the viewport has scrolled past it, in any pane whose shell reports command
boundaries, with nothing injected into the user's shell.

### 2. The inbox: layout follows attention

A pane that needs a human is a badge you must notice. Instead: one ordered queue of
everything awaiting a human (blocked with its kind, errored, finished) across
sessions and attached clients, and focus and layout that can follow it. shux
routes the question; the human answers it.

*Landed when:* with twenty agent panes running, a human clears every blocked
prompt in queue order without searching for it.

### 3. Runs are values

A run (input, output bytes, frame revisions, exit) is content-addressed and
compared with the same primitive that compares two frames. `shux-vt` is
deterministic, so recorded bytes re-render identically under a newer shux, a
different width or a different theme. "Did this agent behave as it did last
week" and "does this TUI still look right under the new renderer" become one
question. `lens gate` needs a scenario file written in advance. A run needs
only to have happened.

*Landed when:* two recordings of the same task are compared by identity, and a
difference arrives as an explained cell-level delta.

### 4. The pane tree is the authority tree

Plugins get a least-authority grant model
([`docs/designs/permissions`](docs/designs/permissions/README.md)). The real
actors are agents in panes launching agents in panes. Instead: a pane's credential
derives from its parent's and can only narrow (subtree only, read only,
expiring), a pane knows who it is ([#251](https://github.com/indrasvat/shux/issues/251)),
and the audit log records the chain.

*Landed when:* an agent hands a sub-agent a credential that provably cannot
touch the parent's panes, and the audit log answers "which grant allowed this
keystroke".

## Non-goals

- **Inference in the daemon.** No model calls, summarizers or output
  classifiers in core. Interpretation belongs to plugins and is labelled as such.
- **Vendor adapters.** shux never learns an agent's UI.
- **Orchestration.** shux does not plan or assign work.
- **Execution time travel.** Replay reproduces what was seen, not process
  state. No checkpointing.
- **Leaving the machine.** Recordings and frames are log-sensitive and stay
  local. Sharing is an explicit export.
