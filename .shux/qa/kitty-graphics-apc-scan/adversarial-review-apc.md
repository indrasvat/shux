# Adversarial review — APC handling surface

`dootsabha` is not available in this environment. Per CLAUDE.md's *Tooling
fallbacks* table, the council step was served by **parallel cold-context
adversarial agents on disjoint surfaces**, each driving the real system rather
than reasoning from source. This file records the surface-A review; the protocol
surface is in `adversarial-review-protocol.md`.

## Charter
Attack the proposed design for extracting kitty-graphics APC sequences, which
touches every byte of every pane in the multiplexer.

## Verdict delivered
**"Section A as written must be replaced, not patched."**

The original design — strip APC bytes out of the stream before handing the rest
to vte — is wrong at the premise. Deleting bytes from a stream feeding a state
machine changes that machine's state. vte leaves a string state on `ESC <any>`,
`CAN` (0x18) and `SUB` (0x1A), not only on `ESC \`
(vte-0.15 `src/lib.rs:182`, `:438-450`).

## Findings reproduced independently before acting on them
A divergence harness ran the proposed splitter and stock vte side by side.
**12 of 13 hand cases diverged.** The three classes:

| input | stock vte | strip-first splitter |
|---|---|---|
| `ESC [ 3 ESC _ G x ESC \ HELLO` | CSI aborted, prints `HELLO` | **synthesizes `CSI 3 H`** — a cursor jump invented from nothing |
| `ESC _ G broken` + `ESC [ 31 m RED …` | full coloured shell output | **everything swallowed** |
| `ESC ] 0 ; title ESC _ G a=q ; ESC \ HELLO` | title set, prints `HELLO` | title **and** text lost |
| `ESC P + q 544e ESC _ G a=q ; ESC \ HELLO` | DCS ends, prints `HELLO` | `HELLO` becomes DCS payload |
| `ESC _ G a=T ; AAAA CAN HELLO` | prints `HELLO` | swallowed |

Two data claims were also re-derived from the repo itself rather than taken on
trust:

- **C1 hazard.** `.shux/fixtures/vt-corpus/rich-tui/vivecaka.raw` contains six
  `0x9F` bytes at offsets 3258, 5699, 5753, 5930, 5983, 6267 — every one a UTF-8
  continuation byte of `U+27A0`/`U+27D0`/`U+27C1`/`U+27E1`. A byte-level scan for
  an 8-bit APC introducer would eat those glyphs. 8-bit C1 is therefore out of
  scope, stated in the module docs.
- **Chunk boundaries are the common case.** In the recorded terminal-browser
  stream, **140 of 289 APCs (48%)** straddle an 8192-byte PTY read
  (`crates/shux-pty/src/manager.rs:58`); largest APC 4152 bytes.

## Also found: the originally-proposed tests had no teeth
The stated properties ("splitter output concatenated == input with APCs
removed") use the splitter's own notion of an APC as their oracle, and pass on a
splitter that eats text. Separately, the existing VT corpus cannot serve as a
regression bed for this: 5 files / 99 KB containing **zero** `ESC _`, `ESC X`,
`ESC ^`, CAN or SUB.

This was confirmed the hard way during implementation: a first draft of the abort
tests asserted "nothing emitted" on streams that never terminate, which is true
for the correct *and* the broken scanner. Mutating the scanner left them green.
They were rewritten to place a well-formed terminator later in the stream, and
then proved red by mutation.

## Resolution adopted
**Slice, don't strip.** vte receives every byte unchanged; only its `advance`
calls are cut at APC boundaries. The text path is bit-identical by construction,
so every divergence above is structurally impossible rather than merely tested,
and a scanner false positive costs at most a spurious image.
