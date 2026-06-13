# Visual Inspection

Inspected full-resolution PNGs:

- `grapheme-80x24-actual.png`
- `grapheme-120x40-actual.png`
- `grapheme-200x60-actual.png`

Result: PASS for the task scope.

Observed behavior:

- Combining marks are preserved in text capture and visible as accent placement
  in the raster output.
- Styled grapheme text keeps foreground styling.
- Adjacent CJK/ASCII content remains aligned; no payload visibly spills into
  following cells.
- ZWJ emoji, skin-tone modifiers, VS16, and regional-indicator flags are
  preserved in capture text.
- PNG raster output still degrades some composed emoji and flags to monochrome
  fallback glyphs or boxes because `fontdue` has no shaping/color-emoji engine.
  This is expected and documented as out of scope for task 069.

Pixel verification:

- `grapheme-80x24-pixel.json`: exact match, `pixel_diff_ratio = 0.0`.
- `grapheme-120x40-pixel.json`: exact match, `pixel_diff_ratio = 0.0`.
- `grapheme-200x60-pixel.json`: exact match, `pixel_diff_ratio = 0.0`.
