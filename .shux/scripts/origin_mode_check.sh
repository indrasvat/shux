#!/usr/bin/env bash
# Verify DECOM origin-mode and scroll-region rendering through real shux pane automation.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
task="072-shux-vt-origin-mode-scroll-region"
golden_task="072-shux-vt-origin-mode-scroll-region"
qa_dir="${SHUX_ORIGIN_MODE_QA:-${repo_root}/.shux/qa/${task}}"
golden_dir="${SHUX_ORIGIN_MODE_GOLDENS:-${repo_root}/.shux/goldens/${golden_task}}"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-origin-mode.XXXXXX")"
session="origin-mode-${RANDOM}-$$"
trigger="${runtime}/label"
promote="${SHUX_ORIGIN_MODE_PROMOTE:-0}"

cleanup() {
  shux_harness_cleanup_runtime "${runtime}" "${shux_bin}" "${session}"
}
trap cleanup EXIT

mkdir -p "${qa_dir}" "${golden_dir}"

fixture_py="${runtime}/origin_mode_fixture.py"
cat >"${fixture_py}" <<'PY'
import os
import sys
import time

trigger = sys.argv[1]
last = None


def terminal_size() -> tuple[int, int]:
    size = os.get_terminal_size(sys.stdout.fileno())
    return size.columns, size.lines


def clipped(text: str, cols: int, fill: str) -> str:
    return (text + (fill * cols))[:cols]


def draw(label: str) -> None:
    cols, rows = terminal_size()
    top = 3
    bottom = max(top + 2, rows - 2)
    body_height = bottom - top + 1
    mid_col = min(max(18, cols // 3), max(1, cols - 14))
    right_col = min(max(mid_col + 14, (cols * 2) // 3), max(1, cols - 10))

    out = sys.stdout
    out.write("\x1b[?25l\x1b[0m\x1b[2J\x1b[H")
    out.write(f"\x1b[1;1H{clipped(f'HEADER {label}', cols, '-')}")
    out.write(f"\x1b[{rows};1H{clipped(f'FOOTER {label}', cols, '=')}")

    out.write(f"\x1b[{top};{bottom}r\x1b[?6h")
    out.write("\x1b[1;1H\x1b[2KBODY-TOP")
    out.write(f"\x1b[{body_height};1H\x1b[2KBODY-BOTTOM")

    out.write(f"\x1b[{body_height};{mid_col}H")
    for idx in range(body_height + 2):
        out.write(f"SCROLL-{idx:02d}\n")

    out.write("\x1b[1;1H\x1b[2KBODY-TOP")
    out.write(f"\x1b[{body_height};1H\x1b[2KBODY-BOTTOM")
    out.write(f"\x1b[2;{mid_col}H\x1b[999BCLAMP-DOWN")
    out.write(f"\x1b[2;{right_col}H\x1b[999ACLAMP-UP")
    out.write(f"\x1b[?6l\x1b[r\x1b[{rows};1H{clipped(f'FOOTER {label}', cols, '=')}")
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
    session create "${session}" -d --title "origin mode" -- \
    python3 -u "${fixture_py}" "${trigger}"
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
    -s "${session}" -p "${pane_id}" --text "FOOTER ${label}" --timeout-ms 15000 >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane capture \
    -s "${session}" -p "${pane_id}" --lines "${rows}" >"${qa_dir}/${label}.txt"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" pane snapshot \
    -s "${session}" -p "${pane_id}" -o "${qa_dir}/${label}-actual.png" >/dev/null

  python3 - "${qa_dir}/${label}.txt" "${label}" "${rows}" >"${qa_dir}/${label}-text.json" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
label = sys.argv[2]
rows = int(sys.argv[3])
lines = path.read_text(encoding="utf-8").splitlines()
while len(lines) < rows:
    lines.append("")
top_idx = 2
bottom_idx = rows - 3
checks = {
    "header_fixed": lines[0].startswith(f"HEADER {label}"),
    "footer_fixed": lines[rows - 1].startswith(f"FOOTER {label}"),
    "body_top_in_margin": "BODY-TOP" in lines[top_idx],
    "body_bottom_in_margin": "BODY-BOTTOM" in lines[bottom_idx],
    "clamp_up_in_top_margin": "CLAMP-UP" in lines[top_idx],
    "clamp_down_in_bottom_margin": "CLAMP-DOWN" in lines[bottom_idx],
    "no_body_in_header": "BODY" not in lines[0],
    "no_body_in_footer": "BODY" not in lines[rows - 1],
    "no_footer_bleed_above_last_row": all(
        "FOOTER" not in line for line in lines[: rows - 1]
    ),
}
status = "pass" if all(checks.values()) else "fail"
print(json.dumps({"status": status, "label": label, "checks": checks}, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY

  if [ "${promote}" = "1" ]; then
    cp "${qa_dir}/${label}-actual.png" "${golden_dir}/${label}-expected.png"
    cp "${qa_dir}/${label}.txt" "${golden_dir}/${label}-expected.txt"
  fi

  if [ ! -f "${golden_dir}/${label}-expected.png" ]; then
    printf 'missing expected PNG: %s\n' "${golden_dir}/${label}-expected.png" >&2
    printf 'rerun with SHUX_ORIGIN_MODE_PROMOTE=1 only after DootSabha-approved origin-mode baselines are reviewed\n' >&2
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

capture_one "origin-80x24" 80 24
capture_one "origin-120x40" 120 40
capture_one "origin-200x60" 200 60

python3 - "${qa_dir}" >"${qa_dir}/origin-capture-report.json" <<'PY'
import json
import sys
from pathlib import Path

qa_dir = Path(sys.argv[1])
labels = ["origin-80x24", "origin-120x40", "origin-200x60"]
reports = [json.loads((qa_dir / f"{label}-text.json").read_text()) for label in labels]
status = "pass" if all(report["status"] == "pass" for report in reports) else "fail"
print(json.dumps({"status": status, "text_reports": reports}, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY

jq -n \
  --arg task "${task}" \
  --arg capture "origin-capture-report.json" \
  '{
    task: $task,
    capture_report: $capture,
    screenshots: [
      "origin-80x24-actual.png",
      "origin-80x24-expected.png",
      "origin-80x24-diff.png",
      "origin-120x40-actual.png",
      "origin-120x40-expected.png",
      "origin-120x40-diff.png",
      "origin-200x60-actual.png",
      "origin-200x60-expected.png",
      "origin-200x60-diff.png"
    ],
    pixel_metrics: [
      "origin-80x24-pixel.json",
      "origin-120x40-pixel.json",
      "origin-200x60-pixel.json"
    ],
    captures: [
      "origin-80x24.txt",
      "origin-120x40.txt",
      "origin-200x60.txt"
    ],
    text_metrics: [
      "origin-80x24-text.json",
      "origin-120x40-text.json",
      "origin-200x60-text.json"
    ]
  }' >"${qa_dir}/origin-automation-report.json"
