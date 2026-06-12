#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixtures="${VT_CORPUS_FIXTURES:-${repo_root}/.shux/fixtures/vt-corpus}"
goldens="${VT_CORPUS_GOLDENS:-${repo_root}/.shux/goldens/073-vt-corpus}"
out="${VT_CORPUS_OUT:-${repo_root}/.shux/out/073-vt-corpus/promote}"

command -v cargo >/dev/null 2>&1 || { echo "missing required command: cargo" >&2; exit 2; }

mkdir -p "${goldens}" "${out}"

cargo run -p shux-raster --example vt_corpus_harness -- \
  --mode promote \
  --fixtures "${fixtures}" \
  --goldens "${goldens}" \
  --out "${out}"

echo "VT corpus baselines promoted to ${goldens}"
echo "Review the generated files and keep them only with DootSabha approval."
