#!/usr/bin/env bash
# Rich-TUI compatibility for issue #174 (CLAUDE.md hard rule).
#
# The rule: `vim`/`nvim`, `lazygit`, `btop`/`htop` must render correctly in
# panes, and it is a REQUIRED pass for changes to PTY spawn, pane sizing/resize
# and input encoding. This change touches all three: every pane child is now
# TOLD a pixel geometry it never saw before, and every mouse-aware app now
# RECEIVES button events it never saw before.
#
#   SHUX_BIN=<binary> .shux/scripts/issue_174_richtui_check.sh
#
# Two kinds of assertion, because they are worth different amounts:
#
#   · vim and nvim give GROUND TRUTH. Both are asked, in their own words, where
#     the cursor ended up after a click at a known pane-local cell. An
#     off-by-one in the hit-test — the defect most likely to survive a unit
#     test, because the test and the code share the same arithmetic — shows up
#     here as a number that does not match.
#   · htop, btop and lazygit are asked to keep rendering. They take the mouse
#     and they take the alternate screen, so a pane that garbles under a click
#     or under non-zero `ws_xpixel` shows up as a missing needle or as residue.
#
# The colour assertion is made on the TUI's OWN screen, not on a probe line.
# Printing `TRUECOLOR/INDEXED/BASIC` before launch proves nothing here: every
# one of these apps takes the alternate screen, so the probe is on the discarded
# primary and the capture never sees it. A regression that stripped every colour
# while leaving text and cursor placement intact would have passed. Instead the
# snapshot PNG is sampled for distinct colours, which is a property of the
# picture the app actually drew.
#
# Clicks are written to the master side of a real pty running `shux session
# attach`, so crossterm parses them exactly as it would parse a real click.
# Nothing here stubs the client.
#
# A UTF-8 locale is part of the pane env alongside TERM and COLORTERM, and it is
# load-bearing: without it `btop` refuses to start and `lazygit` exits before
# drawing, both of which read as rendering failures and are not.
#
# Output: .shux/out/issue-174/richtui/. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
out_dir="${repo_root}/.shux/out/issue-174/richtui"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-174-rich.XXXXXX")"
driver="${repo_root}/.shux/scripts/lib/pty_drive.py"
cols="${EVID_COLS:-100}"
rows="${EVID_ROWS:-30}"

# The cell the click targets. Pane-local and 1-based; the single pane's rect
# starts at (1,1) with the default outline, so this is also the 0-based screen
# cell, and the wire coordinate is one more again.
target_col=12
target_row=7
wire_col=$((target_col + 1))
wire_row=$((target_row + 1))

sessions=()
failures=0
passes=0
# How many of the checks read the cursor back out of the app (vim/nvim).
ground_truth=0
skipped=()

cleanup() {
  for s in "${sessions[@]:-}"; do
    [ -n "${s}" ] && shux_harness_kill_session "${runtime}" "${shux_bin}" "${s}"
  done
  shux_harness_stop_daemon "${runtime}"
  shux_harness_assert_no_daemon "${runtime}" || shux_harness_stop_daemon "${runtime}"
  sleep 0.5
  rm -rf "${runtime}"
}
trap cleanup EXIT

# `wait-settled ... || true` appears below, which is the pattern CLAUDE.md warns
# turns an error into a fast success. It is safe HERE and only here: every one is
# preceded by a `wait-for` that hard-fails if the content never arrived, so the
# settle is a quieting delay on top of an assertion that already passed, and
# every check after it reads captured content rather than an exit code. A settle
# that times out costs a slightly earlier screenshot, not a false pass.
# An explicit, isolated config: the click target below is computed from the
# DEFAULT rounded one-cell outline, so a developer whose own config says
# `border_style = "none"` would get a one-cell cursor mismatch and a false
# failure even though forwarding is correct. Nothing about this check should
# depend on the host's appearance settings.
config_home="${runtime}/config"
mkdir -p "${config_home}/shux"
printf '[appearance]\nborder_style = "rounded"\n' >"${config_home}/shux/config.toml"

sx() {
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${config_home}" \
    "${shux_bin}" "$@"
}

mkdir -p "${out_dir}"
echo "==> rich TUIs under a real click: $(${shux_bin} version 2>/dev/null | head -1)"
echo "    click target: pane-local (${target_col}, ${target_row})"

# A buffer whose every line is long enough that column N is a real column, and
# whose content names its own line so a mis-scrolled view is visible.
buffer="${runtime}/grid.txt"
python3 - "${buffer}" <<'PY'
import sys
with open(sys.argv[1], "w") as f:
    for i in range(1, 41):
        f.write(f"L{i:02d}." + "abcdefghij" * 9 + "\n")
PY

# start <name> <settle-needle> <argv...>  -- launches the TUI in a fresh session
start() {
  local name="$1" needle="$2"; shift 2
  local session="rich174-${name}"
  local script="${runtime}/${session}.sh"
  local go="${runtime}/${session}.go"
  rm -f "${go}"
  {
    printf 'while [ ! -e "%s" ]; do sleep 0.05; done\n' "${go}"
    printf "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n'\n"
    printf 'cd "%s"\n' "${repo_root}"
    printf 'exec %s\n' "$*"
  } >"${script}"

  sx session create "${session}" -d --title "${session}" -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
        HOME="${runtime}" sh "${script}" >/dev/null
  sessions+=("${session}")
  started_session="${session}"
  started_pane="$(sx --format json pane list -s "${session}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
  sx pane set-size -s "${session}" -p "${started_pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  : >"${go}"
  # Content first, THEN settle. `wait-settled` alone races a slow starter and
  # captures a blank screen that every later assertion then agrees with.
  sx pane wait-for -s "${session}" -p "${started_pane}" -t "${needle}" --timeout-ms 30000 >/dev/null
  sx pane wait-settled "${started_pane}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
}

finish() {
  local session="$1"
  sx session kill "${session}" >/dev/null 2>&1 || true
  local kept=() s
  for s in "${sessions[@]}"; do [ "${s}" = "${session}" ] || kept+=("${s}"); done
  sessions=("${kept[@]:-}")
}

drive() {
  local session="$1" log="$2"; shift 2
  local args=() s
  for s in "$@"; do args+=(--step "${s}"); done
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${config_home}" \
      TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
    python3 "${driver}" --cols "${cols}" --rows "${rows}" --log "${log}" \
      --timeout 90 "${args[@]}" -- "${shux_bin}" session attach -s "${session}"
}

capture() {
  local session="$1" pane="$2" name="$3"
  sx pane wait-settled "${pane}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
  sx pane capture -s "${session}" -p "${pane}" --lines "${rows}" >"${out_dir}/${name}.txt"
  sx pane snapshot -s "${session}" -p "${pane}" -o "${out_dir}/${name}.png" >/dev/null
}

# Assert the TUI's own screen is in colour.
#
# The measure is CHROMA, not a distinct-colour count. Counting colours does not
# discriminate: I tried it first, and a greyscale copy of the real htop screen
# still scored 221 distinct colours because antialiasing alone produces hundreds
# of greys. It would have passed a screen with every colour stripped -- the
# exact regression this is here to catch. Chroma separates them cleanly:
# measured 14.9% on the real screen, 0.000% on the greyscale copy.
#
# 2% is well under every app here and unreachable by a monochrome frame.
assert_colourful() {
  local name="$1"
  local out
  if out="$(uv run --script "${repo_root}/.shux/scripts/lib/png_not_blank.py" \
      "${out_dir}/${name}.png" --min-colors 24 --min-ink-ratio 0.02 \
      --min-chroma-ratio 0.02 2>&1)"; then
    return 0
  fi
  printf '    %-9s FAIL — screen is not in colour; a monochrome regression would pass here\n' \
    "${name}"
  printf '%s\n' "${out}"
  return 1
}

# ── vim / nvim: ground truth on where the click landed ─────────────────────
#
# Each is asked, in its own words, for the cursor position after the click. A
# `-u NONE` editor opens the buffer at line 1 in the top-left of the window, so
# pane-local (col, row) must come back as exactly (row, col).
check_editor() {
  local name="$1" bin="$2"
  if ! command -v "${bin}" >/dev/null 2>&1; then
    skipped+=("${name} (not installed)")
    printf '    %-9s SKIPPED — not installed\n' "${name}"
    return 0
  fi
  start "${name}" "abcdefghij" \
    "${bin}" -u NONE \
    -c "'set mouse=a noswapfile nonumber nowrap laststatus=2 hlsearch ruler'" \
    -c "'silent! /abcdefghij'" \
    -c "'nohlsearch | set hlsearch'" \
    "${buffer}"
  local session="${started_session}" pane="${started_pane}"

  drive "${session}" "${out_dir}/${name}-attach.log" \
    'sleep:3.0' \
    "send:\\x1b[<0;${wire_col};${wire_row}M" 'sleep:0.4' \
    "send:\\x1b[<0;${wire_col};${wire_row}m" 'sleep:0.6' \
    'send::echo "CURSOR=".line(".").",".col(".")\r' 'sleep:1.5'

  capture "${session}" "${pane}" "${name}"
  local got
  got="$(grep -o 'CURSOR=[0-9]*,[0-9]*' "${out_dir}/${name}.txt" | tail -1 || true)"
  local want="CURSOR=${target_row},${target_col}"
  if [ -z "${got}" ]; then
    printf '    %-9s FAIL — %s never reported a cursor position (the click did not reach it)\n' \
      "${name}" "${bin}"
    failures=$((failures + 1))
  elif [ "${got}" != "${want}" ]; then
    printf '    %-9s FAIL — click landed at %s, expected %s\n' "${name}" "${got}" "${want}"
    failures=$((failures + 1))
  elif ! grep -q 'abcdefghij' "${out_dir}/${name}.txt"; then
    printf '    %-9s FAIL — buffer is not on the final screen\n' "${name}"
    failures=$((failures + 1))
  else
    if ! assert_colourful "${name}"; then
      failures=$((failures + 1))
      return 0
    fi
    printf '    %-9s ok   click landed exactly on (%s, %s); %s bytes png\n' \
      "${name}" "${target_col}" "${target_row}" "$(wc -c <"${out_dir}/${name}.png")"
    passes=$((passes + 1))
    ground_truth=$((ground_truth + 1))
  fi
  finish "${session}"
}

# ── htop / btop / lazygit: keep rendering under a click ────────────────────
check_tui() {
  local name="$1" bin="$2" needle="$3"; shift 3
  if ! command -v "${bin}" >/dev/null 2>&1; then
    skipped+=("${name} (not installed)")
    printf '    %-9s SKIPPED — not installed\n' "${name}"
    return 0
  fi
  start "${name}" "${needle}" "$@"
  local session="${started_session}" pane="${started_pane}"

  drive "${session}" "${out_dir}/${name}-attach.log" \
    'sleep:3.0' \
    "send:\\x1b[<0;${wire_col};${wire_row}M" 'sleep:0.4' \
    "send:\\x1b[<0;${wire_col};${wire_row}m" 'sleep:0.6' \
    "send:\\x1b[<0;30;10M" 'sleep:0.3' "send:\\x1b[<32;44;14M" 'sleep:0.3' \
    "send:\\x1b[<0;44;14m" 'sleep:2.0'

  capture "${session}" "${pane}" "${name}"
  # The pane must still be the app's own screen: its needle present, and no
  # bare mouse-report bytes leaked onto it (which is what an app that did NOT
  # consume the reports would show).
  if ! grep -q -- "${needle}" "${out_dir}/${name}.txt"; then
    printf '    %-9s FAIL — %s is not on the final screen after the clicks\n' "${name}" "${needle}"
    failures=$((failures + 1))
  elif grep -q '\[<[0-9]*;[0-9]*;[0-9]*[Mm]' "${out_dir}/${name}.txt"; then
    printf '    %-9s FAIL — raw mouse reports are visible on the screen\n' "${name}"
    failures=$((failures + 1))
  else
    if ! assert_colourful "${name}"; then
      failures=$((failures + 1))
      return 0
    fi
    printf '    %-9s ok   still rendering under clicks; %s bytes png\n' \
      "${name}" "$(wc -c <"${out_dir}/${name}.png")"
    passes=$((passes + 1))
  fi
  finish "${session}"
}

check_editor vim vim
check_editor nvim nvim
check_tui htop htop CPU htop
check_tui btop btop cpu btop --utf-force
check_tui lazygit lazygit Files lazygit

echo "    artifacts: ${out_dir}"
if [ "${#skipped[@]}" -ne 0 ]; then
  printf '    skipped: %s\n' "${skipped[*]}"
fi
# CLAUDE.md's list also names `vicaya` and `vivecaka`. Neither is packaged, so
# neither is covered here; say so rather than let the list look complete.
for absent in vicaya vivecaka; do
  command -v "${absent}" >/dev/null 2>&1 \
    || skipped+=("${absent} (not installed; named in CLAUDE.md's matrix)")
done

if [ "${failures}" -ne 0 ]; then
  echo "==> FAIL (${passes} ok, ${failures} failed)"
  exit 1
fi
# `passes > 0` is too weak a floor: htop/btop/lazygit assert only "still
# renders", which holds with forwarding ripped out entirely. Only vim and nvim
# read the cursor back and can say the click landed on the right cell, so at
# least one of them must actually have run.
if [ "${ground_truth}" -eq 0 ]; then
  echo "==> FAIL — neither vim nor nvim ran; nothing here checked WHERE a click"
  echo "           landed, only that the TUIs still draw. That is not the matrix."
  exit 1
fi
echo "==> PASS (${passes} ok, ${ground_truth} with cursor ground truth)"
