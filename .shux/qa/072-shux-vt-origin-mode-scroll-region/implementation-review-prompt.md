Review this shux task-072 implementation diff for correctness gaps.

Task: docs/tasks/072-shux-vt-origin-mode-scroll-region.md
Design: .shux/qa/072-shux-vt-origin-mode-scroll-region/DESIGN.md
Diff: .shux/qa/072-shux-vt-origin-mode-scroll-region/implementation-diff.txt

Focus only on defects that would block merging:
- DECOM origin-mode cursor addressing for CUP/HVP/VPA.
- DECSET/DECRST ?6 homing side effects.
- DECSTBM scroll-region validation and homing.
- CPR/DSR origin-relative row reporting.
- CUU/CUD/CNL/CPL/VPR scroll-margin clamping.
- Save/restore restoring origin mode without clamping saved absolute cursor to current margins.
- Corpus fixture, shux automation, exact pixel evidence, and baseline promotion separation.

Return concise findings with severity. If no blocker remains, say mergeable.
