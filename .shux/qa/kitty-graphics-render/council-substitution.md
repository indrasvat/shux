# DootSabha status for `kitty-graphics-render` (commits 8d4cc8f · 10d9cbf · 4238f6b)

`dootsabha` is not installed on this host, at either audit:

```
$ command -v dootsabha
(no output, exit 1)
```

CLAUDE.md's *Tooling fallbacks* table says a missing `dootsabha` never
downgrades feature-protocol steps 1 (design council) and 7 (implementation-diff
council) — the substitution is parallel adversarial agents on disjoint surfaces,
named in the PR.

## What this branch carries

**Design (step 1).** A written design record exists — `docs/designs/inline-images.md`,
with the superseded-PRD argument, the scope line, the work-item table, and the
D1/D2/D11 plus "Decisions item 4 implements" sections that `10d9cbf` added. It
is a design *document*, not a council and not an adversarial pass. No council
JSON and no substitution artifact for the design step is in the diff.

**Implementation diff (step 7).** No council JSON either. What the diff does
carry is two independent SOLID VT QA runs against the real system, on disjoint
surfaces from the implementer's own testing:

| Run | Commit | Verdict | Outcome |
|---|---|---|---|
| 1 | `8d4cc8f` | FAIL | P1-1 alt-screen placement leak into `vim`; P1-2 `a=d` bypassing the synchronized-output freeze; P1-3 missing evidence; P2-1/P2-2/P2-3 and four P3s |
| 2 | `4238f6b` | PASS | every P1 and both actionable P2s re-measured fixed, each A/B'd against a one-hunk revert; five notes recorded |

Each run drove real panes, real `kitten icat`, real installed TUIs and the raw
replay corpus; each finding was reproduced before it was reported, and both
production fixes shipped with a test observed failing on the unfixed tree. That
is materially the *implementation-diff* half of the fallback. It is **not** the
design half, and one gate is not "parallel agents".

## What this file is not

This is a record of the substitution and of what is still missing. It is not a
council artifact and must not be read as one. The QA manifest's
`dootsabha_design` and `dootsabha_implementation` keys point here because the
manifest requires a tracked path and this is the honest one.

`gh` is not installed on this host either (`gh: command not found`), so the audit
could not read the PR description to see whether the substitution is named there
as CLAUDE.md requires. A reviewer must check that. If the PR does not name it,
step 1 is unmet.

The previous work item in this series (`bff879a2`, PR #181) recorded its own
substitution at `.shux/qa/kitty-graphics-control-parse/council-substitution.md`;
that document covers a different commit and a different surface (APC scan and
control-block parse), not the renderer audited here.
