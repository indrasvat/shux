---
name: adversarial-review
description: Spawn parallel adversarial subagents that DRIVE THE REAL SYSTEM (not reason from source) to break new code before it ships — schemas, contracts, parsers, serializers, protocols, state machines, security/redaction, freeze guards. Use as a standard step on any nontrivial feature/task once the implementation is green and BEFORE the final convergence (dootsabha) review. Each agent gets one disjoint attack surface + a "try to break X" charter; every finding is independently reproduced before it is believed, then fixed with a regression test. Trigger phrases include "adversarial review", "try to break this", "find the bugs I missed", "harden before done", "attack this schema/parser/contract", and the shux feature-protocol adversarial step.
---

# Adversarial review

Static review and design councils miss bugs that only surface when code meets real
input. This does the opposite: agents whose job is to **break** the code by driving
the real system. On shux task 078 this pattern found a shipped-blocking bug (a
validator that rejected ❤️ ⚠️ emoji) that both the author and three design councils
had passed over.

## When

Standard step for any nontrivial feature / schema / contract / parser / serializer /
protocol / state machine / security or freeze guard — **after it compiles green,
before the final dootsabha convergence review and before "done".** Skip only for
trivial or mechanical changes.

## Method

1. **Split the attack surface** into 2–4 **disjoint** areas (e.g. round-trip &
   canonicalization · the verdict/state model · the security/guard). One agent per
   area. Run them in **parallel, in the background.**
2. **Charter each agent** (template below): its ONE surface, the PROMISE to falsify,
   5–10 concrete hostile inputs, and the hard rule — **drive the real system; do not
   reason from the source alone.**
3. **Independently reproduce every finding** before believing it. Agents over- and
   under-claim; a confident report is a hypothesis. Turn each real defect into a
   failing test first.
4. **Fix + pin.** Every confirmed defect gets a fix AND a regression test (or a new
   proptest-generator case) so it cannot return.
5. **Feed forward.** Record which vectors were verified clean and any out-of-scope
   findings for the owning task.

## Charter template

> ADVERSARIAL review. Read-only; report findings, do not edit. Target: `<file / surface>`.
> **Drive the REAL system** — build a scratch harness with a path-dep, or run the real
> binary, and feed it hostile input. Do NOT reason from source alone; that is where the
> sharpest bugs hide.
> PROMISE to falsify: `<the invariant — e.g. "any real input round-trips losslessly",
> "exactly one canonical encoding", "masked content never leaks", "the guard cannot be
> bypassed">`.
> Attack vectors — for each, say REAL BUG or CORRECTLY HANDLED, with `file:line`, the
> exact triggering input, and whether an existing test catches it: `<5–10 concrete
> hostile inputs>`.
> Rank BLOCKER / MAJOR / MINOR. If the code is correct on an attack, say so in one line.
> End with a ranked defect list + a concrete failing test for the top 3.

## Discipline

- **Real system > reasoning.** The blocker on 078 came from an agent that RAN the real
  VT on `❤️`; static reasoning (and the author) dismissed the same vector as theoretical.
- **Your own self-checks are not a substitute.** Expect to be wrong about your own code
  — that is the entire point of the pass.
- **Verify before fixing.** Reproduce independently; downgrade what you can't.

## Anti-patterns

- Overlapping charters → agents duplicate work and leave gaps. Keep areas disjoint.
- "Review this file" → invites static reading. Always say **drive the real system**.
- One mega-agent → weaker than N focused parallel ones.
- Trusting a finding without an independent repro, or fixing without a regression test.
