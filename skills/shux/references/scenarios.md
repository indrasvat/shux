# Scenario-driver patterns

For repeatable scripted interactions with a TUI — drive a vivecaka /
lazygit / bubbletea app from outside, capture a labeled PNG at each step,
diff against a golden image in CI.

## The pattern (bash)

```bash
#!/usr/bin/env bash
set -euo pipefail
SHUX="${SHUX_BIN:-shux}"
SESSION="${SESSION:-demo}"
OUT_DIR="${OUT_DIR:-./snapshots}"
mkdir -p "$OUT_DIR"

# Pre-encoded control bytes.
ESC=$(printf '\033' | base64)
ENTER=$(printf '\r' | base64)
TAB=$(printf '\t' | base64)

# 1 — spawn
"$SHUX" kill -s "$SESSION" >/dev/null 2>&1 || true
RESP=$("$SHUX" api session.create \
  "{\"name\":\"$SESSION\",\"command\":[\"vivecaka\",\"--repo\",\"cli/cli\"]}")
PID=$(printf '%s' "$RESP" | jq -r .result.pane_id)

# 2 — resize (synchronous)
"$SHUX" api pane.set_size "{\"pane_id\":\"$PID\",\"cols\":200,\"rows\":60}" >/dev/null

# 3 — helpers
snap () {
  local label="$1"
  "$SHUX" api pane.snapshot "{\"pane_id\":\"$PID\"}" \
    | jq -r .result.png_base64 | base64 -d > "$OUT_DIR/${label}.png"
  echo "→ ${label}.png"
}
send_text () { "$SHUX" api pane.send_keys "{\"pane_id\":\"$PID\",\"text\":$1}" >/dev/null; }
send_b64  () { "$SHUX" api pane.send_keys "{\"pane_id\":\"$PID\",\"data\":\"$1\"}" >/dev/null; }
sleep_for () { sleep "$1"; }

# 4 — the scenario
sleep_for 6 ; snap "01_loaded"
send_text "\"j\"" ; sleep_for 1 ; snap "02_after_j"
send_text "\"/\"" ; sleep_for 1
send_text "\"actions\"" ; sleep_for 1 ; snap "03_searched"
send_b64 "$ESC" ; sleep_for 1
send_b64 "$ENTER" ; sleep_for 3 ; snap "04_pr_detail"
send_b64 "$TAB" ; sleep_for 1 ; snap "05_tab_checks"

# 5 — teardown
"$SHUX" kill -s "$SESSION" >/dev/null
```

## Declarative scenario array (the pattern that scales)

Drop the imperative `send/snap` calls and define the scenario as
data — easier to copy, easier to diff, easier to ship as a test fixture.

```bash
SCENARIO=(
  "wait :              : 6000 : -"
  "snap :              :    0 : 01_loaded"
  "text : \"j\"        :  800 : -"
  "snap :              :    0 : 02_after_j"
  "text : \"/\"        :  500 : -"
  "text : \"actions\"  : 1500 : -"
  "snap :              :    0 : 03_searched"
  "esc  :              :  500 : -"
  "enter :             : 3000 : -"
  "snap :              :    0 : 04_pr_detail"
  "tab  :              : 1500 : -"
  "snap :              :    0 : 05_tab_checks"
)

for row in "${SCENARIO[@]}"; do
  action="$(printf '%s' "$row" | awk -F: '{gsub(/^ +| +$/,"",$1); print $1}')"
  value="$(  printf '%s' "$row" | awk -F: '{for(i=2;i<NF-1;i++) printf "%s%s",$i,(i<NF-2?":":"")}' | sed 's/^ *//;s/ *$//')"
  sleepms="$(printf '%s' "$row" | awk -F: '{gsub(/^ +| +$/,"",$(NF-1)); print $(NF-1)}')"
  label="$(  printf '%s' "$row" | awk -F: '{gsub(/^ +| +$/,"",$NF);   print $NF}')"

  case "$action" in
    wait)  : ;;
    text)  send_text "$value" ;;
    esc)   send_b64 "$ESC" ;;
    enter) send_b64 "$ENTER" ;;
    tab)   send_b64 "$TAB" ;;
    snap)  snap "$label" ;;
    *)     echo "unknown action: $action"; exit 1 ;;
  esac
  python3 -c "import time;time.sleep($sleepms/1000)"
done
```

Editing the `SCENARIO=(...)` array is the entire authoring surface for a
new visual regression test. Copy the file, swap rows, get a new test.

## Golden-image comparison

In CI: render snapshots, diff against checked-in goldens, fail on
exceeding a perceptual threshold.

```python
# tests/raster_golden.py (sketch)
import base64, json, subprocess, numpy as np
from PIL import Image
from io import BytesIO

def snapshot(pane_id):
    out = subprocess.check_output(
        ["shux", "api", "pane.snapshot", json.dumps({"pane_id": pane_id})])
    return Image.open(BytesIO(base64.b64decode(json.loads(out)["result"]["png_base64"])))

def assert_matches_golden(img, golden_path, atol=2):
    g = Image.open(golden_path)
    if g.size != img.size:
        raise AssertionError(f"size: got {img.size} want {g.size}")
    diff = np.abs(np.asarray(img) - np.asarray(g)).max()
    if diff > atol:
        img.save(golden_path + ".actual.png")
        raise AssertionError(f"pixel diff {diff} > tolerance {atol}")
```

Determinism contract recommendation: compare **raw RGBA** (or
post-decode pixels), not PNG byte streams. PNG encoder internals can
change with crate versions; the pixel-level promise is the one shux
upholds.

## Watch a TUI process exit

```bash
# Poll the pane's PTY until process exits — checking pane.list every Ns.
while true; do
  state=$("$SHUX" api pane.list "{\"session_id\":\"$SID\"}" \
          | jq -r '.result[] | select(.id=="'"$PID"'") | .exit_code')
  if [[ "$state" != "null" && -n "$state" ]]; then
    echo "exited: $state"; break
  fi
  sleep 1
done
```

Or — pure event-driven — use `events.history` with `count` increasing
each poll, and watch for a `pane.exited` event matching your pane.

## Multi-pane fan-out

Send the same keystroke to every pane in a session at once
(iTerm2-broadcast-input equivalent):

```bash
for pid in $("$SHUX" api pane.list "{\"session_id\":\"$SID\"}" | jq -r '.result[].id'); do
  "$SHUX" api pane.send_keys "{\"pane_id\":\"$pid\",\"text\":\"ls\\n\"}" >/dev/null &
done
wait
```
