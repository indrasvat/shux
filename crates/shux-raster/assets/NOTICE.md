# Bundled font attribution

`JetBrainsMonoNerdFontMono-Regular.ttf` is the upstream Nerd Fonts patched
build of JetBrains Mono Regular (the strictly-monospaced single-cell-width
NF glyphs variant), fetched from the Nerd Fonts project release archive:

- Nerd Fonts project: <https://github.com/ryanoasis/nerd-fonts/>
- Source release: Nerd Fonts v3.4.0
- Upstream JetBrains Mono: <https://github.com/JetBrains/JetBrainsMono>
  (version 2.304)

Both the original JetBrains Mono typeface and the Nerd Fonts patched
derivative are distributed under the SIL Open Font License v1.1 (see
`OFL.txt`). The OFL allows free use, modification, embedding, and
redistribution; downstream packagers should treat the binary
distribution as a derived work under the OFL.

To regenerate the bundled asset against a newer Nerd Fonts release:

```bash
curl -fsSL -o /tmp/jbm.zip \
  https://github.com/ryanoasis/nerd-fonts/releases/latest/download/JetBrainsMono.zip
unzip -p /tmp/jbm.zip JetBrainsMonoNerdFontMono-Regular.ttf \
  > crates/shux-raster/assets/JetBrainsMonoNerdFontMono-Regular.ttf
unzip -p /tmp/jbm.zip OFL.txt > crates/shux-raster/assets/OFL.txt
# Re-run the rasterizer tests — the contract is asserted by
# bundled_font_covers_important_nf_and_unicode_glyphs + friends.
cargo nextest run -p shux-raster
```

## `NotoEmoji-Regular.ttf`

The monochrome outline emoji fallback used by the PNG rasterizer so PNG
snapshots resolve standalone emoji codepoints (🍺 🧩 🦀 🚀 ⚡ …) instead
of rendering tofu. Fetched from the Google Fonts CDN:

- Family: `Noto Emoji` (Version 3.005, ~860 KB)
- Source: <https://fonts.google.com/noto/specimen/Noto+Emoji>
- Upstream: <https://github.com/notofonts/emoji>

Released under the SIL Open Font License v1.1 (same as JetBrains Mono;
`OFL.txt` applies). The `name`-table License URL field points to the
SIL OFL site; no separate license text needs to ship alongside the .ttf
under that licence.

**Scope (v1):** monochrome rendering of standalone emoji codepoints.
Composed emoji (ZWJ sequences like `👨‍💻`, skin-tone modifiers, regional
indicator flag pairs) and colour rendering are *not* supported in v1 —
they require grapheme-cluster-aware storage in `shux-vt`, which today
keys cells on a single `char`. Tracked as future work.

To regenerate the bundled asset (the gstatic URL is content-hashed and
rolls when Google Fonts updates the version — re-resolve via the CSS):

```bash
# Google Fonts may emit one or many @font-face URLs depending on
# weight / subset slicing. We pin to the regular weight + no subset
# splitting so a single TTF is expected; assert exactly one match
# rather than blindly `head -n1`'ing.
urls=$(curl -fsSL -A "Mozilla/5.0" \
  "https://fonts.googleapis.com/css2?family=Noto+Emoji" \
  | grep -oE 'https://fonts.gstatic.com/[^)]+\.ttf')
n=$(echo "$urls" | wc -l)
if [ "$n" -ne 1 ]; then
  echo "expected exactly 1 NotoEmoji TTF URL, got $n — pin manually" >&2
  echo "$urls" >&2
  exit 1
fi
curl -fsSL -o crates/shux-raster/assets/NotoEmoji-Regular.ttf "$urls"
cargo nextest run -p shux-raster
```

Bypass route if the CSS path stops resolving cleanly: fetch a tagged
release TTF directly from <https://github.com/notofonts/emoji/releases>
(monochrome `.ttf`, not the COLR/CBDT colour variants) and drop it
into this directory under the same filename.
