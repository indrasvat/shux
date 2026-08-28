#!/usr/bin/env bash
# Issue #174 half B: button events must reach a mouse-aware app in a pane.
#
# Drives the REAL client on a REAL pty: `shux attach` runs under a pseudo-
# terminal and the harness writes genuine SGR mouse sequences to that pty's
# master, so crossterm parses them exactly as it would parse a click from a
# terminal emulator. Nothing here stubs the client, the protocol frame, or the
# daemon's routing.
#
#   SHUX_BIN=<binary> .shux/scripts/issue_174_mouse_check.sh
#
# The pane runs a mouse-aware echo: it enables SGR mouse tracking exactly as
# vim/htop/terminal-browser do, drops line discipline so nothing but our own
# writer touches the screen, and prints every byte it is handed. What lands on
# that screen IS what the app received.
#
# Assertions:
#   1. mode 1002 (button-event): press, drag and release all arrive, SGR-encoded,
#      at the right PANE-LOCAL cell, with the release keeping its real button
#      and ending in lowercase `m`
#   2. a SHIFT-held gesture delivers NOTHING to the app -- the escape hatch that
#      keeps shux's own text selection reachable
#   3. mode 1000 (normal): press and release arrive but the DRAG does not. A
#      real terminal reports only what the app asked for, and 1000 does not ask
#      for motion. Getting this wrong is invisible until an app mishandles an
#      event it never subscribed to.
#   4. a pane whose app did NOT ask for the mouse receives nothing at all
#   5. the coordinate is right under `appearance.border_style = "none"` too.
#      The compositor drops the 1-cell outline inset when no outline is drawn,
#      so the pane starts at the origin and the SAME wire coordinate must come
#      out one cell further into the pane. The hit-test used to inset
#      unconditionally, which put every click one cell off under that config
#      and made the last column and row unreachable.
#
# Output: .shux/out/issue-174/mouse/. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
out_dir="${repo_root}/.shux/out/issue-174/mouse"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-174-ms.XXXXXX")"
driver="${repo_root}/.shux/scripts/lib/pty_drive.py"

cols="${EVID_COLS:-100}"
rows="${EVID_ROWS:-30}"
failures=0
sessions=()

cleanup() {
  for s in "${sessions[@]:-}"; do
    [ -n "${s}" ] && shux_harness_kill_session "${runtime}" "${shux_bin}" "${s}"
  done
  shux_harness_stop_daemon "${runtime}"
  shux_harness_assert_no_daemon "${runtime}" || shux_harness_stop_daemon "${runtime}"
  sleep 0.3
  rm -rf "${runtime}"
}
trap cleanup EXIT

# The daemon reads its config from `$XDG_CONFIG_HOME/shux/config.toml`, so the
# config state is part of the environment every invocation shares.
config_home="${runtime}/config"
mkdir -p "${config_home}/shux"

sx() {
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${config_home}" \
    "${shux_bin}" "$@"
}

mkdir -p "${out_dir}"
echo "==> mouse button forwarding: $(${shux_bin} version 2>/dev/null | head -1)"

# start_pane <session> <mode: off|1000|1002> -- sets $started_pane
#
# Sets a global rather than echoing the id: called in `$( )` the `sessions+=`
# below would run in a subshell, the cleanup array would stay empty, and every
# run would leak a session and its daemon.
started_pane=""
start_pane() {
  local session="$1" mode="$2"
  local script="${runtime}/${session}.sh"
  {
    printf "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n'\n"
    # Raw mode with echo off: the only thing that can put a mouse report on this
    # screen is our own reader, so a match cannot be the tty echoing itself.
    printf 'stty raw -echo\n'
    case "${mode}" in
      1000) printf "printf '\\033[?1000h\\033[?1006h'\n" ;;
      1002) printf "printf '\\033[?1002h\\033[?1006h'\n" ;;
      off)  : ;;
      *)    echo "bad mode ${mode}" >&2; exit 2 ;;
    esac
    printf "printf 'READY\\r\\n'\n"
    # `cat -v` renders ESC as ^[ so the reports are greppable text.
    printf 'exec cat -v\n'
  } >"${script}"

  sx session create "${session}" -d --title "${session}" -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
        HOME="${runtime}" sh "${script}" >/dev/null
  sessions+=("${session}")
  started_pane="$(sx --format json pane list -s "${session}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
}

# attach_and_send <session> <log> <step...>
attach_and_send() {
  local session="$1" log="$2"; shift 2
  local args=()
  local s
  for s in "$@"; do args+=(--step "${s}"); done
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${config_home}" \
      TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
    python3 "${driver}" --cols "${cols}" --rows "${rows}" --log "${log}" \
      --timeout 60 "${args[@]}" \
      -- "${shux_bin}" session attach -s "${session}"
}

# expect <label> <capture> <needle> <present:0|1>
expect() {
  local label="$1" capture="$2" needle="$3" present="$4"
  if grep -qF -- "${needle}" "${capture}"; then
    if [ "${present}" = "1" ]; then
      printf '    %-34s ok   saw %s\n' "${label}" "${needle}"
    else
      printf '    %-34s FAIL — %s reached the app and must not have\n' "${label}" "${needle}"
      failures=$((failures + 1))
    fi
  else
    if [ "${present}" = "1" ]; then
      printf '    %-34s FAIL — %s never reached the app\n' "${label}" "${needle}"
      failures=$((failures + 1))
    else
      printf '    %-34s ok   %s correctly withheld\n' "${label}" "${needle}"
    fi
  fi
}

# ── 1-2: a button-event (1002) pane -- press, drag, release ────────────────
start_pane mouse-btn 1002
pane="${started_pane}"
sx pane wait-for -s mouse-btn -p "${pane}" -t READY --timeout-ms 20000 >/dev/null

# Wire coordinates are 1-based; crossterm reports 0-based; a single pane's rect
# starts at (1,1) after the outline inset. So wire col 11 / row 6 is screen
# (10, 5) and pane-local (10, 5) -- the numbers asserted below.
#
#   plain click      press+release at wire (11,6)
#   drag gesture     press (21,9) -> drag (26,9) -> release (26,9)
#   shift gesture    Cb|4 press (41,13) -> drag (46,13) -> release (46,13)
attach_and_send mouse-btn "${out_dir}/attach-mouse-btn.log" \
  'sleep:2.5' \
  'send:\x1b[<0;11;6M' 'sleep:0.3' 'send:\x1b[<0;11;6m' 'sleep:0.5' \
  'send:\x1b[<0;21;9M' 'sleep:0.3' 'send:\x1b[<32;26;9M' 'sleep:0.3' \
  'send:\x1b[<0;26;9m' 'sleep:0.5' \
  'send:\x1b[<4;41;13M' 'sleep:0.3' 'send:\x1b[<36;46;13M' 'sleep:0.3' \
  'send:\x1b[<4;46;13m' 'sleep:1.5'

sx pane wait-settled "${pane}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
cap_btn="${out_dir}/mouse-1002.txt"
sx pane capture -s mouse-btn -p "${pane}" --lines "${rows}" >"${cap_btn}"
sx pane snapshot -s mouse-btn -p "${pane}" -o "${out_dir}/mouse-1002.png" >/dev/null

expect "1002 click press"      "${cap_btn}" '^[[<0;10;5M'  1
expect "1002 click release"    "${cap_btn}" '^[[<0;10;5m'  1
expect "1002 drag press"       "${cap_btn}" '^[[<0;20;8M'  1
expect "1002 drag motion"      "${cap_btn}" '^[[<32;25;8M' 1
expect "1002 drag release"     "${cap_btn}" '^[[<0;25;8m'  1
expect "shift press withheld"  "${cap_btn}" ';40;12'       0
expect "shift drag withheld"   "${cap_btn}" ';45;12'       0

# ── 3: a normal-tracking (1000) pane -- press and release only ─────────────
start_pane mouse-norm 1000
pane_norm="${started_pane}"
sx pane wait-for -s mouse-norm -p "${pane_norm}" -t READY --timeout-ms 20000 >/dev/null
attach_and_send mouse-norm "${out_dir}/attach-mouse-norm.log" \
  'sleep:2.5' \
  'send:\x1b[<0;21;9M' 'sleep:0.3' 'send:\x1b[<32;26;9M' 'sleep:0.3' \
  'send:\x1b[<0;26;9m' 'sleep:1.5'
sx pane wait-settled "${pane_norm}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
cap_norm="${out_dir}/mouse-1000.txt"
sx pane capture -s mouse-norm -p "${pane_norm}" --lines "${rows}" >"${cap_norm}"
sx pane snapshot -s mouse-norm -p "${pane_norm}" -o "${out_dir}/mouse-1000.png" >/dev/null

expect "1000 press arrives"    "${cap_norm}" '^[[<0;20;8M'  1
expect "1000 release arrives"  "${cap_norm}" '^[[<0;25;8m'  1
expect "1000 drag withheld"    "${cap_norm}" '^[[<32;'      0

# ── 4: a pane that never asked for the mouse ───────────────────────────────
start_pane mouse-off off
pane_off="${started_pane}"
sx pane wait-for -s mouse-off -p "${pane_off}" -t READY --timeout-ms 20000 >/dev/null
attach_and_send mouse-off "${out_dir}/attach-mouse-off.log" \
  'sleep:2.5' 'send:\x1b[<0;11;6M' 'sleep:0.3' 'send:\x1b[<0;11;6m' 'sleep:1.5'
sx pane wait-settled "${pane_off}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
cap_off="${out_dir}/mouse-off.txt"
sx pane capture -s mouse-off -p "${pane_off}" --lines "${rows}" >"${cap_off}"
sx pane snapshot -s mouse-off -p "${pane_off}" -o "${out_dir}/mouse-off.png" >/dev/null
expect "no-mouse app gets nothing" "${cap_off}" '^[[<' 0

# ── 5: the same click under `border_style = "none"` ────────────────────────
#
# A fresh daemon, because config is read once at daemon start. Sessions are
# killed FIRST so the array cleanup drains stays accurate -- stopping the daemon
# out from under live sessions and then clearing the array would leave the exit
# trap unable to say what it had actually cleaned up.
for s in "${sessions[@]:-}"; do
  [ -n "${s}" ] && shux_harness_kill_session "${runtime}" "${shux_bin}" "${s}"
done
sessions=()
shux_harness_stop_daemon "${runtime}"
shux_harness_assert_no_daemon "${runtime}" || shux_harness_stop_daemon "${runtime}"
cat >"${config_home}/shux/config.toml" <<'TOML'
[appearance]
border_style = "none"
TOML

start_pane mouse-noborder 1002
pane_nb="${started_pane}"
sx pane wait-for -s mouse-noborder -p "${pane_nb}" -t READY --timeout-ms 20000 >/dev/null
attach_and_send mouse-noborder "${out_dir}/attach-mouse-noborder.log" \
  'sleep:2.5' 'send:\x1b[<0;11;6M' 'sleep:0.3' 'send:\x1b[<0;11;6m' 'sleep:1.5'
sx pane wait-settled "${pane_nb}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
cap_nb="${out_dir}/mouse-noborder.txt"
sx pane capture -s mouse-noborder -p "${pane_nb}" --lines "${rows}" >"${cap_nb}"
sx pane snapshot -s mouse-noborder -p "${pane_nb}" -o "${out_dir}/mouse-noborder.png" >/dev/null

# Wire (11,6) is screen (10,5) 0-based. With no outline the pane rect starts at
# (0,0), so the pane-local 1-based cell is (11,6) — one further in than the
# (10,5) the default outline produces.
expect "no-outline press"      "${cap_nb}" '^[[<0;11;6M' 1
expect "no-outline release"    "${cap_nb}" '^[[<0;11;6m' 1
expect "no-outline not skewed" "${cap_nb}" '^[[<0;10;5M' 0

for c in "${cap_btn}" "${cap_norm}" "${cap_off}" "${cap_nb}"; do
  if ! grep -q 'TRUECOLOR' "${c}"; then
    printf '    %-34s FAIL — colour probe missing from %s\n' "colour probe" "$(basename "${c}")"
    failures=$((failures + 1))
  fi
done

echo "    artifacts: ${out_dir}"
if [ "${failures}" -ne 0 ]; then
  echo "==> FAIL (${failures})"
  exit 1
fi
echo "==> PASS"
