# DootSabha status for `kitty-graphics-render` (commit 8d4cc8f)

`dootsabha` is not installed on this host:

```
$ command -v dootsabha
(no output, exit 1)
```

CLAUDE.md's *Tooling fallbacks* table says a missing `dootsabha` never
downgrades steps 1 and 7 — the substitution is parallel adversarial agents on
disjoint surfaces, named in the PR.

**What the diff under audit actually carries: nothing.** `git show --stat
8d4cc8f` adds no council JSON, no substitution note, and no `.shux/qa/<scope>/`
directory at all. The previous work item in this series (`bff879a2`, PR #181)
did record its substitution, at
`.shux/qa/kitty-graphics-control-parse/council-substitution.md`; that document
covers commit `51ae4c9` and a different surface (APC scan and control-block
parse), not the renderer this audit is about.

This file records that absence. It is not a council artifact and must not be
read as one. The QA gate's `dootsabha_design` and `dootsabha_implementation`
manifest keys point here because the manifest requires a tracked path, and the
honest tracked path is a statement that the evidence does not exist.
