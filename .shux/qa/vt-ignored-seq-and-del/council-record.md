# Council record — issues #126 and #127

`dootsabha` is not installed in the cloud session that made this change, so
both council steps used the documented fallback (CLAUDE.md, *Tooling
fallbacks*): parallel adversarial agents on disjoint surfaces, each driving the
real system. Every finding was reproduced before it was believed and fixed
with a test seen failing first.

## Design

Settled before coding, against `vte` 0.15 and Alacritty on the same release:

- `vte` raises `ignore` on more than 32 parameters (sub-parameters included)
  and on a third intermediate, then hands over what survived. shux discards
  such a CSI, ESC or DCS whole, as Alacritty does in `csi_dispatch`. A
  discarded sequence still ends a grapheme cluster exactly as an unrecognised
  one does.
- The owner's note on #127 that intermediates are "structurally safe" does not
  hold: `CSI ? 2026 $ $ p` arrives as DECRQM with `ignore` set and the base
  answered it.
- A scalar with no `unicode-width` width (DEL, U+0080..U+009F) is not a glyph:
  no cell, no cursor move, no REP source. A C1 that `vte` prints because its
  bytes were split across reads is routed to `execute`, so chunking cannot
  change the screen.
- No claim of xterm/VT500 parity: xterm may keep the first parameters of an
  overflowed CSI. Risk accepted and stated in the PR: a CSI with more than 32
  values is now discarded whole; the rich-TUI corpus peaks at 10.

## Implementation diff

Round one, four agents on disjoint surfaces (parser semantics, egress, rich-TUI
corpus, the e2e harness). Findings fixed:

- cluster rule: a discarded CSI/ESC must end the cluster like an unrecognised
  one — `a_discarded_csi_ends_the_cluster_like_an_unrecognised_one`,
  `a_discarded_esc_ends_the_cluster_like_an_unrecognised_one`.
- split UTF-8 C1 printed as a cell — `c1_controls_render_the_same_however_the_read_was_split`,
  `a_split_c1_control_ends_the_cluster_as_a_whole_one_does`.
- ESC and C1 tests that could not fail — `esc_dispatch_flagged_ignore_is_discarded`
  drives `esc_dispatch` with the flag directly.
- sub-parameter limit lacked an at-limit control — added.
- colour probes asserted as text only — `Env::assert_colour_probe` checks cell
  colours from `pane glance --cells`.
- e2e harness raced pane sizing — a go-file gate written after `set-size`.
- OSC 8 URI carrying a control was replayed verbatim by attach — dropped,
  `a_hyperlink_carrying_a_control_character_is_dropped`.

Corpus scan: no CSI over 10 slots, no three-intermediate sequence, no DEL or C1
in any rich-TUI fixture.

Round two, one agent on the raster change the visual evidence exposed
(combining marks drawn over the previous cell). It approved the re-promoted
`synthetic-grapheme-storage-current` golden in place of DootSabha approval,
probed 26 cluster shapes across four fonts at both commits, and reported two
residual cases — a mark straddling its origin (`i` + U+0336) and a mark on a
wide base — both fixed in `combining_mark_lands_on_its_own_base_cell`, seen
red first (18 px spill; mark absent from the wide base's second cell). Declined
with reason: anchor-positioned fonts (Liberation Mono) need a shaper.
