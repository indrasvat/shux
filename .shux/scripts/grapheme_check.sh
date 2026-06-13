#!/usr/bin/env bash
# Verify grapheme-aware VT storage through real shux pane automation.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
task="069-shux-vt-grapheme-cell-storage"
qa_dir="${SHUX_GRAPHEME_QA:-${repo_root}/.shux/qa/${task}}"
golden_dir="${SHUX_GRAPHEME_GOLDENS:-${repo_root}/.shux/goldens/${task}}"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-grapheme.XXXXXX")"
session="grapheme-${RANDOM}-$$"
trigger="${runtime}/go"
promote="${SHUX_GRAPHEME_PROMOTE:-0}"

cleanup() {
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" session kill "${session}" >/dev/null 2>&1 || true
  rm -rf "${runtime}"
}
trap cleanup EXIT

mkdir -p "${qa_dir}" "${golden_dir}"

stress_py="${runtime}/grapheme_stress.py"
cat >"${stress_py}" <<'PY'
import sys
import time

out = sys.stdout
out.write("\x1b[2J\x1b[H")
out.write("SHUX_GRAPHEME_START\n")
out.write("combining: cafe\u0301 naive e\u0301\n")
out.write("variation: tool 🛠\ufe0f gear ⚙\ufe0f\n")
out.write("skin: thumbs 👍🏽 wave 👋🏿\n")
out.write("zwj: technologist 👨\u200d💻 scientist 👩\u200d🔬\n")
out.write("flags: us 🇺🇸 india 🇮🇳 japan 🇯🇵\n")
out.write("wide-adjacent: 界\u0301A 好B 語C\n")
out.write("\x1b[38;2;255;190;80mstyled-link: e\u0301 🛠\ufe0f 👨\u200d💻\x1b[0m\n")
out.write("SHUX_GRAPHEME_END\n")
out.flush()
time.sleep(600)
PY

create_json="$(
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" --format json \
    session create "${session}" -d --title "grapheme storage" -- \
    sh -lc "while [ ! -f '${trigger}' ]; do sleep 0.05; done; exec python3 -u '${stress_py}'"
)"
pane_id="$(jq -r '.pane_id' <<<"${create_json}")"

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
  -s "${session}" -p "${pane_id}" --cols 80 --rows 24 >/dev/null
touch "${trigger}"
env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
  -s "${session}" -p "${pane_id}" --text "SHUX_GRAPHEME_END" --timeout-ms 15000 >/dev/null

capture_one() {
  local label="$1"
  local cols="$2"
  local rows="$3"

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane set-size \
    -s "${session}" -p "${pane_id}" --cols "${cols}" --rows "${rows}" >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane wait-for \
    -s "${session}" -p "${pane_id}" --text "SHUX_GRAPHEME_END" --timeout-ms 15000 >/dev/null
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
    printf 'rerun with SHUX_GRAPHEME_PROMOTE=1 only after expected grapheme rendering is reviewed\n' >&2
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

capture_one "grapheme-80x24" 80 24
capture_one "grapheme-120x40" 120 40
capture_one "grapheme-200x60" 200 60

python3 - "${qa_dir}" >"${qa_dir}/grapheme-capture-report.json" <<'PY'
import json
import sys
from pathlib import Path

qa_dir = Path(sys.argv[1])
labels = ["grapheme-80x24", "grapheme-120x40", "grapheme-200x60"]
required = [
    "SHUX_GRAPHEME_START",
    "SHUX_GRAPHEME_END",
    "e\u0301",
    "🛠\ufe0f",
    "👍🏽",
    "👨\u200d💻",
    "🇺🇸",
    "界\u0301A",
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
  --arg capture "grapheme-capture-report.json" \
  '{
    task: $task,
    capture_report: $capture,
    screenshots: [
      "grapheme-80x24-actual.png",
      "grapheme-80x24-expected.png",
      "grapheme-80x24-diff.png",
      "grapheme-120x40-actual.png",
      "grapheme-120x40-expected.png",
      "grapheme-120x40-diff.png",
      "grapheme-200x60-actual.png",
      "grapheme-200x60-expected.png",
      "grapheme-200x60-diff.png"
    ],
    pixel_metrics: [
      "grapheme-80x24-pixel.json",
      "grapheme-120x40-pixel.json",
      "grapheme-200x60-pixel.json"
    ],
    captures: [
      "grapheme-80x24.txt",
      "grapheme-120x40.txt",
      "grapheme-200x60.txt"
    ]
  }' >"${qa_dir}/grapheme-automation-report.json"
