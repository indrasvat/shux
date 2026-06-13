#!/usr/bin/env bash
# Verify DEC Special Graphics rendering through real shux pane automation.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
task="070-shux-vt-dec-special-graphics"
golden_task="070-dec-special-graphics"
qa_dir="${SHUX_DEC_GRAPHICS_QA:-${repo_root}/.shux/qa/${task}}"
golden_dir="${SHUX_DEC_GRAPHICS_GOLDENS:-${repo_root}/.shux/goldens/${golden_task}}"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-dec-graphics.XXXXXX")"
session="dec-graphics-${RANDOM}-$$"
trigger="${runtime}/go"
promote="${SHUX_DEC_GRAPHICS_PROMOTE:-0}"

cleanup() {
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" session kill "${session}" >/dev/null 2>&1 || true
  rm -rf "${runtime}"
}
trap cleanup EXIT

mkdir -p "${qa_dir}" "${golden_dir}"

stress_py="${runtime}/dec_graphics_stress.py"
cat >"${stress_py}" <<'PY'
import sys
import time

out = sys.stdout
out.write("\x1b[2J\x1b[H")
out.write("SHUX_DEC_START\n")
out.write("g0-box: \x1b(0lqqqqk\x1b(B\n")
out.write("        \x1b(0x    x\x1b(B  ascii-safe\n")
out.write("        \x1b(0mqqqqj\x1b(B\n")
out.write("g1-shift: A\x1b)0\x0elqkxmj\x0fZ\n")
out.write("redesignate: \x1b)0\x0eq\x1b)Bq\n")
out.write("full-map: \x0f\x1b(0_`abcdefghijklmnopqrstuvwxyz{|}~\x1b(B\n")
out.write("\x1b[38;2;120;220;180mcolor-boundary: \x0f\x1b(0tqqqu\x1b(B text\x1b[0m\n")
out.write("unicode-direct: ┌─┐│└┘ stays\n")
out.write("wide-safe: \x0f\x1b(0你q\x1b(B\n")
out.write("rep: \x0f\x1b(0q\x1b[3b\x1b(B\n")
out.write("SHUX_DEC_END\n")
out.flush()
time.sleep(600)
PY

create_json="$(
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" --format json \
    session create "${session}" -d --title "dec graphics" -- \
    sh -lc "while [ ! -f '${trigger}' ]; do sleep 0.05; done; exec python3 -u '${stress_py}'"
)"
pane_id="$(jq -r '.pane_id' <<<"${create_json}")"

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
  -s "${session}" -p "${pane_id}" --cols 80 --rows 24 >/dev/null
touch "${trigger}"
env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
  -s "${session}" -p "${pane_id}" --text "SHUX_DEC_END" --timeout-ms 15000 >/dev/null

capture_one() {
  local label="$1"
  local cols="$2"
  local rows="$3"

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
    -s "${session}" -p "${pane_id}" --cols "${cols}" --rows "${rows}" >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
    -s "${session}" -p "${pane_id}" --text "SHUX_DEC_END" --timeout-ms 15000 >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane capture \
    -s "${session}" -p "${pane_id}" --lines "${rows}" >"${qa_dir}/${label}.txt"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane snapshot \
    -s "${session}" -p "${pane_id}" -o "${qa_dir}/${label}-actual.png" >/dev/null

  if [ "${promote}" = "1" ]; then
    cp "${qa_dir}/${label}-actual.png" "${golden_dir}/${label}-expected.png"
    cp "${qa_dir}/${label}.txt" "${golden_dir}/${label}-expected.txt"
  fi

  if [ ! -f "${golden_dir}/${label}-expected.png" ]; then
    printf 'missing expected PNG: %s\n' "${golden_dir}/${label}-expected.png" >&2
    printf 'rerun with SHUX_DEC_GRAPHICS_PROMOTE=1 only after DootSabha-approved glyph baselines are reviewed\n' >&2
    exit 1
  fi
  cp "${golden_dir}/${label}-expected.png" "${qa_dir}/${label}-expected.png"

  cd "${repo_root}"
  .claude/automations/pixel_verify.py \
    ".shux/qa/${task}/${label}-actual.png" \
    ".shux/qa/${task}/${label}-expected.png" \
    --diff ".shux/qa/${task}/${label}-diff.png" \
    --max-pixel-diff-ratio 0.0 \
    --max-mean-channel-delta 0.0 \
    >".shux/qa/${task}/${label}-pixel.json"
}

capture_one "dec-80x24" 80 24
capture_one "dec-120x40" 120 40
capture_one "dec-200x60" 200 60

python3 - "${qa_dir}" >"${qa_dir}/dec-capture-report.json" <<'PY'
import json
import sys
from pathlib import Path

qa_dir = Path(sys.argv[1])
labels = ["dec-80x24", "dec-120x40", "dec-200x60"]
required = [
    "SHUX_DEC_START",
    "SHUX_DEC_END",
    "┌────┐",
    "│    │  ascii-safe",
    "└────┘",
    "A┌─┐│└┘Z",
    "redesignate: ─q",
    "full-map:  ◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·",
    "color-boundary: ├───┤ text",
    "unicode-direct: ┌─┐│└┘ stays",
    "wide-safe: 你─",
    "rep: ────",
]
checks = []
for label in labels:
    text = (qa_dir / f"{label}.txt").read_text(encoding="utf-8")
    checks.append({
        "label": label,
        "capture_bytes": len(text.encode()),
        "required_present": {needle: needle in text for needle in required},
    })
status = "pass" if all(all(c["required_present"].values()) for c in checks) else "fail"
print(json.dumps({"status": status, "checks": checks}, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY

jq -n \
  --arg task "${task}" \
  --arg capture "dec-capture-report.json" \
  '{
    task: $task,
    capture_report: $capture,
    screenshots: [
      "dec-80x24-actual.png",
      "dec-80x24-expected.png",
      "dec-80x24-diff.png",
      "dec-120x40-actual.png",
      "dec-120x40-expected.png",
      "dec-120x40-diff.png",
      "dec-200x60-actual.png",
      "dec-200x60-expected.png",
      "dec-200x60-diff.png"
    ],
    pixel_metrics: [
      "dec-80x24-pixel.json",
      "dec-120x40-pixel.json",
      "dec-200x60-pixel.json"
    ],
    captures: [
      "dec-80x24.txt",
      "dec-120x40.txt",
      "dec-200x60.txt"
    ]
  }' >"${qa_dir}/dec-automation-report.json"
