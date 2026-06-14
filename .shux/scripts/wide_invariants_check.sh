#!/usr/bin/env bash
# Verify wide-cell invariant rendering through real shux pane automation.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
task="068-shux-vt-wide-cell-invariants"
qa_dir="${SHUX_WIDE_INVARIANTS_QA:-${repo_root}/.shux/qa/${task}}"
golden_dir="${SHUX_WIDE_INVARIANTS_GOLDENS:-${repo_root}/.shux/goldens/${task}}"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-wide-invariants.XXXXXX")"
session="wide-invariants-${RANDOM}-$$"
trigger="${runtime}/go"
promote="${SHUX_WIDE_INVARIANTS_PROMOTE:-0}"

cleanup() {
  shux_harness_cleanup_runtime "${runtime}" "${shux_bin}" "${session}"
}
trap cleanup EXIT

mkdir -p "${qa_dir}" "${golden_dir}"

stress_py="${runtime}/wide_stress.py"
cat >"${stress_py}" <<'PY'
import sys
import time

out = sys.stdout
out.write("\x1b[2J\x1b[H")
out.write("SHUX_WIDE_START\n")
out.write("plain: ASCII | cjk: 界好語文 | mixed: A界B好C\n")
out.write("\x1b[48;2;24;80;120mcolored-cjk: 彩界彩好彩\x1b[0m\n")
out.write("emoji-wide: 🍺 🧩 🦀 🚀\n")
out.write("\x1b[6;1Hwide-overwrite: 你你ABCD")
out.write("\x1b[6;17H好")
out.write("\x1b[8;1Hdch-tail: 界ABCD")
out.write("\x1b[8;12H\x1b[1P")
out.write("\x1b[10;1Hech-tail: 界ABCD")
out.write("\x1b[10;12H\x1b[1X")
out.write("\x1b[12;1Hich-edge: AB界")
out.write("\x1b[12;11H\x1b[1@")
out.write("\x1b[14;1Hrep-wide: 界\x1b[2b")
out.write("\x1b[16;80H界")
out.write("\x1b[18;1HSHUX_WIDE_END")
out.flush()
time.sleep(600)
PY

create_json="$(
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" --format json \
    session create "${session}" -d --title "wide invariants" -- \
    sh -lc "while [ ! -f '${trigger}' ]; do sleep 0.05; done; exec python3 -u '${stress_py}'"
)"
pane_id="$(jq -r '.pane_id' <<<"${create_json}")"

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
  -s "${session}" -p "${pane_id}" --cols 80 --rows 24 >/dev/null
touch "${trigger}"
env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
  -s "${session}" -p "${pane_id}" --text "SHUX_WIDE_END" --timeout-ms 15000 >/dev/null

capture_one() {
  local label="$1"
  local cols="$2"
  local rows="$3"

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
    -s "${session}" -p "${pane_id}" --cols "${cols}" --rows "${rows}" >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
    -s "${session}" -p "${pane_id}" --text "SHUX_WIDE_END" --timeout-ms 15000 >/dev/null
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
    printf 'rerun with SHUX_WIDE_INVARIANTS_PROMOTE=1 only when updating approved .shux/goldens baselines\n' >&2
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

capture_one "wide-80x24" 80 24
capture_one "wide-120x40" 120 40
capture_one "wide-200x60" 200 60

python3 - "${qa_dir}" >"${qa_dir}/wide-capture-report.json" <<'PY'
import json
import sys
from pathlib import Path

qa_dir = Path(sys.argv[1])
labels = ["wide-80x24", "wide-120x40", "wide-200x60"]
required = ["SHUX_WIDE_START", "SHUX_WIDE_END", "界", "好", "colored-cjk", "🍺", "🚀"]
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
  --arg capture "wide-capture-report.json" \
  '{
    task: $task,
    capture_report: $capture,
    screenshots: [
      "wide-80x24-actual.png",
      "wide-80x24-expected.png",
      "wide-80x24-diff.png",
      "wide-120x40-actual.png",
      "wide-120x40-expected.png",
      "wide-120x40-diff.png",
      "wide-200x60-actual.png",
      "wide-200x60-expected.png",
      "wide-200x60-diff.png"
    ],
    pixel_metrics: [
      "wide-80x24-pixel.json",
      "wide-120x40-pixel.json",
      "wide-200x60-pixel.json"
    ],
    captures: [
      "wide-80x24.txt",
      "wide-120x40.txt",
      "wide-200x60.txt"
    ]
  }' >".shux/qa/${task}/wide-automation-report.json"
