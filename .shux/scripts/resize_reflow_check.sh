#!/usr/bin/env bash
# Verify soft-wrap resize reflow through real shux pane automation.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
task="067-shux-vt-resize-reflow"
qa_dir="${SHUX_RESIZE_REFLOW_QA:-${repo_root}/.shux/qa/${task}}"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-resize-reflow.XXXXXX")"
session="resize-reflow-${RANDOM}-$$"
trigger="${runtime}/go"
expected="SHUX_RESIZE_REFLOW_abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789_END"

cleanup() {
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" session kill "${session}" >/dev/null 2>&1 || true
  rm -rf "${runtime}"
}
trap cleanup EXIT

mkdir -p "${qa_dir}"

create_json="$(
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" --format json \
    session create "${session}" -d --title "resize reflow" -- \
    sh -lc "while [ ! -f '${trigger}' ]; do sleep 0.05; done; exec python3 -u -c 'import time; print(\"${expected}\", flush=True); time.sleep(600)'"
)"
pane_id="$(jq -r '.pane_id' <<<"${create_json}")"

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
  -s "${session}" -p "${pane_id}" --cols 80 --rows 24 >/dev/null
touch "${trigger}"
env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
  -s "${session}" -p "${pane_id}" --text "_END" --timeout-ms 15000 >/dev/null

capture_one() {
  local label="$1"
  local cols="$2"
  local rows="$3"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
    -s "${session}" -p "${pane_id}" --cols "${cols}" --rows "${rows}" >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
    -s "${session}" -p "${pane_id}" --text "_END" --timeout-ms 15000 >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane capture \
    -s "${session}" -p "${pane_id}" --lines "${rows}" >"${qa_dir}/${label}.txt"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane snapshot \
    -s "${session}" -p "${pane_id}" -o "${qa_dir}/${label}-actual.png" >/dev/null
}

capture_one "resize-80x24-before" 80 24
capture_one "resize-120x40" 120 40
capture_one "resize-40x12" 40 12
capture_one "resize-80x24-after" 80 24

python3 - "${qa_dir}" "${expected}" >"${qa_dir}/resize-capture-report.json" <<'PY'
import json
import sys
from pathlib import Path

qa_dir = Path(sys.argv[1])
expected = sys.argv[2]
labels = ["resize-80x24-before", "resize-120x40", "resize-40x12", "resize-80x24-after"]
checks = []
for label in labels:
    text = (qa_dir / f"{label}.txt").read_text(encoding="utf-8")
    compact = text.replace("\n", "")
    checks.append({
        "label": label,
        "contains_expected": expected in compact,
        "capture_bytes": len(text.encode()),
    })

status = "pass" if all(check["contains_expected"] for check in checks) else "fail"
print(json.dumps({"status": status, "expected": expected, "checks": checks}, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY

cd "${repo_root}"
.claude/automations/pixel_verify.py \
  ".shux/qa/${task}/resize-80x24-after-actual.png" \
  ".shux/qa/${task}/resize-80x24-before-actual.png" \
  --diff ".shux/qa/${task}/resize-80x24-return-diff.png" \
  --max-pixel-diff-ratio 0.0 \
  --max-mean-channel-delta 0.0 \
  >".shux/qa/${task}/resize-80x24-return-pixel.json"

jq -n \
  --arg task "${task}" \
  --arg capture "resize-capture-report.json" \
  --arg pixel "resize-80x24-return-pixel.json" \
  '{
    task: $task,
    capture_report: $capture,
    pixel_metric: $pixel,
    screenshots: [
      "resize-80x24-before-actual.png",
      "resize-120x40-actual.png",
      "resize-40x12-actual.png",
      "resize-80x24-after-actual.png",
      "resize-80x24-return-diff.png"
    ],
    captures: [
      "resize-80x24-before.txt",
      "resize-120x40.txt",
      "resize-40x12.txt",
      "resize-80x24-after.txt"
    ]
  }' >".shux/qa/${task}/resize-automation-report.json"
