#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
out_dir="${VT_CORPUS_RECORD_OUT:-${repo_root}/.shux/out/073-vt-corpus/recordings}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-vt-corpus-record.XXXXXX")"

mkdir -p "${out_dir}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 2
  }
}

need jq
need base64
need shasum

cleanup() {
  local sessions=()
  while read -r session; do
    case "${session}" in
      vt-corpus-*) sessions+=("${session}") ;;
    esac
  done < <(
    shux_harness_timeout 8s \
      env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" --format json session list 2>/dev/null \
      | python3 -c 'import json,sys; [print(s["name"]) for s in json.load(sys.stdin)]' 2>/dev/null || true
  )
  shux_harness_cleanup_runtime "${runtime}" "${shux_bin}" "${sessions[@]:-}"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="${runtime}"
export TERM=xterm-256color
export COLORTERM=truecolor
unset SHUX_SOCKET

extract_pane_id() {
  python3 -c 'import json,sys; print(json.load(sys.stdin)["pane_id"])'
}

record_case() {
  local name="$1"
  local command="$2"
  local session="vt-corpus-${name}"
  local trigger="${out_dir}/${name}.trigger"
  local raw="${out_dir}/${name}.raw"
  local json="${out_dir}/${name}.json"
  local capture="${out_dir}/${name}-capture.txt"
  local snapshot="${out_dir}/${name}-snapshot.png"
  local create_json pane_id record_pid

  rm -f "${trigger}" "${raw}" "${json}" "${capture}" "${snapshot}"
  create_json="$("${shux_bin}" --format json session create "${session}" -d --title "${name}" -- sh -lc "while [ ! -f '${trigger}' ]; do sleep 0.05; done; cd '${repo_root}'; exec ${command}")"
  pane_id="$(printf '%s' "${create_json}" | extract_pane_id)"
  "${shux_bin}" pane set-size -s "${session}" -p "${pane_id}" --cols 120 --rows 36 >/dev/null
  "${shux_bin}" --format json pane record -s "${session}" -p "${pane_id}" --to "${raw}" --duration-ms 2500 --force >"${json}" &
  record_pid=$!
  sleep 0.2
  touch "${trigger}"
  wait "${record_pid}" || return 1
  "${shux_bin}" pane capture -s "${session}" -p "${pane_id}" > "${capture}" || true
  "${shux_bin}" --format json pane snapshot -s "${session}" -p "${pane_id}" \
    | jq -r .png_base64 | base64 -d > "${snapshot}" || true
  test -s "${raw}"
  shasum -a 256 "${raw}" | awk -v name="${name}" -v raw="${raw}" -v capture="${capture}" -v snapshot="${snapshot}" '{print name "\t" raw "\t" $1 "\t" capture "\t" snapshot}'
}

{
  if command -v btop >/dev/null 2>&1; then
    record_case btop "btop" || true
  fi
  if command -v lazygit >/dev/null 2>&1; then
    record_case lazygit "lazygit" || true
  fi
  if command -v nvim >/dev/null 2>&1; then
    record_case nvim "nvim --clean -u NONE +'set noruler laststatus=2' +'set statusline=shux-vt-corpus'" || true
  elif command -v vim >/dev/null 2>&1; then
    record_case vim "vim -Nu NONE" || true
  fi
  if command -v vicaya-tui >/dev/null 2>&1; then
    record_case vicaya "vicaya-tui '${repo_root}'" || true
  fi
  if command -v vivecaka >/dev/null 2>&1; then
    record_case vivecaka "vivecaka --repo indrasvat/shux" || true
  fi
} | tee "${out_dir}/recordings.tsv"

echo "Recordings written to ${out_dir}. This target never updates committed fixtures or goldens."
