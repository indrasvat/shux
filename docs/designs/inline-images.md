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
| 3 | APC scan, control parse, refusals and bounds | landed, [#181](https://github.com/indrasvat/shux/pull/181) |
| — | absolute-line anchoring made sound | landed, [#182](https://github.com/indrasvat/shux/pull/182) |
| 4 | raster compositing — `pane snapshot`, `glance` | this change |
| 5 | multi-pane composers — `window` / `session snapshot` | next |
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

## Decisions item 4 implements

- **D11's reply half, now that the renderer exists.** Answering `a=q` was
  deferred above for exactly one reason — a reply advertises support, and a
  client that believes it drops its text fallback. Item 4 IS the renderer, so
  the deferral ends here. Measured: default `kitten icat` probes each transport
  and waits on a DA1 sentinel; with no `OK` before it, it reports the terminal
  unsupported and transmits nothing at all. Only a DIRECT probe is answered —
  silence on `t=t`/`t=s` is what makes icat fall back to direct, so D11's
  refusal stays a refusal without needing a reply of its own.
- **Placements mutate the grid, but only through `sync::Presented`.** A picture
  moves the cursor, so the graphics path can no longer be write-free. It takes
  the synchronized-output freeze like every other presented-frame write, or a
  placement inside a `CSI ?2026h` window tears the redraw it landed in (#115).
- **`z` is accepted and not honoured.** Refusing is worse: unlike a transport
  refusal, a client told "no" here has no fallback and shows nothing.
