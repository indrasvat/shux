#!/usr/bin/env bash
# Verify mutable tab-stop rendering through real shux pane automation.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
task="071-shux-vt-tab-stops"
golden_task="071-tab-stops"
qa_dir="${SHUX_TAB_STOPS_QA:-${repo_root}/.shux/qa/${task}}"
golden_dir="${SHUX_TAB_STOPS_GOLDENS:-${repo_root}/.shux/goldens/${golden_task}}"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-tab-stops.XXXXXX")"
session="tab-stops-${RANDOM}-$$"
trigger="${runtime}/label"
promote="${SHUX_TAB_STOPS_PROMOTE:-0}"

cleanup() {
  shux_harness_cleanup_runtime "${runtime}" "${shux_bin}" "${session}"
}
trap cleanup EXIT

mkdir -p "${qa_dir}" "${golden_dir}"

stress_py="${runtime}/tab_stops_stress.py"
cat >"${stress_py}" <<'PY'
import sys
import time

trigger = sys.argv[1]
last = None
configured = False

def draw(label: str) -> None:
    global configured
    out = sys.stdout
    out.write("\x1b[2J\x1b[H\x1b[?25l")
    out.write(f"SHUX_TAB_START {label}\n")

    if label == "tabs-clear-all-80x24":
        out.write("\x1b[3g\rclear-all:\tZ\n")
    else:
        if not configured:
            out.write("default:\tA\tB\n")
            out.write("\x1b[13G\x1bH\rcustom:\tA\tB\tC\n")
            out.write("\x1b[9G\x1b[g")
            configured = True
        out.write("\rpreserved:\tA\tB\n")
        if label == "tabs-120x40":
            out.write("\x1b[88G\tW\n")

    out.write(f"SHUX_TAB_END {label}\n")
    out.flush()

while True:
    try:
        with open(trigger, "r", encoding="utf-8") as handle:
            label = handle.read().strip()
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
    session create "${session}" -d --title "tab stops" -- \
    python3 -u "${stress_py}" "${trigger}"
)"
pane_id="$(jq -r '.pane_id' <<<"${create_json}")"

capture_one() {
  local label="$1"
  local cols="$2"
  local rows="$3"

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
    -s "${session}" -p "${pane_id}" --cols "${cols}" --rows "${rows}" >/dev/null
  printf '%s\n' "${label}" >"${trigger}"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
    -s "${session}" -p "${pane_id}" --text "SHUX_TAB_END ${label}" --timeout-ms 15000 >/dev/null
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
    printf 'rerun with SHUX_TAB_STOPS_PROMOTE=1 only after DootSabha-approved tab-stop baselines are reviewed\n' >&2
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

capture_one "tabs-80x24" 80 24
capture_one "tabs-120x40" 120 40
capture_one "tabs-return-80x24" 80 24
capture_one "tabs-clear-all-80x24" 80 24

python3 - "${qa_dir}" >"${qa_dir}/tab-capture-report.json" <<'PY'
import json
import sys
from pathlib import Path

qa_dir = Path(sys.argv[1])
cases = [
    ("tabs-80x24", 80, "initial"),
    ("tabs-120x40", 120, "resize-wide"),
    ("tabs-return-80x24", 80, "resize-return"),
    ("tabs-clear-all-80x24", 80, "clear-all"),
]

def line_starting(text: str, prefix: str) -> str:
    for line in text.splitlines():
        if line.startswith(prefix):
            return line
    return ""

def char_at(line: str, col: int) -> str:
    chars = list(line)
    return chars[col] if col < len(chars) else ""

checks = []
for label, cols, kind in cases:
    text = (qa_dir / f"{label}.txt").read_text(encoding="utf-8")
    default = line_starting(text, "default:")
    custom = line_starting(text, "custom:")
    preserved = line_starting(text, "preserved:")
    clear_all = line_starting(text, "clear-all:")
    check = {
        "label": label,
        "cols": cols,
        "kind": kind,
    }
    if kind == "initial":
        check.update({
            "default_A_col_16": char_at(default, 16) == "A",
            "default_B_col_24": char_at(default, 24) == "B",
            "custom_A_col_8": char_at(custom, 8) == "A",
            "custom_B_col_12": char_at(custom, 12) == "B",
            "custom_C_col_16": char_at(custom, 16) == "C",
        })
    if kind != "clear-all":
        check.update({
            "preserved_A_col_12": char_at(preserved, 12) == "A",
            "preserved_B_col_16": char_at(preserved, 16) == "B",
        })
    if kind == "resize-wide":
        wide = line_starting(text, " " * 88 + "W")
        check["resize_wide_default_col_88"] = char_at(wide, 88) == "W"
    if kind == "clear-all":
        check["clear_all_Z_last_col"] = char_at(clear_all, cols - 1) == "Z"
    checks.append(check)

status = "pass" if all(
    all(value for key, value in check.items() if key not in {"label", "cols"})
    for check in checks
) else "fail"
print(json.dumps({"status": status, "checks": checks}, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY

jq -n \
  --arg task "${task}" \
  --arg capture "tab-capture-report.json" \
  '{
    task: $task,
    capture_report: $capture,
    screenshots: [
      "tabs-80x24-actual.png",
      "tabs-80x24-expected.png",
      "tabs-80x24-diff.png",
      "tabs-120x40-actual.png",
      "tabs-120x40-expected.png",
      "tabs-120x40-diff.png",
      "tabs-return-80x24-actual.png",
      "tabs-return-80x24-expected.png",
      "tabs-return-80x24-diff.png",
      "tabs-clear-all-80x24-actual.png",
      "tabs-clear-all-80x24-expected.png",
      "tabs-clear-all-80x24-diff.png"
    ],
    pixel_metrics: [
      "tabs-80x24-pixel.json",
      "tabs-120x40-pixel.json",
      "tabs-return-80x24-pixel.json",
      "tabs-clear-all-80x24-pixel.json"
    ],
    captures: [
      "tabs-80x24.txt",
      "tabs-120x40.txt",
      "tabs-return-80x24.txt",
      "tabs-clear-all-80x24.txt"
    ]
  }' >"${qa_dir}/tab-automation-report.json"
