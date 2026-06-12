#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixtures="${VT_CORPUS_FIXTURES:-${repo_root}/.shux/fixtures/vt-corpus}"
goldens="${VT_CORPUS_GOLDENS:-${repo_root}/.shux/goldens/073-vt-corpus}"
out="${VT_CORPUS_OUT:-${repo_root}/.shux/out/073-vt-corpus}"
qa="${VT_CORPUS_QA:-${repo_root}/.shux/qa/073-shux-vt-corpus-regression-harness}"
rendered="${out}/rendered"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 2
  }
}

need cargo
need jq
need uv

rm -rf "${rendered}"
mkdir -p "${rendered}" "${qa}"

cargo run -p shux-raster --example vt_corpus_harness -- \
  --mode verify \
  --fixtures "${fixtures}" \
  --goldens "${goldens}" \
  --out "${rendered}"

find "${qa}" -maxdepth 1 -type f \( \
  -name '*-actual.png' -o \
  -name '*-diff.png' -o \
  -name '*-pixel.json' -o \
  -name 'pixel-report.json' -o \
  -name 'corpus-report.json' -o \
  -name 'evidence-manifest.json' \
\) -delete

cp "${rendered}/corpus-report.json" "${qa}/corpus-report.json"

failed=0
screenshots=()
pixel_metrics=()

while IFS=$'\t' read -r layer name actual expected diff; do
  case_id="${layer}-${name}"
  qa_actual="${qa}/${case_id}-actual.png"
  qa_diff="${qa}/${case_id}-diff.png"
  qa_metric="${qa}/${case_id}-pixel.json"
  cp "${actual}" "${qa_actual}"
  if ! uv run --script "${repo_root}/.claude/automations/pixel_verify.py" \
    "${actual}" "${expected}" \
    --diff "${qa_diff}" \
    --max-pixel-diff-ratio 0 \
    --max-mean-channel-delta 0 > "${qa_metric}"; then
    failed=1
  fi
  screenshots+=("$(basename "${qa_actual}")")
  screenshots+=("$(basename "${qa_diff}")")
  pixel_metrics+=("$(basename "${qa_metric}")")
done < <(jq -r '.cases[] | [.layer, .name, .actual_png, .expected_png, .diff_png] | @tsv' "${rendered}/corpus-report.json")

pixel_report_inputs=()
for metric in "${pixel_metrics[@]}"; do
  pixel_report_inputs+=("${qa}/${metric}")
done
jq -s '{schema_version: 1, cases: .}' "${pixel_report_inputs[@]}" > "${qa}/pixel-report.json"

screenshots_json="$(printf '%s\n' "${screenshots[@]}" | jq -R . | jq -s .)"
pixel_metrics_json="$(printf '%s\n' "${pixel_metrics[@]}" | jq -R . | jq -s .)"

jq -n \
  --arg task "073-shux-vt-corpus-regression-harness" \
  --arg solid "SOLID-QA.md" \
  --arg design "dootsabha-design.json" \
  --arg impl "dootsabha-implementation.json" \
  --arg corpus_report "corpus-report.json" \
  --arg pixel_report "pixel-report.json" \
  --argjson screenshots "${screenshots_json}" \
  --argjson pixel_metrics "${pixel_metrics_json}" \
  '{
    task: $task,
    solid_qa_report: $solid,
    dootsabha_design: $design,
    dootsabha_implementation: $impl,
    reports: [$corpus_report, $pixel_report],
    screenshots: $screenshots,
    pixel_metrics: $pixel_metrics
  }' > "${qa}/evidence-manifest.json"

if [[ "${failed}" -ne 0 ]]; then
  echo "VT corpus pixel comparison failed; inspect ${qa}" >&2
  exit 1
fi

echo "VT corpus verification passed; evidence written to ${qa}"
