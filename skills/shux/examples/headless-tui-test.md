# Example: headless TUI regression test in CI

You're building a TUI in Bubbletea / ratatui / Charm. You want every PR
to fail if the rendered output drifts. Without shux you'd need iTerm2
running on a macOS CI runner with the Python SDK — expensive, flaky,
hard to debug. With shux it's a script that fits the `.shux/` layout.

## Layout

```
your-project/
└── .shux/
    ├── templates/visual-test.toml   (committed — spawn spec)
    ├── scripts/scenario.sh          (committed — driver)
    ├── goldens/01_loaded.png ...    (committed — reference frames)
    └── out/                         (gitignored — new snapshots & diffs)
```

`shux init` creates the directories. Add the template + script yourself.

## What CI runs

```bash
# Setup (one-off in the workflow image)
curl -sSfL https://shux.pages.dev/install.sh | sh

# The actual test
bash .shux/scripts/scenario.sh
```

## `.shux/scripts/scenario.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail
SHUX="${SHUX:-shux}"
SESSION="visual-test"
OUT=".shux/out"
GOLDENS=".shux/goldens"
mkdir -p "$OUT"

ENTER=$(printf '\r' | base64)
TAB=$(printf '\t' | base64)

cleanup () { "$SHUX" session kill "$SESSION" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Spawn. Deterministic clock so any TUI time-rendering stays stable.
SOURCE_DATE_EPOCH=1700000000 \
  "$SHUX" session create "$SESSION" -d -- mytui --no-network --fixtures-dir=tests/fixtures

# Fixed dims so snapshots are byte-comparable.
"$SHUX" pane set-size -s "$SESSION" --cols 160 --rows 48 >/dev/null

snap () {
  local label="$1"
  "$SHUX" pane snapshot -s "$SESSION" -o "$OUT/${label}.png" --cols 160 --rows 48 >/dev/null
}

# Drive — same sequence every run.
sleep 3                                 ; snap 01_loaded
"$SHUX" pane send-keys -s "$SESSION" --text 'j' >/dev/null
sleep 0.5                               ; snap 02_after_j
"$SHUX" pane send-keys -s "$SESSION" --data "$ENTER" >/dev/null
sleep 2                                 ; snap 03_detail_open
"$SHUX" pane send-keys -s "$SESSION" --data "$TAB" >/dev/null
sleep 0.5                               ; snap 04_next_tab

# Diff. The comparator below is from references/scenarios.md — feel free
# to swap in pixelmatch / SSIM with a higher tolerance for AA drift.
python3 - <<PY
import sys, numpy as np
from PIL import Image
fails = []
for label in ["01_loaded","02_after_j","03_detail_open","04_next_tab"]:
    a = np.asarray(Image.open(f".shux/out/{label}.png"))
    g = np.asarray(Image.open(f".shux/goldens/{label}.png"))
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
bash .shux/scripts/scenario.sh          # fails, saves new PNGs to .shux/out/
cp .shux/out/*.png .shux/goldens/       # accept the new look
git add .shux/goldens/
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
