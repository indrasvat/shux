# 091 — DootSabha implementation-diff council: N/A in this environment, substituted

**Status:** `N/A-substituted`

Feature-protocol step 6 (a council on the implementation diff, before pushing) could not
be run — see `dootsabha-design.md`. What carries it instead:

1. **The implementer's mutation check**, `.shux/scripts/issue_122_mutation_check.sh`:
   16 mutations of the shipped fix, re-run independently by this gate. **16 killed,
   0 survived**, each naming the test that killed it.
2. **This gate's own mutations**, designed without reference to that list: 9 further
   mutations across the clamp, the degenerate wide-character path, pending auto-wrap,
   the private-marker guard, and alternate-screen entry/exit. 7 killed, 2 survived —
   both recorded as P3 coverage gaps in `SOLID-QA.md`, both confirmed observable by a
   probe binary rather than asserted from source.
3. **A deterministic A/B replay** of the five committed rich-TUI raw PTY recordings
   through both binaries: byte-identical text, cells and pixels.

**This is a substitution, not a waiver.**
