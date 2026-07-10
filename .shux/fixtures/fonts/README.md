# Lens fixture fonts (test-harness only — NOT the default raster chain)

These fonts are wired into the LENS test harness's isolated daemon config
(`appearance.font_fallbacks`, see `crates/shux/tests/lens_common/mod.rs`) so
the lens goldens render real Devanagari and the fixture CJK glyphs instead
of tofu (user adjudication, PRD §17 font-risk row). They do NOT change
shux's default bundled font chain — `make test-vt-corpus` and every
non-lens render path are unaffected.

| File | What | License | Provenance |
|---|---|---|---|
| `NotoSansDevanagari-Regular.ttf` | Full static hinted TTF (glyf) | OFL 1.1 (`OFL-NotoSansDevanagari.txt`) | https://raw.githubusercontent.com/notofonts/notofonts.github.io/main/fonts/NotoSansDevanagari/hinted/ttf/NotoSansDevanagari-Regular.ttf |
| `NotoSansJP-shuxlens-subset.ttf` | Tiny subset (~4 KB): EXACTLY the 9 CJK codepoints the lens fixtures use | OFL 1.1 (`OFL-NotoSansJP.txt`) | Subset of https://raw.githubusercontent.com/google/fonts/main/ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf |

sha256 of both TTFs is pinned in `.shux/goldens/lens/evidence-manifest.json`.

## CJK subset reproducibility

Codepoints (extracted from `.shux/fixtures/lens/*.sh` + `t/*.sh`):
`ステト実界真端終面` (U+30B9 U+30C6 U+30C8 U+5B9F U+754C U+771F U+7AEF
U+7D42 U+9762).

```sh
curl -fsSL -o NotoSansJP-var.ttf \
  "https://raw.githubusercontent.com/google/fonts/main/ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf"
uvx --from fonttools fonttools varLib.instancer \
  NotoSansJP-var.ttf wght=400 -o NotoSansJP-static400.ttf
printf 'ステト実界真端終面' > cjk_chars.txt
uvx --from fonttools pyftsubset NotoSansJP-static400.ttf \
  --text-file=cjk_chars.txt \
  --output-file=NotoSansJP-shuxlens-subset.ttf \
  --layout-features='' \
  --name-IDs='0,1,2,3,4,6,13,14' \
  --notdef-outline
```

The subset is committed (not the 9 MB source) per the golden re-mint
adjudication: repo pays ~4 KB for real CJK pixels in the lens goldens.
Adding new CJK text to a lens fixture requires re-running the subset with
the expanded character set (and a golden re-mint + re-approval).

## Rendering caveat (known + acceptable, recorded in BASELINE-APPROVAL.md)

The shux rasterizer (fontdue) does per-codepoint glyph lookup with NO
OpenType shaping: Devanagari conjuncts/matras render decomposed (each
codepoint's nominal glyph side by side), not as shaped ligatures. The
goldens capture that decomposed-but-real rendering — the point of these
fonts is "no tofu", not typographically-correct shaping.
