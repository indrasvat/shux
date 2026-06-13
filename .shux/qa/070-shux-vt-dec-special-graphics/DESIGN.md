# Task 070 Design: DEC Special Graphics Charset

## Goal

Render common VT100 DEC special graphics output as Unicode line drawing instead
of literal ASCII letters. This closes the ncurses/classic TUI path where apps
emit `ESC ( 0`, `ESC ) 0`, SO/SI, and ASCII bytes such as `lqkxmj` for boxes.

## Existing State

- `crates/shux-vt/src/parser.rs` has no charset state.
- `execute()` currently treats SO (`0x0e`) and SI (`0x0f`) as unhandled C0.
- `esc_dispatch()` currently ignores `ESC ( 0`, `ESC ) 0`, `ESC ( B`,
  and `ESC ) B`.
- The synthetic `dec-special-graphics-current` fixture tracks the old behavior
  where `ESC ( 0 lqk ESC ( B` renders as `lqk`.

## Proposed Shape

Add a small ISO-2022 subset only for the VT100 DEC Special Graphics case:

- `Charset` enum: `Ascii`, `DecSpecialGraphics`.
- `Charsets` state on `VirtualTerminal`: `g0`, `g1`, and `active`.
- Default state: `g0 = Ascii`, `g1 = Ascii`, `active = G0`.
- `RIS` resets charset state to defaults.
- DECSC/DECRC save and restore charset state alongside cursor state.
- `ESC ( 0` designates G0 as DEC Special Graphics.
- `ESC ) 0` designates G1 as DEC Special Graphics.
- `ESC ( B` designates G0 as ASCII.
- `ESC ) B` designates G1 as ASCII.
- Unsupported G0/G1 designations fall back to ASCII, which is a safe no-op
  render for national replacement / UK / DEC technical probes.
- `SI` selects G0; `SO` selects G1.
- Printable characters pass through `translate_printable()` before `write_char`.

Do not implement full ISO-2022 designation, G2/G3, locking shifts beyond
SO/SI, single-shift controls, GR/96-charsets, national replacement charsets,
DECSTR, or locale-specific legacy charsets.

## Mapping

Implement the common xterm/VT100 DEC graphics map for printable ASCII bytes:

```text
_      ` ◆    a ▒    b ␉    c ␌    d ␍    e ␊    f °    g ±    h ␤
i ␋    j ┘    k ┐    l ┌    m └    n ┼    o ⎺    p ⎻    q ─    r ⎼
s ⎽    t ├    u ┤    v ┴    w ┬    x │    y ≤    z ≥    { π    | ≠
} £    ~ ·
```

Unmapped characters stay unchanged. This preserves ordinary text if an app
misuses the designation or emits unsupported bytes.

## Tests

Unit:

- G0 `ESC ( 0` maps `lqkxmj` to `┌─┐│└┘`.
- G1 `ESC ) 0` maps only while SO is active and returns to G0 on SI.
- Charset state persists when designation and payload arrive in separate
  `process()` calls.
- Dynamic re-designation changes the active slot immediately.
- `ESC ( B` / `ESC ) B` restore ASCII and do not leak.
- Unsupported G0/G1 designations fall back to ASCII safely.
- REP repeats already translated glyphs without double translation.
- Wide Unicode emitted while DEC graphics is active remains width-correct.
- Unmapped characters pass through.
- `RIS` resets charset state.

Integration / corpus:

- Replace the current wrong-behavior DEC fixture with a post-070 fixture whose
  expected text/PNG shows Unicode line drawing. The DootSabha design council
  approved the expected glyph content in `dootsabha-design.json`; the expected
  PNG files are committed under `.shux/goldens/070-dec-special-graphics/`
  before SOLID QA.
- Keep `wide-cjk-ansi-dec-edit` stable except its DEC `lqk` line becomes box
  drawing.
- Add/adjust a pane capture integration test proving public `pane.capture`
  returns Unicode box drawing for a DEC fixture.

Shux automation:

- Add a `make test-vt-dec-special-graphics` target backed by a script.
- Generate a deterministic DEC stress screen at 80x24, 120x40, and 200x60.
- Save full-resolution actual/expected/diff PNGs and pixel JSON under
  `.shux/qa/070-dec-special-graphics/`.

QA:

- DootSabha design council saved before coding.
- DootSabha implementation diff review saved before completion.
- SOLID QA report first line exactly `VERDICT: PASS`.
- Evidence manifest references design review, implementation review,
  screenshots, pixel metrics, and SOLID report.

## Risks

- Mapping after Rust `char` decoding must only affect ASCII-range printable
  characters. Non-ASCII Unicode box drawing already emitted by modern apps must
  remain untouched.
- Active charset state must not attach to zero-width combining logic in a way
  that corrupts grapheme anchors.
- SO/SI must clear active grapheme anchors just like other control sequences.
- Reset paths must restore charset state, otherwise DEC mode can leak between
  app startup/teardown streams.
- Do not reset charset state on alternate-screen enter/leave; xterm treats it
  as terminal state, not buffer-local grid state.
