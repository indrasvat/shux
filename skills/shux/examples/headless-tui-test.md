# Example: headless TUI regression test in CI

You're building a TUI in Bubbletea / ratatui / Charm. You want every PR
to fail if the rendered output drifts. Without shux you'd need iTerm2
running on a macOS CI runner with the Python SDK — expensive, flaky,
hard to debug. With shux it's a script.

## What CI runs

```bash
# Setup (one-off in the workflow image)
curl -sSf https://shux.pages.dev/install | sh

# The actual test
bash tests/visual/scenario.sh
```

`tests/visual/scenario.sh` spawns your TUI under shux, drives it through
the same key sequence on every CI run, snapshots at known points, and
diffs against checked-in goldens.

## Full example: testing a hypothetical `mytui` PR-review app

```bash
#!/usr/bin/env bash
set -euo pipefail
SHUX="${SHUX:-shux}"
SESSION="visual-test"
OUT="$(mktemp -d)"
GOLDENS="tests/visual/goldens"

# Pre-encoded keys we'll send below.
ENTER=$(printf '\r' | base64)
TAB=$(printf '\t' | base64)
ESC=$(printf '\033' | base64)

cleanup () { "$SHUX" kill -s "$SESSION" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Spawn under shux. Use a deterministic SOURCE_DATE_EPOCH so any time
# rendering in the TUI is stable across runs.
RESP=$(SOURCE_DATE_EPOCH=1700000000 "$SHUX" api session.create '{
  "name": "visual-test",
  "command": ["mytui", "--no-network", "--fixtures-dir=tests/fixtures"]
}')
PID=$(printf '%s' "$RESP" | jq -r .result.pane_id)

# Fixed dims so snapshots are byte-comparable.
"$SHUX" api pane.set_size "{\"pane_id\":\"$PID\",\"cols\":160,\"rows\":48}" >/dev/null

snap () {
  local label="$1"
  "$SHUX" api pane.snapshot "{\"pane_id\":\"$PID\"}" \
    | jq -r .result.png_base64 | base64 -d > "$OUT/${label}.png"
}

# Drive — same sequence every run.
sleep 3                                 ; snap 01_loaded
"$SHUX" api pane.send_keys "{\"pane_id\":\"$PID\",\"text\":\"j\"}" >/dev/null
sleep 0.5                               ; snap 02_after_j
"$SHUX" api pane.send_keys "{\"pane_id\":\"$PID\",\"data\":\"$ENTER\"}" >/dev/null
sleep 2                                 ; snap 03_detail_open
"$SHUX" api pane.send_keys "{\"pane_id\":\"$PID\",\"data\":\"$TAB\"}" >/dev/null
sleep 0.5                               ; snap 04_next_tab

# Diff. The comparator below is from references/scenarios.md — feel free
# to swap in pixelmatch / SSIM with a higher tolerance for AA drift.
python3 - <<PY
import sys, numpy as np
from PIL import Image
fails = []
for label in ["01_loaded","02_after_j","03_detail_open","04_next_tab"]:
    a = np.asarray(Image.open(f"$OUT/{label}.png"))
    g = np.asarray(Image.open(f"$GOLDENS/{label}.png"))
    if a.shape != g.shape:
        fails.append(f"{label}: shape mismatch {a.shape} vs {g.shape}")
        continue
    d = np.abs(a.astype(int) - g.astype(int)).max()
    if d > 2:
        fails.append(f"{label}: max pixel diff {d}")
if fails:
    print("VISUAL REGRESSION:", *fails, sep="\n - ")
    sys.exit(1)
print("✓ all snapshots match goldens.")
PY
```

## Updating the goldens

When you intend a visual change:

```bash
bash tests/visual/scenario.sh          # fails, saves new PNGs to $OUT
cp $OUT/*.png tests/visual/goldens/    # accept the new look
git add tests/visual/goldens/
```

Commit the new goldens alongside the change. PR review now includes
"does this PNG look right?".

## Why this works on Linux CI

- No display server. shux's daemon owns the rasterizer; nothing about it
  needs an X server or Wayland compositor.
- No macOS. iTerm2 + Python SDK was the macOS-only piece in the older
  pattern; shux replaces it with pure-Rust glyph rendering.
- Deterministic dimensions. `pane.set_size` is synchronous, so the
  follow-up `pane.snapshot` is guaranteed to see the new size.

## Tolerances

`fontdue` uses f32 math for glyph rasterization. Across Linux x86 + macOS
arm64 you may see 1–2 LSB of alpha drift on anti-aliased pixels. Two
options:

- **Tight match** (recommended): `max_pixel_diff <= 2` on raw RGBA.
- **Perceptual match**: SSIM >= 0.995 on the decoded pixel arrays.

Don't compare PNG byte streams — the `image` crate's encoder internals
can change across versions and you'll flake.
