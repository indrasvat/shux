#!/usr/bin/env bash
# Rich-TUI compatibility for issue #117 (CLAUDE.md hard rule).
#
# The rule: `vim`/`nvim`, `lazygit`, `btop`/`htop` must render correctly in
# panes, and it is a REQUIRED pass for any change to VT parsing. DECALN is a VT
# parsing change, so each TUI is started in a pane that has just been filled
# edge-to-edge with the alignment pattern, and must repaint over it with no
# residue.
#
#   SHUX_BIN=<binary> .shux/scripts/issue_117_richtui_check.sh
#
# Synchronised the same way as the evidence harness: the pane waits for a
# go-file (so the resize lands before anything draws), and the harness waits
# for the TUI's own output before looking at the screen. Nothing here swallows
# a timeout.
#
# A UTF-8 locale is part of the pane env CLAUDE.md names alongside TERM and
# COLORTERM, and it is load-bearing: without it `btop` refuses to start ("No
# UTF-8 locale detected") and `lazygit` exits before drawing, both of which read
# as rendering failures and are not.
#
# Most of these TUIs take the ALTERNATE screen, so the pattern they have to
# survive is on the primary. That is exactly the interesting case: the screen
# they are handed comes out of the one-slot spare, so a pattern-filled buffer
# that was wrongly recycled as blank would show through their UI.
#
# Output: .shux/out/issue-117/richtui/<tui>.{png,txt}. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
out_dir="${repo_root}/.shux/out/issue-117/richtui"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-117-rich.XXXXXX")"
cols="${EVID_COLS:-100}"
rows="${EVID_ROWS:-30}"

sessions=()
failures=0
passes=0
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

sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" "$@"; }

mkdir -p "${out_dir}"
echo "==> rich TUIs over the DECALN pattern: $(${shux_bin} version 2>/dev/null | head -1)"

full_row="$(printf 'E%.0s' $(seq 1 "${cols}"))"

# check <name> <binary> <settle-needle> <argv...>
check() {
  local name="$1" bin="$2" needle="$3"; shift 3
  if ! command -v "${bin}" >/dev/null 2>&1; then
    skipped+=("${name} (not installed)")
    printf '    %-10s SKIPPED — not installed\n' "${name}"
    return 0
  fi

  local session="rich-${name}"
  local script="${runtime}/${session}.sh"
  local go="${runtime}/${session}.go"
  rm -f "${go}"
  {
    printf 'while [ ! -e "%s" ]; do sleep 0.05; done\n' "${go}"
    # Colour probe, then fill the page edge to edge, THEN hand the pane over.
    printf "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n'\n"
    printf "printf '\\033#8'\n"
    printf 'cd "%s"\n' "${repo_root}"
    printf 'exec %s\n' "$*"
  } >"${script}"

  sx session create "${session}" -d --title "${session}" -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
        HOME="${runtime}" sh "${script}" >/dev/null
  sessions+=("${session}")
  local pane
  pane="$(sx --format json pane list -s "${session}" | jq -r '.[0].id')"
  sx pane set-size -s "${session}" -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  : >"${go}"

  # Wait for the TUI's OWN output, then for the screen to stop moving. Both
  # are bounded and both hard-fail; btop and htop redraw forever, so the
  # settle uses a short quiet window rather than demanding true stillness.
  if ! sx pane wait-for -s "${session}" -p "${pane}" -t "${needle}" --timeout-ms 25000 >/dev/null 2>&1; then
    printf '    %-10s FAIL — never showed %s\n' "${name}" "${needle}"
    sx pane capture -s "${session}" -p "${pane}" --lines "${rows}" >"${out_dir}/${name}.txt" || true
    failures=$((failures + 1))
    sx session kill "${session}" >/dev/null 2>&1 || true
    return 0
  fi
  sx pane wait-settled "${pane}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true

  sx pane snapshot -s "${session}" -p "${pane}" -o "${out_dir}/${name}.png" >/dev/null
  sx pane capture -s "${session}" -p "${pane}" --lines "${rows}" >"${out_dir}/${name}.txt"

  # The assertion: not one row of the pattern survived the TUI's repaint.
  local residue
  residue=$(grep -c -- "${full_row}" "${out_dir}/${name}.txt" || true)
  local drew=0
  grep -q -- "${needle}" "${out_dir}/${name}.txt" && drew=1
  if [ "${drew}" = "0" ]; then
    printf '    %-10s FAIL — %s is not on the FINAL screen (it only flashed past)\n' "${name}" "${needle}"
    failures=$((failures + 1))
  elif [ "${residue}" = "0" ]; then
    printf '    %-10s ok   repainted clean (%s bytes png, %s lines)\n' \
      "${name}" "$(wc -c <"${out_dir}/${name}.png")" "$(wc -l <"${out_dir}/${name}.txt")"
    passes=$((passes + 1))
  else
    printf '    %-10s FAIL — %s full row(s) of the pattern survived\n' "${name}" "${residue}"
    failures=$((failures + 1))
  fi

  sx session kill "${session}" >/dev/null 2>&1 || true
  local kept=() s
  for s in "${sessions[@]}"; do [ "${s}" = "${session}" ] || kept+=("${s}"); done
  sessions=("${kept[@]:-}")
  return 0
}

printf 'a file for the editors to open\nsecond line\n' >"${runtime}/edit.txt"

check vim     vim     "a file for the editors" vim -u NONE -N "${runtime}/edit.txt"
check nvim    nvim    "a file for the editors" nvim -u NONE -N "${runtime}/edit.txt"
check htop    htop    "CPU"                    htop
check btop    btop    "cpu"                    btop
check lazygit lazygit "Status"                 lazygit
check less    less    "a file for the editors" less "${runtime}/edit.txt"

echo "==> ${passes} ok, ${failures} failed, ${#skipped[@]} skipped"
for s in "${skipped[@]:-}"; do [ -n "${s}" ] && echo "    skipped: ${s}"; done
if [ "${failures}" -gt 0 ]; then exit 1; fi
