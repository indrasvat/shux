#!/usr/bin/env bash
# Visual evidence for issues #126 (overflowed control sequences executed) and
# #127 (DEL stored as a printable cell).
#
#   SHUX_BIN=<fixed binary> LABEL=head                  .shux/scripts/issue_126_127_evidence.sh
#   SHUX_BIN=<base binary>  LABEL=base EXPECT_DEFECT=1  .shux/scripts/issue_126_127_evidence.sh
#
# ORACLE. Each geometry shoots two panes through the SAME binary: `actual`
# sends a stream laced with DEL bytes and with sequences that overflow vte's
# parameter or intermediate limits; `expected` sends the same stream with every
# one of those removed, which is exactly what a conforming terminal (VT500
# state machine, Alacritty on the same vte release) shows. The expected PNG is
# derived from the specification, never minted: 0 changed pixels is the pass
# condition, and the base binary must NOT reach it (EXPECT_DEFECT=1 fails the
# run if the defect does not reproduce, so that arm cannot pass vacuously).
#
# Every scene carries a truecolor, 256-indexed and basic ANSI probe.
#
# Output: .shux/out/vt-ignored-seq-and-del/<label>/ (gitignored scratch).

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
label="${LABEL:-head}"
expect_defect="${EXPECT_DEFECT:-0}"
geometries="${GEOMETRIES:-80x24 120x40 200x60}"
out_dir="${repo_root}/.shux/out/vt-ignored-seq-and-del/${label}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/sx126.XXXXXX")"
pixel_verify="${repo_root}/.claude/automations/pixel_verify.py"

for tool in jq uv "${shux_bin}"; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "FATAL: ${tool} not found" >&2
    exit 2
  fi
done

sessions=()
cleanup() {
  for s in "${sessions[@]:-}"; do
    [ -n "${s}" ] && shux_harness_kill_session "${runtime}" "${shux_bin}" "${s}"
  done
  shux_harness_stop_daemon "${runtime}"
  shux_harness_assert_no_daemon "${runtime}" || shux_harness_stop_daemon "${runtime}"
  rm -rf "${runtime}"
}
trap cleanup EXIT

sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" "$@"; }

mkdir -p "${out_dir}"
echo "==> ${label}: $(sx version 2>/dev/null | head -1)"

# `CSI <first>` padded with `;1` to 40 parameters, past vte's limit of 32.
over() { # over <first-params> <final>
  local s="\\033[$1" _
  for _ in $(seq 1 39); do s="${s};1"; done
  printf '%s%s' "${s}" "$2"
}

# scene_body <cols> <rows> <with-noise: 1|0>
scene_body() {
  local cols="$1" rows="$2" noise="$3"
  local del='' rep='' el='' sgr='' rqm='' esc=''
  if [ "${noise}" = 1 ]; then
    del='\177'
    rep="$(over 3 b)"
    el="$(over 2 K)"
    sgr="$(over 41 m)"
    # shellcheck disable=SC2016  # literal $$: DECRQM with a third intermediate
    rqm='\033[?2026$$p'
    esc='\033###8'
  fi
  local fill
  fill="$(printf 'w%.0s' $(seq 1 "${cols}"))"
  cat <<EOF
printf '\033[1;1H\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m'
printf '\033[3;1Hdel:  \033[38;5;208mA${del}B${del}C\033[0m e${del}\314\201 end'
printf '\033[4;1Hrep:  Q${rep}R\033[2b'
printf '\033[5;1Hel:   this line survives${el}'
printf '\033[6;1Hsgr:  ${sgr}plain pen\033[0m'
printf '\033[7;1Hesc:  ${esc}no alignment fill'
printf '${rqm}'
printf '\033[9;1H${fill}${del}'
printf 'wrapped'
printf '\033[${rows};1Hdone'
EOF
}

# shoot <session> <cols> <rows> <png>
shoot() {
  local session="$1" cols="$2" rows="$3" png="$4" body="$5"
  local script="${runtime}/${session}.sh" go="${runtime}/${session}.go" done_file="${runtime}/${session}.done"
  {
    printf 'while [ ! -e "%s" ]; do sleep 0.05; done\n' "${go}"
    printf '%s\n' "${body}"
    printf ': >"%s"\n' "${done_file}"
    printf 'exec sleep 900\n'
  } >"${script}"
  sx session create "${session}" -d --title "${session}" -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 sh "${script}" >/dev/null
  sessions+=("${session}")
  local pane
  pane="$(sx --format json pane list -s "${session}" | jq -r '.[0].id')"
  sx pane set-size -s "${session}" -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  : >"${go}"
  local deadline=$((SECONDS + 30))
  while [ ! -e "${done_file}" ]; do
    if [ "${SECONDS}" -ge "${deadline}" ]; then
      echo "FATAL: ${session} never finished its script" >&2
      exit 1
    fi
    sleep 0.1
  done
  sx pane wait-settled "${pane}" --quiet 400 --timeout 15000 >/dev/null
  sx pane snapshot -s "${session}" -p "${pane}" -o "${png}" >/dev/null
  sx pane capture -s "${session}" -p "${pane}" --lines "${rows}" >"${png%.png}.txt"
  for probe in TRUECOLOR INDEXED BASIC "done"; do
    if ! grep -q "${probe}" "${png%.png}.txt"; then
      echo "FATAL: ${session} capture is missing ${probe}" >&2
      exit 1
    fi
  done
  sx session kill "${session}" >/dev/null 2>&1 || true
}

failures=0
for geo in ${geometries}; do
  cols="${geo%x*}"
  rows="${geo#*x}"
  actual="${out_dir}/scene-${geo}-actual.png"
  expected="${out_dir}/scene-${geo}-expected.png"
  diff_png="${out_dir}/scene-${geo}-diff.png"
  metric="${out_dir}/pixel-scene-${geo}.json"
  shoot "ev126a-${geo}" "${cols}" "${rows}" "${actual}" "$(scene_body "${cols}" "${rows}" 1)"
  shoot "ev126e-${geo}" "${cols}" "${rows}" "${expected}" "$(scene_body "${cols}" "${rows}" 0)"
  # A comparison counts only when pixel_verify wrote a complete metric whose
  # status agrees with its exit code. A crash also exits 1, and must never read
  # as a reproduced defect.
  status=0
  uv run --script "${pixel_verify}" "${actual}" "${expected}" --diff "${diff_png}" \
    --max-pixel-diff-ratio 0 --max-mean-channel-delta 0 >"${metric}" || status=$?
  case "${status}" in
    0) verdict=pass ;;
    1) verdict=fail ;;
    *) verdict=none ;;
  esac
  if [ "${verdict}" = none ] ||
    ! jq -e --arg v "${verdict}" \
      '.status == $v and (.changed_pixels | type == "number")' "${metric}" >/dev/null 2>&1; then
    echo "FATAL: pixel_verify did not compare ${geo} (exit ${status})" >&2
    exit 2
  fi
  changed="$(jq -r '.changed_pixels' "${metric}")"
  printf '  %-8s %-4s changed_pixels=%s\n' "${geo}" "${verdict}" "${changed}"
  if [ "${expect_defect}" = 1 ]; then
    [ "${verdict}" = fail ] || failures=$((failures + 1))
  else
    [ "${verdict}" = pass ] || failures=$((failures + 1))
    if grep -q $'\x7f' "${actual%.png}.txt"; then
      echo "  ${geo}: DEL reached pane capture" >&2
      failures=$((failures + 1))
    fi
  fi
done

if [ "${failures}" -ne 0 ]; then
  if [ "${expect_defect}" = 1 ]; then
    echo "FAIL: ${failures} geometries did not reproduce the defect" >&2
  else
    echo "FAIL: ${failures} geometries differ from the oracle" >&2
  fi
  exit 1
fi
echo "OK (${label})"
