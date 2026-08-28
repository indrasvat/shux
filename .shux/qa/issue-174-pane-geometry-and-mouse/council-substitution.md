# Council substitution — issue #174

`dootsabha` is not installed in this environment. Per CLAUDE.md *Tooling
fallbacks*, the two mandatory council steps were not skipped; each ran as
parallel adversarial agents on disjoint surfaces driving the real system.

| Feature-protocol step | Substitution |
|---|---|
| 1. Council on the design, before coding | Parallel adversarial agents on disjoint surfaces (PTY winsize declaration; mouse routing/encoding; snapshot & compositor geometry). |
| 7. Council on the implementation diff, before pushing | Parallel adversarial agents re-run against the built tree; findings landed in `df3640d` ("close the findings both hard gates and the convergence review raised") and `6f8cf00`. |

Findings from the substituted councils that are visible in the diff:

- `apply_resize_to_window` re-fans every pane winsize on a live
  `appearance.border_style` reload (`crates/shux/src/attach.rs`), pinned by
  `a_border_style_reload_re_fans_every_pane_winsize`.
- `pane_rect_in` removes a second live-config read per mouse event.
- Verification-machinery defects: `issue_174_pixel_ab.sh` had no cleanup trap
  and leaked a daemon; its blank-image guard checked text rather than PNGs
  (now `.shux/scripts/lib/png_not_blank.py`); `pty_drive.py` reported success
  for a signal-killed child; the mutation battery refuses to run on a dirty
  tree.

The QA gate verified the *effect* of these steps in the diff and in fresh
measurements. It did not observe the council transcripts, which are not
committed.
