#!/usr/bin/env bash
# Measure grapheme-storage impact on the ASCII common path.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
task="069-shux-vt-grapheme-cell-storage"
qa_dir="${SHUX_GRAPHEME_QA:-${repo_root}/.shux/qa/${task}}"
baseline="${SHUX_GRAPHEME_PERF_BASELINE:-${qa_dir}/performance-baseline.json}"
current="${qa_dir}/performance-current.json"
report="${qa_dir}/performance-report.json"

mkdir -p "${qa_dir}"
cd "${repo_root}"

cargo build --release -p shux-vt --example grapheme_perf >/dev/null

tmp_out="$(mktemp "${TMPDIR:-/tmp}/shux-grapheme-perf.XXXXXX.json")"
tmp_time="$(mktemp "${TMPDIR:-/tmp}/shux-grapheme-perf-time.XXXXXX.txt")"
trap 'rm -f "${tmp_out}" "${tmp_time}"' EXIT

if /usr/bin/time -l target/release/examples/grapheme_perf >"${tmp_out}" 2>"${tmp_time}"; then
  max_rss_kb="$(awk '/maximum resident set size/ {print int($1 / 1024)}' "${tmp_time}")"
elif /usr/bin/time -v target/release/examples/grapheme_perf >"${tmp_out}" 2>"${tmp_time}"; then
  max_rss_kb="$(awk -F: '/Maximum resident set size/ {gsub(/ /, "", $2); print $2}' "${tmp_time}")"
else
  cat "${tmp_time}" >&2
  exit 1
fi

python3 - "${tmp_out}" "${max_rss_kb:-0}" >"${current}" <<'PY'
import json
import sys

data = json.loads(open(sys.argv[1], encoding="utf-8").read())
data["max_rss_kb"] = int(sys.argv[2] or 0)
print(json.dumps(data, indent=2, sort_keys=True))
PY

if [ ! -f "${baseline}" ]; then
  cp "${current}" "${baseline}"
fi

python3 - "${baseline}" "${current}" >"${report}" <<'PY'
import json
import sys

baseline = json.load(open(sys.argv[1], encoding="utf-8"))
current = json.load(open(sys.argv[2], encoding="utf-8"))

def pct_delta(cur, base):
    if base == 0:
        return 0.0 if cur == 0 else float("inf")
    return ((cur - base) / base) * 100.0

rss_delta = pct_delta(current["max_rss_kb"], baseline["max_rss_kb"])
capture_slowdown = pct_delta(
    baseline["captures_per_second"],
    current["captures_per_second"],
)
cell_size_changed = current["cell_size_bytes"] != baseline["cell_size_bytes"]
status = (
    "pass"
    if not cell_size_changed and rss_delta <= 15.0 and capture_slowdown <= 10.0
    else "fail"
)
report = {
    "status": status,
    "baseline": baseline,
    "current": current,
    "deltas": {
        "rss_percent": rss_delta,
        "capture_slowdown_percent": capture_slowdown,
        "cell_size_changed": cell_size_changed,
    },
    "budgets": {
        "max_rss_increase_percent": 15.0,
        "max_capture_slowdown_percent": 10.0,
        "cell_size_must_match_baseline": True,
    },
}
print(json.dumps(report, indent=2, sort_keys=True))
raise SystemExit(0 if status == "pass" else 1)
PY
