# 086 — Mouse Wheel Scroll (scrollback + app forwarding)

**Status:** Done
**Priority:** High (daily-driver human UX; PRD P0 mouse "scroll-for-scrollback")
**Milestone:** M3 polish
**Depends On:** 020, 021, 062
**Touches:** `crates/shux-vt/src/parser.rs`, `crates/shux/src/attach.rs`, `crates/shux-ui/src/copy_mode.rs` (reuse)

---

## Problem

The mouse scroll wheel does nothing in a normal shux pane. Triple-verified live
(synthetic PTY+pyte, real xterm+xdotool with byte-identical before/after PNGs,
and a screen-recorded video across `less`/`vim`/`htop`):

- **Normal-mode wheel is a no-op.** `handle_mouse` (`attach.rs`) has an explicit
  empty match arm for `ScrollUp`/`ScrollDown`. Scrollback only moves once the user
  manually enters copy mode (`Prefix [`).
- **Mouse events are never forwarded to the inner app.** No code path encodes a
  mouse event and writes it to a pane PTY, and the attach server never reads the
  VT's tracked `mouse_tracking`/`sgr_mouse`. So `vim :set mouse=a`, `htop`, and
  `less --mouse` receive nothing from the wheel.

The RPC protocol doc (`shux-rpc/src/attach.rs`) and PRD both say scroll SHOULD
scroll the focused pane's scrollback — this is a gap against contract, not a
nicety. Root cause is a single deferred hook that never got wired up (not a
recent regression).

## Fix — 3-tier wheel dispatch (tmux/wezterm model)

When a wheel event lands on a pane and shux is **not** already in copy mode,
resolve by the focused-under-cursor pane's live VT state:

1. **App requested the mouse** (`mouse_tracking != None`): encode the wheel as a
   mouse event (SGR when `sgr_mouse`, else X10), pane-local 1-based coords, and
   write it to that pane's PTY via the same writer channel keystrokes use.
   → `vim :set mouse=a`, `htop` scroll natively.
2. **App is on the alternate screen and did not request the mouse**
   (`alternate_screen && !mouse` and alt-scroll enabled): translate the wheel into
   arrow-key presses (3× CUU/CUD), honoring `application_cursor_keys` (`ESC O A`
   vs `ESC [ A`). → plain `less`/`man`/`vim` scroll.
3. **Primary screen, no mouse**: scroll shux's own scrollback by entering /
   advancing copy mode (reuse `copy_mode::scroll_up/scroll_down`, 3 lines/tick,
   tmux-style). Wheel-down reaching the live bottom exits back to live.

Copy-mode-active behavior is unchanged (`handle_copy_mode_mouse` already scrolls).

VT change: track DEC private mode `?1007` (alternate-scroll) in `TerminalModes`
as `alternate_scroll` (default **true** — Debian-xterm/iTerm2/kitty default;
makes pagers scroll out of the box).

### Design decisions
- **Reuse copy-mode state for tier 3** rather than a parallel "scrollback-lite"
  state — lowest render-path-drift risk; copy mode already renders in every path
  (attach/snapshot). Matches tmux (wheel enters copy mode). Keyboard-passthrough
  scrollback-lite is a possible future polish, out of scope here.
- **No modifiers on forwarded wheel** — the `Mouse` frame carries no modifier
  bits; Shift/Ctrl+wheel is not distinguished (acceptable for v1).
- Forward to the pane **under the cursor** (fallback active pane), tmux-style.

## Testing Matrix (render path × state)
| Path / state | Check |
|---|---|
| live attach — primary screen | wheel-up enters scrollback, wheel-down returns to live |
| live attach — `vim :set mouse=a` | wheel forwarded; view scrolls; no shux scrollback engaged |
| live attach — `htop` (alt+mouse) | wheel forwarded; selection moves |
| live attach — `less`/`man` (alt, no mouse) | wheel→arrows; pager scrolls |
| copy-mode active (unchanged) | wheel still scrolls copy viewport |
| snapshot path | scrollback view (tier 3) renders identically to attach |
| unit — encoder | SGR + X10 byte-exact for up/down at known coords |
| unit — tier selection | mouse→forward, alt→arrows, primary→scrollback |
| regression | test that FAILS on old code (old = no PTY write, no scrollback move) |

## Acceptance Criteria
- [ ] Wheel scrolls scrollback on the primary screen (no copy-mode entry needed).
- [ ] Wheel reaches mouse-aware apps (`vim mouse=a`, `htop`) as real mouse events.
- [ ] Wheel scrolls alt-screen non-mouse apps (`less`, `man`) via arrow translation.
- [ ] Copy-mode wheel behavior unchanged.
- [ ] `?1007` tracked; `alternate_scroll` defaults on.
- [ ] `make check` green; new unit + regression tests.

## Definition of Done
- All Acceptance Criteria met; Testing Matrix cells filled or explained.
- `shux-vt-solid-qa` PASS (VT parser touched) committed at `.shux/qa/086/SOLID-QA.md`.
- `shux-tui-qa` PASS (input/mouse UX touched).
- Real-target dogfood: after-video of the wheel scrolling `less`/`vim`/`htop` +
  shell scrollback through the real binary.
- PROGRESS.md + learnings updated; committed + pushed.
- dootsabha council / gh-ghent steps: N/A in this environment (not installed) —
  substituted with `adversarial-review` skill + the two QA gate sub-agents;
  noted in PR.

## Known limitations / follow-ups (validated vs wezterm/Alacritty/Ghostty)

Best-practices research (agent "Kathāsaritsāgara") confirmed all core encoding
decisions match the mature-terminal consensus (and shux is *more* correct than
Alacritty on the DECCKM SS3/CSI rule). Three enhancements are deferred because
they require a protocol/client change beyond this daemon-side fix:

- **Shift+wheel scrollback bypass.** In xterm/wezterm/Ghostty, Shift+wheel
  scrolls the terminal's own scrollback even while an app holds mouse mode. The
  `AttachClientFrame::Mouse` frame carries no modifier bits, so this needs the
  protocol + client (`shux-ui`) to forward modifiers. Deferred. (Mouse-aware
  apps have their own navigation, so the impact is limited.)
- **Horizontal wheel (buttons 66/67).** `shux-ui` currently drops
  `ScrollLeft`/`ScrollRight` before they reach the daemon; forwarding them to
  mouse-aware apps needs a client change. Deferred.
- **Trackpad precision / momentum damping.** The host terminal already quantizes
  wheel input into discrete `ScrollUp`/`ScrollDown` events before shux sees them,
  so this is largely handled upstream; a fast-scroll modifier is a future nicety.
- **Copy mode is session-global, not per-window** (pre-existing). `copy_mode`
  lives on `AttachedSession`, so switching windows while scrolled carries the
  copy indicator to the new window's active pane (agent Mudrārākṣasa's minor
  note). It clamps safely (no crash/corruption). This predates task 086 but is
  reached more often now that the wheel opens copy mode; a proper per-window
  copy-mode state is a separate follow-up.

## Adversarial review (agents driving the real system)

Four parallel breakers drove the real binary on disjoint surfaces (the
`adversarial-review` step of the Feature Protocol):
- **Vetālapañcaviṃśati** (app-forwarding): 8/8 PASS — byte-exact SGR/X10, correct
  pane-local coords in a shifted split, live per-event mode read (no cache bug).
- **Karpūramañjarī** (alt-scroll + rich-TUI): PASS — byte-probe confirmed 3
  arrows/tick, DECCKM SS3/CSI by live state, inert when `?1007` off; htop/top/
  vim/less render un-regressed.
- **Rājataraṅgiṇī** (no-regression): 6/6 PASS — click-focus, drag-resize, manual
  copy mode + yank, copy-mode wheel, keystroke forwarding, right-click menu.
- **Mudrārākṣasa** (scrollback edges): found ONE real bug — wheel-down could not
  exit wheel-opened scrollback (keyboard hijacked until `q`), because an active
  copy mode routes the wheel through `handle_copy_mode_mouse`, which lacked the
  exit-at-bottom check. **Fixed** with the `wheel_initiated` flag on
  `CopyModeState` + exit logic in that handler; regression tests
  `wheel_initiated_scrollback_exits_when_wheeled_back_to_bottom` (red→green) and
  `manual_copy_mode_survives_wheel_back_to_bottom` (guards over-fixing).
