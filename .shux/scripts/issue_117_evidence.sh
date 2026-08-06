#!/usr/bin/env bash
# Visual evidence for issue #117 — DECALN (`ESC # 8`) silently ignored.
#
# Runs against whichever binary `SHUX_BIN` points at, so the same scenes can be
# shot before and after the fix and compared:
#
#   SHUX_BIN=<base binary>  LABEL=before .shux/scripts/issue_117_evidence.sh
#   SHUX_BIN=<fixed binary> LABEL=after  .shux/scripts/issue_117_evidence.sh
#
# Scenes (each shot through shux's own rasterizer, from a real pane running a
# real shell over a real PTY):
#
#   alignment-pattern  a pane that emits the DEC screen-alignment test. Before
#                      the fix the sequence is dropped and the screen still
#                      shows whatever was there; after, it is a wall of `E`.
#   conformance-run    what a terminal test suite sees: the alignment pattern
#                      as a backdrop with corner and centre marks drawn on it.
#   scroll-region      a scroll region set, a loud SGR pen selected, then
#                      DECALN, then four characters. Pins three clauses at
#                      once: the region does not clip the fill, the pen does
#                      not colour it, and the cursor ends up at home.
#   alt-recycle        one application fills the alternate screen with the
#                      pattern and leaves; a second application enters. The
#                      alternate buffer is recycled between them, so a fill
#                      that did not register as a write would show the first
#                      application's screen to the second.
#   richtui-vim        vim opened in a pane that has just been filled with the
#                      pattern. A rich TUI must repaint over it completely.
#
# SYNCHRONISATION. Every scene's pane touches a done-file after its last write
# and only then parks. The harness waits for that file before it looks at the
# screen, so it can never photograph a pane in the middle of its own script —
# `wait-settled` alone cannot tell "finished" from "not started yet". The wait
# is bounded and hard-fails; nothing here is allowed to swallow a timeout.
#
# `after` additionally ASSERTS: a wrong shot fails the script rather than being
# filed as evidence. `before` records whatever the base build did.
#
# Output: .shux/out/issue-117/<label>/*.png (+ .txt). Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
label="${LABEL:-after}"
out_dir="${repo_root}/.shux/out/issue-117/${label}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-117-${label}.XXXXXX")"

# Deliberately small panes: the rasterizer draws a fixed cell size, so fewer
# columns means each glyph is a larger share of the frame. The point of these
# shots is to be readable at a glance, not to survey a desktop.
cols="${EVID_COLS:-48}"
rows="${EVID_ROWS:-14}"

sessions=()
failures=0

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
echo "==> ${label}: $(${shux_bin} version 2>/dev/null | head -1)"

# A colour probe on every scene, so a monochrome regression cannot pass as a
# clean shot: truecolor, 256-indexed and basic ANSI all present.
probe='\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m'

full_row="$(printf 'E%.0s' $(seq 1 "${cols}"))"

# ── helpers ─────────────────────────────────────────────────────────────

session_of=""
pane_of=""

# start <session> <script-body>
# Spawns a pane running the body, then blocks until the body has finished
# writing and the screen has gone quiet. Sets `session_of` / `pane_of`.
start() {
  local session="$1" body="$2" tail_cmd="${3:-sleep 900}"
  local script="${runtime}/${session}.sh"
  local done_file="${runtime}/${session}.done"
  local go_file="${runtime}/${session}.go"
  rm -f "${done_file}" "${go_file}"
  {
    # The pane is spawned at the daemon's default geometry and resized by the
    # harness a moment later. A script that draws before that lands is drawing
    # on the wrong screen and its output is then REFLOWED, which is a different
    # picture entirely — so it waits for the go-file the resize is followed by.
    printf 'while [ ! -e "%s" ]; do sleep 0.05; done\n' "${go_file}"
    printf '%s\n' "${body}"
    printf ': >"%s"\n' "${done_file}"
    printf 'exec %s\n' "${tail_cmd}"
  } >"${script}"

  sx session create "${session}" -d --title "${session}" -- \
    env TERM=xterm-256color COLORTERM=truecolor sh "${script}" >/dev/null
  sessions+=("${session}")
  local pane
  pane="$(sx --format json pane list -s "${session}" | jq -r '.[0].id')"
  sx pane set-size -s "${session}" -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  : >"${go_file}"

  # 1. The pane's own script has written its last byte to the PTY.
  local deadline=$((SECONDS + 30))
  while [ ! -e "${done_file}" ]; do
    if [ "${SECONDS}" -ge "${deadline}" ]; then
      echo "FATAL: ${session} never finished its script" >&2
      exit 1
    fi
    sleep 0.1
  done
  # 2. The daemon has consumed them and the screen has stopped moving. Both
  #    steps are required: (1) alone races the daemon's PTY read, (2) alone
  #    races the pane's own startup.
  sx pane wait-settled "${pane}" --quiet 400 --timeout 15000 >/dev/null

  session_of="${session}"
  pane_of="${pane}"
}

shoot() { # shoot <name>
  sx pane snapshot -s "${session_of}" -p "${pane_of}" -o "${out_dir}/$1.png" >/dev/null
  sx pane capture -s "${session_of}" -p "${pane_of}" >"${out_dir}/$1.txt"
  # A valid PNG of the right dimensions can still be blank, so the .txt beside
  # it is what the assertions read.
  printf '    %-24s %8s bytes png, %3s lines text\n' \
    "$1" "$(wc -c <"${out_dir}/$1.png")" "$(wc -l <"${out_dir}/$1.txt")"
}

finish() { # finish -- tear the scene's session down
  sx session kill "${session_of}" >/dev/null 2>&1 || true
  local kept=()
  local s
  for s in "${sessions[@]}"; do
    [ "${s}" = "${session_of}" ] || kept+=("${s}")
  done
  sessions=("${kept[@]:-}")
  session_of=""
  pane_of=""
}

# expect <name> <yes|no> <pattern> <description>
expect() {
  local name="$1" want_match="$2" pattern="$3" desc="$4"
  local hit=0
  if grep -q -- "${pattern}" "${out_dir}/${name}.txt"; then hit=1; fi
  local ok=1
  if [ "${want_match}" = "yes" ] && [ "${hit}" = "0" ]; then ok=0; fi
  if [ "${want_match}" = "no" ] && [ "${hit}" = "1" ]; then ok=0; fi
  if [ "${ok}" = "1" ]; then
    printf '      ok   %s\n' "${desc}"
  else
    printf '      FAIL %s (%s.txt)\n' "${desc}" "${name}"
    if [ "${label}" = "after" ]; then failures=$((failures + 1)); fi
  fi
  return 0
}

# ── scene 1: the alignment pattern ──────────────────────────────────────
echo "  scene: alignment-pattern"
start ev117-align "$(cat <<EOF
printf '${probe}\n'
printf 'this pane is about to run the screen-alignment test\n'
printf '\033#8'
EOF
)"
shoot "alignment-pattern"
expect "alignment-pattern" yes "${full_row}" "the screen is filled with E"
expect "alignment-pattern" no "TRUECOLOR" "the fill covered the earlier output"
finish

# ── scene 2: what a conformance suite sees ──────────────────────────────
echo "  scene: conformance-run"
start ev117-conf "$(cat <<EOF
printf '${probe}\n'
printf '\033#8'
printf '\033[1;1H\033[7m+\033[0m'
printf '\033[1;${cols}H\033[7m+\033[0m'
printf '\033[${rows};1H\033[7m+\033[0m'
printf '\033[${rows};${cols}H\033[7m+\033[0m'
printf '\033[$((rows / 2));$(((cols - 18) / 2))H\033[1;38;2;255;210;90m SCREEN ALIGNMENT \033[0m'
EOF
)"
shoot "conformance-run"
expect "conformance-run" yes "EEEE" "the alignment backdrop is present"
expect "conformance-run" yes "SCREEN ALIGNMENT" "the suite's own marks drew over it"
finish

# ── scene 3: scroll region, pen, and the homed cursor ───────────────────
echo "  scene: scroll-region"
start ev117-region "$(cat <<EOF
printf '${probe}\n'
printf '\033[4;9r'
printf '\033[1;38;2;255;80;80;48;5;27m'
printf '\033#8'
printf 'HOME'
EOF
)"
shoot "scroll-region"
expect "scroll-region" yes "^HOMEEEEE" "the cursor was homed, onto the pattern"
expect "scroll-region" yes "${full_row}" "rows outside the scroll region were filled too"
finish

# ── scene 4: the recycled alternate screen ──────────────────────────────
echo "  scene: alt-recycle"
start ev117-alt "$(cat <<EOF
printf '${probe}\n'
printf 'PRIMARY SCREEN\n'
printf '\033[?1049h\033#8'
printf '\033[?1049l'
printf '\033[?1049h'
printf '\033[3;3HSECOND APPLICATION STARTS HERE'
EOF
)"
shoot "alt-recycle"
expect "alt-recycle" yes "SECOND APPLICATION" "the second application drew"
expect "alt-recycle" no "EEEE" "no pattern leaked into the recycled buffer"
finish

# ── scene 5: a rich TUI over the pattern ────────────────────────────────
echo "  scene: richtui-vim"
if command -v vim >/dev/null 2>&1; then
  vim_file="${runtime}/alignment.txt"
  printf 'DECALN fills the screen with E.\nvim must repaint over it.\n' >"${vim_file}"
  # The pattern is drawn, the done-file is touched, and only then does vim
  # take the pane over -- so the wait below is on vim's own output, not on a
  # pane that has not started yet.
  start ev117-vim "$(cat <<EOF
printf '${probe}\n'
printf '\033#8'
EOF
)" "vim -u NONE -N '${vim_file}'"
  sx pane wait-for -s "${session_of}" -p "${pane_of}" \
    -t "DECALN fills" --timeout-ms 20000 >/dev/null
  sx pane wait-settled "${pane_of}" --quiet 600 --timeout 15000 >/dev/null
  shoot "richtui-vim"
  expect "richtui-vim" yes "DECALN fills" "vim repainted the pane"
  expect "richtui-vim" no "${full_row}" "no row of the pattern survived vim's repaint"
  finish
else
  echo "    (vim not installed -- scene skipped)"
fi

echo "==> ${label} evidence in ${out_dir}"
if [ "${failures}" -gt 0 ]; then
  echo "==> ${failures} assertion(s) FAILED" >&2
  exit 1
fi
