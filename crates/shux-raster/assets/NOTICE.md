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
