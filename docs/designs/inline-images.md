# Inline images — kitty graphics, natively emulated

Full design plan, with video proof of every capability claim:
<https://claude.ai/code/artifact/3e89a37e-fb48-40c4-9bc6-ca3c8493566e>

This file exists so the code can cite something a reader can open, and so the
PRD's superseded line has somewhere to point.

## The decision that supersedes the PRD

`docs/PRD.md` §6.1 originally specified **DCS passthrough for the focused
pane**, tmux-style, and `docs/tasks/055-image-passthrough.md` spells that
architecture out down to filenames. Both are superseded.

tmux's passthrough is `tty_add(); tty_invalidate();`. It has no clipping, so an
image spills past its pane; no redraw, so it vanishes on the next repaint; no
detach survival; and nothing in it can reach `pane snapshot`, `window snapshot`,
`session snapshot` or the event stream. shux emulates the protocol instead:
parse the command, hold the image, and re-emit under shux's own id namespace.

Precedent: Zellij (MIT). Target application: terminal-browser.

## Scope

Ships: `t=d` transport only, alt-screen placements anchored to an absolute
line, no animation. Deferred: inline images in shell scrollback (needs reflow
anchoring), Sixel, Unicode placeholder cells.

## Work items

| | Delivers | State |
|---|---|---|
| [#174](https://github.com/indrasvat/shux/issues/174) | pane pixel geometry + button events | landed, [#176](https://github.com/indrasvat/shux/pull/176) |
| [#175](https://github.com/indrasvat/shux/issues/175) | kitty-under-Xvfb verification rig | landed, [#178](https://github.com/indrasvat/shux/pull/178) |
| 3 | APC scan, control parse, refusals and bounds | this change |
| 4 | raster compositing — `pane snapshot`, `glance` | next |
| 5 | multi-pane composers — `window` / `session snapshot` | |
| 6 | attach re-transmit — humans see images | |

## Decisions this change implements

- **D1 — native emulation, no passthrough escape.** As above.
- **D2 — kitty protocol first.** Ghostty accepts only kitty. Sixel is the later
  fallback for VS Code and Windows Terminal. iTerm2's OSC 1337 has no reliable
  probe, no placement model and no deletion.
- **D11 — refuse `t=f`/`t=t`/`t=s`.** These name a FILE in the payload. Refusing
  the medium removes an arbitrary-file-read class outright. D11 also says to say
  so in a reply; that half waits for the renderer, because the protocol treats
  any response as an advertisement of support and a client that believes it
  abandons its text fallback.
