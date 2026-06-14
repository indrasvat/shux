#!/usr/bin/env bash
# Verify shux-vt dirty-region tracking through VT replay, raster parity, and live shux PNG automation.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
task="074-shux-vt-dirty-region-tracking"
qa_dir="${SHUX_DIRTY_QA:-${repo_root}/.shux/qa/${task}}"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-dirty-region.XXXXXX")"
session="solid-vt-${task}-${RANDOM}-$$"
trigger="${runtime}/tick"
session_created=0

cleanup() {
  if [ "${session_created}" = "1" ]; then
    shux_harness_cleanup_runtime "${runtime}" "${shux_bin}" "${session}"
  else
    shux_harness_stop_daemon "${runtime}"
    rm -rf "${runtime}"
  fi
}
trap cleanup EXIT

mkdir -p "${qa_dir}"

cargo run --release -p shux-raster --example dirty_region_harness >/dev/null

uv run --script "${repo_root}/.claude/automations/pixel_verify.py" \
  "${qa_dir}/dirty-120x30-actual.png" \
  "${qa_dir}/dirty-120x30-expected.png" \
  --diff "${qa_dir}/dirty-120x30-diff.png" \
  --max-pixel-diff-ratio 0.0 \
  --max-mean-channel-delta 0.0 \
  >"${qa_dir}/dirty-120x30-pixel.json"

fixture_py="${runtime}/dirty_live_fixture.py"
cat >"${fixture_py}" <<'PY'
import os
import sys
import time

trigger = sys.argv[1]
last = None

def draw(label: str) -> None:
    cols = os.get_terminal_size(sys.stdout.fileno()).columns
    out = sys.stdout
    out.write("\x1b[?25l\x1b[2J\x1b[H")
    out.write("DIRTY REGION LIVE CHECK".ljust(cols, "-"))
    out.write("\x1b[5;10H" + f"tick={label}")
    out.write("\x1b[8;1H" + ("=" * min(cols, 80)))
    out.write("\x1b[10;1HOnly a small area changes between ticks; full PNG capture remains stable.")
    out.write("\x1b[12;1Hcolor-probe: ")
    out.write("\x1b[38;2;120;220;180mTRUECOLOR\x1b[0m ")
    out.write("\x1b[38;5;196mINDEXED\x1b[0m ")
    out.write("\x1b[34mBASIC\x1b[0m")
    out.flush()

while True:
    try:
        label = open(trigger, "r", encoding="utf-8").read().strip()
    except FileNotFoundError:
        time.sleep(0.05)
        continue
    if label and label != last:
        draw(label)
        last = label
    time.sleep(0.05)
PY

create_json="$(
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" --format json \
    session create "${session}" -d --title "dirty regions" -- \
    env TERM=xterm-256color COLORTERM=truecolor python3 -u "${fixture_py}" "${trigger}"
)"
session_created=1
pane_id="$(jq -r '.pane_id' <<<"${create_json}")"

captures=()
screenshots=()
for viewport in 80x24 120x40 200x60; do
  cols="${viewport%x*}"
  rows="${viewport#*x}"
  label="dirty-live-${viewport}"
  tick="${viewport}"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
    -s "${session}" -p "${pane_id}" --cols "${cols}" --rows "${rows}" >/dev/null
  printf '%s\n' "${tick}" >"${trigger}"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
    -s "${session}" -p "${pane_id}" --text "tick=${tick}" --timeout-ms 15000 >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane snapshot \
    -s "${session}" -p "${pane_id}" -o "${qa_dir}/${label}-actual.png" >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane capture \
    -s "${session}" -p "${pane_id}" --lines "${rows}" >"${qa_dir}/${label}.txt"
  captures+=("${label}.txt")
  screenshots+=("${label}-actual.png")
done

python3 - "${qa_dir}" >"${qa_dir}/dirty-live-report.json" <<'PY'
import json
import sys
from pathlib import Path

qa_dir = Path(sys.argv[1])
viewports = ["80x24", "120x40", "200x60"]
checks = []
for viewport in viewports:
    text = (qa_dir / f"dirty-live-{viewport}.txt").read_text(encoding="utf-8")
    viewport_checks = {
        "header_present": "DIRTY REGION LIVE CHECK" in text,
        "tick_present": f"tick={viewport}" in text,
        "body_present": "Only a small area changes" in text,
        "color_probe_text_present": "TRUECOLOR" in text and "INDEXED" in text and "BASIC" in text,
    }
    checks.append({
        "viewport": viewport,
        "capture": f"dirty-live-{viewport}.txt",
        "screenshot": f"dirty-live-{viewport}-actual.png",
        "checks": viewport_checks,
        "status": "pass" if all(viewport_checks.values()) else "fail",
    })
status = "pass" if all(item["status"] == "pass" for item in checks) else "fail"
print(json.dumps({"status": status, "checks": checks}, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY

uv run --with pillow python - "${qa_dir}" >"${qa_dir}/dirty-live-color-report.json" <<'PY'
import json
import sys
from pathlib import Path

from PIL import Image

qa_dir = Path(sys.argv[1])
viewports = ["80x24", "120x40", "200x60"]
checks = []
for viewport in viewports:
    path = qa_dir / f"dirty-live-{viewport}-actual.png"
    image = Image.open(path).convert("RGBA")
    colored = 0
    data = image.tobytes()
    for idx in range(0, len(data), 4):
        r, g, b, a = data[idx : idx + 4]
        if a and max(r, g, b) - min(r, g, b) > 40 and max(r, g, b) > 80:
            colored += 1
    checks.append({
        "viewport": viewport,
        "screenshot": path.name,
        "colored_pixels": colored,
        "status": "pass" if colored > 100 else "fail",
    })
status = "pass" if all(item["status"] == "pass" for item in checks) else "fail"
print(json.dumps({"status": status, "checks": checks}, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY

screenshots_json="$(
  {
    printf '%s\n' "dirty-120x30-actual.png" "dirty-120x30-expected.png" "dirty-120x30-diff.png"
    printf '%s\n' "${screenshots[@]}"
  } | jq -R . | jq -s .
)"
captures_json="$(printf '%s\n' "${captures[@]}" | jq -R . | jq -s .)"

jq -n \
  --arg task "${task}" \
  --arg solid "SOLID-QA.md" \
  --arg design "dootsabha-design.json" \
  --arg implementation "dootsabha-implementation.json" \
  --argjson screenshots "${screenshots_json}" \
  --argjson captures "${captures_json}" \
  '{
    task: $task,
    solid_qa_report: $solid,
    dootsabha_design: $design,
    dootsabha_implementation: $implementation,
    screenshots: $screenshots,
    captures: $captures,
    pixel_metrics: ["dirty-120x30-pixel.json"],
    reports: [
      "dirty-region-report.json",
      "performance.json",
      "dirty-live-report.json",
      "dirty-live-color-report.json"
    ],
    notes: [
      "Dirty-region API is observation-only in task 074; raster parity compares a dirty-tracking-disabled VT replay against a dirty-tracking-enabled VT replay of the same fixture.",
      "No renderer consumes dirty regions in task 074, so dirty/incremental-render pixel parity is intentionally out of scope until a renderer consumer lands.",
      "Cursor-only movement is documented as outside grid dirty regions because cursor presentation is rendered as an overlay.",
      "Live shux automation includes explicit truecolor, indexed-color, and basic-color probes; screenshots must contain saturated color pixels."
    ]
  }' >"${qa_dir}/evidence-manifest.json"
