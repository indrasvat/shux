#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
out_dir="${SHUX_ISSUE70_OUT:-${repo_root}/.shux/out/issue-70}"
gh_hound="${GH_HOUND_BIN:-/Users/indrasvat/code/github.com/indrasvat-gh-hound/bin/gh-hound}"
vivecaka="${VIVECAKA_BIN:-/Users/indrasvat/.local/bin/vivecaka}"

mkdir -p "${out_dir}"

runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-issue70.XXXXXX")"
cleanup() {
  shux_harness_cleanup_runtime \
    "${runtime}" \
    "${shux_bin}" \
    issue70-lossless \
    issue70-gh-hound \
    issue70-vivecaka \
    issue70-btop
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="${runtime}"
export TERM=xterm-256color
export COLORTERM=truecolor
unset SHUX_SOCKET

extract_pane_id() {
  python3 -c 'import json,sys; print(json.load(sys.stdin)["pane_id"])'
}

make_payload() {
  python3 - "$1" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
payload = bytearray()
for i in range(8192):
    payload.extend(b"\x1b[2JFRAME:")
    payload.extend(f"{i:04d}".encode())
    payload.extend(b":")
path.write_bytes(bytes(payload))
PY
}

record_exact_payload() {
  local session="issue70-lossless"
  local expected="${out_dir}/expected.raw"
  local actual="${out_dir}/lossless-record.raw"
  local record_json="${out_dir}/lossless-record.json"
  local trigger="${out_dir}/trigger"
  local emit_py="${out_dir}/emit_payload.py"
  local expected_sha actual_sha pane_id create_json

  rm -f "${expected}" "${actual}" "${record_json}" "${trigger}" "${emit_py}"
  make_payload "${expected}"
  cat >"${emit_py}" <<PY
from pathlib import Path
import sys
sys.stdout.buffer.write(Path("${expected}").read_bytes())
PY

  create_json="$("${shux_bin}" --format json session create "${session}" -d --title "issue 70 lossless" -- sh -lc "while [ ! -f '${trigger}' ]; do sleep 0.05; done; python3 '${emit_py}'; sleep 2")"
  pane_id="$(printf '%s' "${create_json}" | extract_pane_id)"

  "${shux_bin}" pane set-size -s "${session}" -p "${pane_id}" --cols 120 --rows 36 >/dev/null
  "${shux_bin}" --format json pane record -s "${session}" -p "${pane_id}" --to "${actual}" --duration-ms 1500 --force >"${record_json}" &
  local record_pid=$!
  sleep 0.2
  touch "${trigger}"
  wait "${record_pid}"

  expected_sha="$(shasum -a 256 "${expected}" | awk '{print $1}')"
  actual_sha="$(shasum -a 256 "${actual}" | awk '{print $1}')"
  if [[ "${expected_sha}" != "${actual_sha}" ]]; then
    echo "lossless record SHA mismatch: expected=${expected_sha} actual=${actual_sha}" >&2
    exit 1
  fi
  python3 - "${record_json}" "${expected}" <<'PY'
from pathlib import Path
import json
import sys

record = json.loads(Path(sys.argv[1]).read_text())
expected_len = Path(sys.argv[2]).stat().st_size
assert record["status"] == "complete", record
assert record["lossless"] is True, record
assert record["bytes_written"] == expected_len, record
PY
}

snapshot_tool() {
  local session="$1"
  local title="$2"
  local png="$3"
  shift 3
  local create_json pane_id raw

  create_json="$("${shux_bin}" --format json session create "${session}" -d --title "${title}" -- "$@")"
  pane_id="$(printf '%s' "${create_json}" | extract_pane_id)"
  "${shux_bin}" pane set-size -s "${session}" -p "${pane_id}" --cols 120 --rows 36 >/dev/null
  sleep 2
  raw="${out_dir}/${session}.raw"
  "${shux_bin}" --format json pane record -s "${session}" -p "${pane_id}" --to "${raw}" --duration-ms 1000 --force >"${out_dir}/${session}-record.json" &
  local record_pid=$!
  sleep 0.2
  case "${session}" in
    issue70-gh-hound)
      "${shux_bin}" pane send-keys -s "${session}" -p "${pane_id}" --data DQ== >/dev/null || true
      ;;
    *)
      "${shux_bin}" pane send-keys -s "${session}" -p "${pane_id}" --text "?" >/dev/null || true
      ;;
  esac
  wait "${record_pid}"
  "${shux_bin}" pane snapshot -s "${session}" -p "${pane_id}" -o "${png}" >/dev/null
  test -s "${png}"
}

record_exact_payload

if command -v gh >/dev/null 2>&1 && gh hound --help >/dev/null 2>&1; then
  snapshot_tool issue70-gh-hound "issue 70 gh-hound" "${out_dir}/gh-hound.png" gh hound --repo indrasvat/shux
elif [[ -x "${gh_hound}" ]]; then
  snapshot_tool issue70-gh-hound "issue 70 gh-hound" "${out_dir}/gh-hound.png" "${gh_hound}" --repo indrasvat/shux
else
  echo "SKIP gh-hound screenshot: neither gh hound nor ${gh_hound} is executable" >&2
fi

if [[ -x "${vivecaka}" ]]; then
  snapshot_tool issue70-vivecaka "issue 70 vivecaka" "${out_dir}/vivecaka.png" "${vivecaka}" --repo indrasvat/shux
else
  echo "SKIP vivecaka screenshot: ${vivecaka} is not executable" >&2
fi

if command -v btop >/dev/null 2>&1; then
  snapshot_tool issue70-btop "issue 70 btop" "${out_dir}/btop.png" btop
else
  echo "SKIP btop screenshot: btop is not on PATH" >&2
fi

echo "issue-70 lossless record proof written to ${out_dir}"
