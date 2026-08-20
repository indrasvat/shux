# Adversarial review — the implementation diff

Third cold-context agent, and the first to review CODE rather than a design.
`dootsabha` is unavailable here; see `adversarial-review-apc.md` for why this
substitutes for it. This file is the **step 6** review (implementation diff);
the other two records are step 1 (design).

An earlier revision of `evidence-manifest.json` pointed `dootsabha_implementation`
at the second *design* review. That satisfied step 1 twice and step 6 not at all,
and is exactly the kind of mislabelling a manifest exists to prevent.

## Charter
Attack `git diff f071c89..HEAD` as written — especially the three commits no
review had seen: the QA harness, the memchr scanner rewrite, and the RIS fix.

## Verdict delivered
**DO-NOT-SHIP as-is**, with three blocking items and four that should land in the
same push. Every one is addressed in the commit that follows this file.

## What it could not break (stated, because it bounds the risk)
- **The memchr rewrite is behaviourally identical to the byte loop it replaced.**
  400,000 randomised chunked streams over a hostile alphabet, comparing cuts,
  `end` offsets, bodies and residual scanner state: **0 mismatches**. Plus an
  overflow sweep at `MAX-2 … MAX+2` across six chunk sizes: 0 mismatches.
- **`advance_slice` loses and duplicates nothing.** `VtHandler` holds only
  borrows and has no `Drop`; the `vte::Parser` — and with it the partial-UTF-8
  buffer, OSC buffer, params and state — is never rebuilt. `MAX_RESPONSES_PER_BATCH`
  remains per-batch: 600 queries with an APC between each yields exactly 512
  replies on base and on HEAD.
- **Bit-identity holds.** 120,000 randomised streams (87% APC-bearing) × 3
  geometries × 3 chunkings, plus all five corpus fixtures and 3.5 MB of recorded
  real streams including the 1.1 MB terminal-browser capture, base vs HEAD:
  **zero mismatches** across text, cursor, alt-screen, title, scrollback,
  content revision, palette, scroll region, default colours and reply stream.
- Commit-message numbers spot-checked: "48% of APCs straddle an 8192-byte read"
  is exact (140/289); "largest observed APC 4152 bytes" is exact; the three
  stock-vte-vs-strip-first rows in the module docs reproduce.

## Blocking findings, all fixed in the next commit
1. **`daemon stop` broke a documented contract** — two shipped skill docs promise
   `exit 0 when none is running`; the previous commit made it exit 1 with no
   recovery path, and `daemon status` disagreed with it. Reverted to exit 0 with
   a stderr warning, which keeps the contract and still surfaces the anomaly.
2. **The committed pixel evidence named a commit two VT-touching commits stale**,
   and its own `note` field said otherwise. Regenerated against the code commit.
3. **`SOLID-QA.md` absent**, so the hard gate fails. (Written only on a PASS.)

## Non-blocking findings, also fixed
4. **Scanner/vte divergence:** vte stays in Escape across C0, DEL and 8-bit
   bytes, so `ESC LF _` opens an APC that the scanner missed — false negatives
   only, but in code whose justification is "correct by construction". The
   `memmem("ESC _")` fast path had the same bug in worse form.
5. **The neutrality proptest could not reach vte's partial-UTF-8 buffering** —
   its alphabet was ASCII-only while its docstring called it "hostile". Widening
   it exposed that chunking-invariance is not a property shux has (a
   pre-existing vte defect, reproduced at base), so the property was replaced
   with a sliced-vs-unsliced oracle that isolates this code instead.
6. **The harness masked its own precondition** (`stty -echo 2>/dev/null`) and its
   leak scan compared an unresolved path, so it could pass having matched nothing.
7. **The env deny-list missed ZELLIJ entirely** and `LC_TERMINAL*`; families are
   now matched by prefix, and `WINDOW` is gated on `STY`.

## Also raised, and acted on
- The echo-determinism measurement was quoted in three places and reproducible
  from none: the harness lived only in scratch. It is committed here now, with
  its output, precisely so the next agent does not remove `stty -echo` again.
- `make test-vt` ran `--lib` only and skipped every shux-vt integration test —
  including the differential oracles. Now 619 tests instead of 376.
