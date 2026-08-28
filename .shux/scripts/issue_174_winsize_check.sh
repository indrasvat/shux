#!/usr/bin/env bash
# Issue #174 half A: a pane must be TOLD its pixel geometry.
#
# Drives the real daemon: spawns a pane, reads `TIOCGWINSZ` from INSIDE it, then
# resizes the pane and reads again. Both reads must report `ws_xpixel` /
# `ws_ypixel` equal to cols/rows times the cell box shux declares -- before this
# change both were 0 at spawn and stayed 0 across every resize.
#
#   SHUX_BIN=<binary> .shux/scripts/issue_174_winsize_check.sh
#
# CELL_W/CELL_H default to shux's declared cell box (the 14.0 rasterizer's, see
# `shux_pty::handle::DECLARED_CELL_PIXELS`). They are inputs so this script can
# be run against a tree with a different declared box and still be a real test
# rather than a tautology.
#
# The pane also emits the mandatory colour probe, so a monochrome regression in
# the capture path cannot let this pass.
#
# Output: .shux/out/issue-174/winsize/. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
out_dir="${repo_root}/.shux/out/issue-174/winsize"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-174-ws.XXXXXX")"
cell_w="${CELL_W:-9}"
cell_h="${CELL_H:-19}"

resize_cols="${RESIZE_COLS:-64}"
resize_rows="${RESIZE_ROWS:-18}"

session="ws174"
failures=0

cleanup() {
  shux_harness_kill_session "${runtime}" "${shux_bin}" "${session}" || true
  shux_harness_stop_daemon "${runtime}"
  shux_harness_assert_no_daemon "${runtime}" || shux_harness_stop_daemon "${runtime}"
  sleep 0.3
  rm -rf "${runtime}"
}
trap cleanup EXIT

sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" "$@"; }

mkdir -p "${out_dir}"
echo "==> winsize pixel geometry: $(${shux_bin} version 2>/dev/null | head -1)"
echo "    declared cell box: ${cell_w}x${cell_h}px"

# The reader runs INSIDE the pane and prints what the kernel hands its own tty.
# `sys.stdout` is the pane's pty slave, so this is the same fd any app reads.
reader="${runtime}/winsize.py"
cat >"${reader}" <<'PY'
import fcntl, struct, sys, termios, time
tag = sys.argv[1]
prev = None
# The resize arrives asynchronously (TIOCSWINSZ + SIGWINCH); poll until the
# geometry changes or the budget runs out, then print. A fixed sleep here would
# race the daemon and read the OLD size, which is the failure this must not
# manufacture.
want = sys.argv[2] if len(sys.argv) > 2 else None
deadline = time.time() + 10
while True:
    rows, cols, xp, yp = struct.unpack(
        "HHHH", fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b"\0" * 8))
    got = f"{cols}x{rows}"
    if want is None or got == want or time.time() > deadline:
        break
    time.sleep(0.05)
print(f"WINSIZE {tag} cols={cols} rows={rows} xpixel={xp} ypixel={yp}", flush=True)
PY

pane_script="${runtime}/pane.sh"
{
  printf "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n'\n"
  printf 'python3 %s spawn\n' "${reader}"
  printf 'while [ ! -e "%s" ]; do sleep 0.05; done\n' "${runtime}/resized"
  printf 'python3 %s resized %sx%s\n' "${reader}" "${resize_cols}" "${resize_rows}"
  printf 'sleep 60\n'
} >"${pane_script}"

sx session create "${session}" -d --title "${session}" -- \
  env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
      HOME="${runtime}" sh "${pane_script}" >/dev/null
pane="$(sx --format json pane list -s "${session}" | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"

# The SPAWN read is taken at the size the pane was CREATED at -- no `set-size`
# first, or the read would be measuring the resize path twice and the spawn call
# site could stay at zero unnoticed. That size is shux's own default, so the
# spawn assertion checks the CONTRACT (pixels = cells x the declared box) against
# whatever cell size the pane reports, rather than restating a constant.
sx pane wait-for -s "${session}" -p "${pane}" -t "WINSIZE spawn" --timeout-ms 20000 >/dev/null

sx pane set-size -s "${session}" -p "${pane}" --cols "${resize_cols}" --rows "${resize_rows}" >/dev/null
: >"${runtime}/resized"
sx pane wait-for -s "${session}" -p "${pane}" -t "WINSIZE resized" --timeout-ms 20000 >/dev/null
sx pane wait-settled "${pane}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true

capture="${out_dir}/winsize.txt"
sx pane capture -s "${session}" -p "${pane}" --lines 40 >"${capture}"
sx pane snapshot -s "${session}" -p "${pane}" -o "${out_dir}/winsize.png" >/dev/null

if ! grep -q 'TRUECOLOR' "${capture}"; then
  echo "    FAIL — colour probe missing from the capture"
  failures=$((failures + 1))
fi

# `set-size` is what fans the winsize to the pty, so BOTH reads are post-resize
# reads: the first proves spawn+resize agree, the second proves resize alone
# re-declares the pixels rather than zeroing them.
# assert_line <tag> [want_cols want_rows]
#
# Always asserts the contract: pixels are non-zero AND exactly cells x the
# declared cell box. `want_cols`/`want_rows` additionally pin the cell geometry
# where the harness commanded it.
assert_line() {
  local tag="$1" want_cols="${2:-}" want_rows="${3:-}"
  local line
  line="$(grep -m1 "^WINSIZE ${tag} " "${capture}" || true)"
  if [ -z "${line}" ]; then
    echo "    FAIL — no WINSIZE ${tag} line in the capture"
    failures=$((failures + 1))
    return 0
  fi
  local cols rows xp yp
  cols="${line#*cols=}"; cols="${cols%% *}"
  rows="${line#*rows=}"; rows="${rows%% *}"
  xp="${line#*xpixel=}"; xp="${xp%% *}"
  yp="${line#*ypixel=}"; yp="${yp%% *}"
  if [ -n "${want_cols}" ] && { [ "${cols}" != "${want_cols}" ] || [ "${rows}" != "${want_rows}" ]; }; then
    echo "    FAIL — ${tag}: pane read ${cols}x${rows}, expected ${want_cols}x${want_rows}"
    failures=$((failures + 1))
    return 0
  fi
  local want_xp=$(( cols * cell_w ))
  local want_yp=$(( rows * cell_h ))
  if [ "${xp}" = "0" ] || [ "${yp}" = "0" ]; then
    echo "    FAIL — ${tag}: pixels are ZERO (xpixel=${xp} ypixel=${yp}) — the defect"
    failures=$((failures + 1))
    return 0
  fi
  if [ "${xp}" != "${want_xp}" ] || [ "${yp}" != "${want_yp}" ]; then
    echo "    FAIL — ${tag}: xpixel=${xp} ypixel=${yp}, expected ${want_xp}/${want_yp}"
    failures=$((failures + 1))
    return 0
  fi
  echo "    ok   ${tag}: ${cols}x${rows} cells = ${xp}x${yp} px"
}

assert_line spawn
assert_line resized "${resize_cols}" "${resize_rows}"

echo "    artifacts: ${out_dir}"
if [ "${failures}" -ne 0 ]; then
  echo "==> FAIL (${failures})"
  exit 1
fi
echo "==> PASS"
