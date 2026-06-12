#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
out_dir="${SHUX_LIBGHOSTTY_RECORD_OUT:-${repo_root}/.shux/out/libghostty-vt-replacement/recordings}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-libghostty-record.XXXXXX")"

mkdir -p "${out_dir}"

cleanup() {
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" --format json session list 2>/dev/null \
    | python3 -c 'import json,sys; [print(s["name"]) for s in json.load(sys.stdin)]' 2>/dev/null \
    | while read -r session; do
        case "${session}" in
          libghostty-*) env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" session kill "${session}" >/dev/null 2>&1 || true ;;
        esac
      done || true
  rm -rf "${runtime}"
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
  local session="libghostty-${name}"
  local trigger="${out_dir}/${name}.trigger"
  local raw="${out_dir}/${name}.raw"
  local json="${out_dir}/${name}.json"
  local create_json pane_id record_pid

  rm -f "${trigger}" "${raw}" "${json}"
  create_json="$("${shux_bin}" --format json session create "${session}" -d --title "${name}" -- sh -lc "while [ ! -f '${trigger}' ]; do sleep 0.05; done; cd '${repo_root}'; exec ${command}")"
  pane_id="$(printf '%s' "${create_json}" | extract_pane_id)"
  "${shux_bin}" pane set-size -s "${session}" -p "${pane_id}" --cols 120 --rows 36 >/dev/null
  "${shux_bin}" --format json pane record -s "${session}" -p "${pane_id}" --to "${raw}" --duration-ms 2500 --force >"${json}" &
  record_pid=$!
  sleep 0.2
  touch "${trigger}"
  wait "${record_pid}" || return 1
  test -s "${raw}"
  echo "${name}:${raw}"
}

{
  if command -v btop >/dev/null 2>&1; then
    record_case btop "btop" || true
  fi
  if command -v lazygit >/dev/null 2>&1; then
    record_case lazygit "lazygit" || true
  fi
  if command -v nvim >/dev/null 2>&1; then
    record_case nvim "nvim --clean -u NONE +'set noruler laststatus=2' +'set statusline=libghostty-shux-vt-spike'" || true
  elif command -v vim >/dev/null 2>&1; then
    record_case vim "vim -Nu NONE" || true
  fi
  if command -v vicaya-tui >/dev/null 2>&1; then
    record_case vicaya "vicaya-tui '${repo_root}'" || true
  elif command -v vicaya >/dev/null 2>&1; then
    record_case vicaya "vicaya" || true
  fi
  if command -v vivecaka >/dev/null 2>&1; then
    record_case vivecaka "vivecaka --repo indrasvat/shux" || true
  fi
} | tee "${out_dir}/recordings.txt"
