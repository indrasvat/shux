# Regenerating `assets/SymbolsNerdFontSubset.ttf`

The bundled `SymbolsNerdFontSubset.ttf` is a 4.8 KB subset of the upstream
[Nerd Fonts `SymbolsNerdFontMono-Regular.ttf`](https://github.com/ryanoasis/nerd-fonts)
trimmed to the codepoints shux actually renders in its status bar (plus a
curated set of future-proof glyphs). The full upstream font is 2.4 MB; subsetting
keeps the binary lean without sacrificing OOTB Nerd Font support.

## When to regenerate

When a new status-bar icon needs a Nerd Font glyph that isn't in the current
subset (see `assets/SymbolsNerdFontSubset.ttf` cmap), add its codepoint to the
list below and rerun.

## Steps

```bash
# 1. Install fonttools (one-time)
uv tool install fonttools

# 2. Download the latest upstream NF symbols-only font
curl -fsSL -o /tmp/nf-symbols.zip \
  https://github.com/ryanoasis/nerd-fonts/releases/latest/download/NerdFontsSymbolsOnly.zip
unzip -p /tmp/nf-symbols.zip SymbolsNerdFontMono-Regular.ttf \
  > /tmp/SymbolsNerdFontMono-Regular.ttf

# 3. Subset to the codepoints we use + the curated future-proof set.
# Keep these sorted by codepoint; add new entries to the END of the list
# (the build script is order-independent but humans diff easier).
CODEPOINTS="\
U+E0A0,\
U+F002,U+F004,U+F005,U+F013,U+F015,U+F017,U+F02C,U+F07B,U+F08E,\
U+F09B,U+F0C5,U+F0E7,U+F0EB,U+F15B,U+F1C0,U+F46B,U+F489,U+F49B"

pyftsubset /tmp/SymbolsNerdFontMono-Regular.ttf \
  --output-file=crates/shux-raster/assets/SymbolsNerdFontSubset.ttf \
  --unicodes="$CODEPOINTS" \
  --layout-features='' \
  --no-hinting \
  --desubroutinize

ls -la crates/shux-raster/assets/SymbolsNerdFontSubset.ttf
# Should be ~5 KB.
```

## What's in the current subset

| Codepoint | Glyph | Use |
|---|---|---|
| U+E0A0 | `` | Git branch (status bar LEFT) |
| U+F002 |  | Search (reserved) |
| U+F004 |  | Heart (reserved) |
| U+F005 |  | Star (reserved) |
| U+F013 |  | Cog / settings (reserved) |
| U+F015 |  | Home / SSH host (status bar LEFT) |
| U+F017 |  | Clock (reserved) |
| U+F02C |  | Tags (reserved) |
| U+F07B |  | Folder (reserved) |
| U+F08E |  | External link (reserved) |
| U+F09B |  | GitHub (reserved) |
| U+F0C5 |  | Files-stack (reserved) |
| U+F0E7 |  | Bolt (reserved) |
| U+F0EB |  | Lightbulb (reserved) |
| U+F15B |  | File (reserved) |
| U+F1C0 |  | Database (reserved) |
| U+F46B |  | Cube / package (reserved) |
| U+F489 |  | Terminal (status bar LEFT + welcome toast title) |
| U+F49B |  | Octahedron (reserved) |

## Licensing

The output `SymbolsNerdFontSubset.ttf` inherits the SIL Open Font License
of the upstream Nerd Fonts project. See `assets/OFL.txt`. The OFL allows
modification (subsetting), so committing the trimmed file is OK; no
attribution beyond the existing OFL.txt is required.
