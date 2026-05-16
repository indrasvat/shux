# Font fallback for PNG snapshots — design proposal

**Tracking:** https://github.com/indrasvat/shux/issues/46
**Author:** indrasvat
**Status:** Draft — council review pending

## Problem

`shux pane snapshot` / `window.snapshot` / `session.snapshot` produce PNGs
that drop emoji glyphs (`🍺`, `🧩`, `🛠️`, etc. render as tofu or blank).
`shux pane capture` preserves them because it just dumps the VT grid text.
The user's terminal renders emoji fine on live `attach` because the
terminal owns the font stack; the snapshot path is the only render path
that lacks emoji glyph coverage.

Secondary bug: `crates/shux/src/config_validate.rs::strict::Appearance`
only declares `border_style`, so `shux config validate` rejects
`nerd_fonts` and `font` — both of which the runtime config (`shux-core`)
accepts. The user's config typo at issue #46 reproduces this drift.

## Current state (1-paragraph each)

**Rasterizer (`crates/shux-raster/src/lib.rs`).** `Rasterizer` holds an
ordered `Vec<Font>` (fontdue). `with_fonts(size, iter)` accepts a chain;
`with_primary_font(size, primary)` is sugar for `[primary, BUNDLED_NF]`.
`font_for(ch)` linear-scans the chain for the first font whose cmap has
a non-zero glyph index. Cell metrics derive from `fonts[0]` only. Bundled
font is JetBrains Mono Nerd Font Mono Regular (2.4 MB OFL). It covers all
NF private-use codepoints but **does not cover the emoji blocks**
(U+1F300–U+1FAFF, U+2600–U+27BF colour variants, etc.).

**Snapshot wiring (`crates/shux/src/main.rs:3000`).** Daemon builds one
`Arc<Rasterizer>` at startup, reading `appearance.font` and falling back
to `Rasterizer::new(14.0)`. Reused for every snapshot RPC call. Hot
reload of `[appearance]` does **not** rebuild the rasterizer (documented
limitation). Live `attach` does not use this rasterizer at all.

**Config schema runtime (`crates/shux-core/src/config.rs:55–89`).**
`AppearanceConfig { border_style: String, nerd_fonts: bool, font: Option<PathBuf> }`.
Lenient parse (`Config` is NOT `deny_unknown_fields` in shux-core — by
design, daemons must not die on unknown keys).

**Config schema validator (`crates/shux/src/config_validate.rs:54–57`).**
Strict mirror `strict::Appearance { border_style: Option<String> }` —
**missing `nerd_fonts` and `font`**. That's the validator gap.

**Rendering library: `fontdue` 0.10.** Grayscale-only. Does NOT support
COLRv0/v1, CBDT, SBIX, or SVG-in-OpenType. Colour emoji fonts (Apple
Color Emoji, Noto Color Emoji, Twemoji) will either fail to load or
render only the monochrome `.notdef` fallback.

## Decision points the council should resolve

### D1 — Colour or monochrome emoji?

| Option | What | Cost | Render quality |
|---|---|---|---|
| **A. Monochrome** | Bundle Noto Emoji (monochrome, ~280 KB subset, OFL) as a third fallback entry. Keep fontdue. | One new bundled font, ~280 KB binary growth. Zero new deps. | Emoji render in grayscale on a monospace cell. Legible, correct width, no tofu. |
| **B. Colour via swash** | Replace fontdue in `shux-raster` with `swash` 0.2 (active, COLRv1 + CBDT/SBIX + bitmap support). Bundle Twemoji Mozilla COLR (~1.1 MB MIT). | New dep, rewrite the per-cell render path (~150 LOC). +0.8 MB binary growth on top of A. | Full colour emoji matches Twitter/GitHub aesthetic. Cross-platform identical (no system font resolution). |
| **C. Hybrid (fontdue + bitmap composite)** | Keep fontdue for text. Detect emoji codepoints separately, blit pre-rendered Twemoji PNG sprites. | New sprite asset (~600 KB), custom emoji-detection code path. | Colour emoji at one fixed size. Brittle around variation selectors and ZWJ sequences. |
| **D. Defer to system fonts** | Try platform emoji font at runtime (Apple Color Emoji on macOS, Noto Color Emoji on Linux). | fontdue can't render them. Would still need swash → collapses to B with extra fallback complexity. | N/A — same end-state as B. |

**Recommendation hypothesis (open for debate):** A is the right v1.
Ships fast, fixes the user complaint at ~80% (no more tofu, spacing
preserved), zero new deps. Promote to B in a later PR if/when colour
emoji proves to be the dominant ask. Snapshots are documentation/proof
artifacts; legibility beats colour fidelity for the primary use case.

### D2 — Configuration surface

Issue suggests:

```toml
[appearance]
font = "/path/to/primary.ttf"
font_fallbacks = [
  "builtin:jetbrains-mono-nerd-font",
  "system:emoji",
]
```

**Proposed (concrete):**

```toml
[appearance]
font = "/path/to/primary.ttf"          # existing; primary text font
font_fallbacks = [                      # NEW: ordered fallback chain
  "builtin:nerd-font",                  # the bundled JBM NF (always available)
  "builtin:emoji",                      # bundled Noto Emoji (option A)
  # "/abs/path/to/symbola.ttf",         # user-supplied absolute path
]
```

Default value when `font_fallbacks` is omitted: `["builtin:nerd-font", "builtin:emoji"]`.
Snapshot rasterizer chain becomes: `[user_font?, ...resolved(font_fallbacks)]`.
`builtin:*` is the explicit allow-list; unknown values warn-and-skip in
the daemon log (don't die). `system:emoji` deferred — fontdue can't load
Apple Color Emoji anyway.

Validator strict mirror gains all three: `nerd_fonts`, `font`,
`font_fallbacks: Vec<String>`.

### D3 — When does the rasterizer rebuild?

Today: daemon-startup only. Hot-reload of `[appearance]` re-renders but
doesn't rebuild the rasterizer.

**Options:**
- **D3a.** Keep current behaviour. Document explicitly that font changes
  need daemon restart. Cheapest.
- **D3b.** Rebuild rasterizer on `config.reload` if any appearance.font*
  field changed. Snapshot path picks up new chain on next call.
  Moderate complexity (Arc swap of the rasterizer behind a Mutex).

**Recommendation hypothesis:** D3b. Issue #46 reporter's first reaction
to a config change will be to retry the snapshot. Restart-required UX
is poor for a discovery-heavy field like fonts.

### D4 — Verification matrix

Per CLAUDE.md Feature Protocol, every render path × every config state
needs verified output. For this feature:

- Render paths: `pane.snapshot`, `window.snapshot`, `session.snapshot`
  (the three that the bug touches). Live `attach` is unaffected.
- Config states: default, `shux config init`, fully-configured
  (`font` + `font_fallbacks` set), malformed
  (`font_fallbacks = ["builtin:does-not-exist"]`), hot-reload.

Cross-path consistency test: at width W, `pane.snapshot` and
`session.snapshot` of a 1×1 layout must produce byte-identical PNGs
when the snapshot focuses the same pane.

Tofu-free assertion test: for each codepoint in
`important_glyphs_for_bundled_font()` PLUS a curated emoji set
(`🍺 🧩 🛠️ ⚡ ✓ ✗ 🦀 📦 🚀`), `rasterizer.has_glyph()` must return true
and `rasterizer.glyph_pixel_count()` must be > 0.

### D5 — Licence + binary size budget

Noto Emoji monochrome subset: OFL-1.1. Compatible with shux's OFL/MIT
deps. ~280 KB bundled. Total binary growth: ~0.3 MB.
Twemoji Mozilla COLR: MIT. ~1.1 MB. Cumulative ~1.4 MB if option B chosen.

Current `target/release/shux` binary: ~28 MB. The deltas are 1–5%.
Acceptable for both A and B by the order-of-magnitude heuristic, but
worth confirming: do we have a stated binary size budget anywhere
(roadmap, PRD, decision log)?

## Open questions for council

1. **Colour vs monochrome (D1):** Recommend A (monochrome via Noto
   Emoji) for v1, B as future work. Counter-argue.
2. **Config surface (D2):** `builtin:nerd-font` + `builtin:emoji` as the
   default chain — is `font_fallbacks` the right TOML key, or should it
   be `font.fallbacks` (sub-table)? Should `nerd_fonts: bool` be
   deprecated in favour of explicitly listing `builtin:nerd-font` in
   `font_fallbacks`?
3. **Hot-reload (D3):** Recommend D3b (rebuild on reload). Worth it, or
   keep daemon-restart for v1?
4. **Validator gap fix scope:** Fix only `nerd_fonts` + `font` +
   `font_fallbacks` in `config_validate.rs::strict::Appearance`, or
   audit the entire strict mirror against runtime `Config` to surface
   other drift?
5. **Tofu-free test:** Should the curated emoji set live in
   `crates/shux-raster/src/lib.rs::important_glyphs_for_bundled_font()`
   or in a new `important_emoji_glyphs()` companion?
6. **Anything I'm missing.** Particularly: variation-selector handling
   (`🛠️` is `🛠` + U+FE0F), ZWJ sequences (`👨‍💻`), wide-cell width math
   for monospace emoji rendering.

Please critique sharply. The goal is a PR that ships solid the first
time, not a PR that triggers a P2 review.
