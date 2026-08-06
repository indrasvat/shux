#!/usr/bin/env bash
# Visual evidence for issue #122 — REP (`CSI b`) sourced from the screen.
#
# Runs against whichever binary `SHUX_BIN` points at, so the same scenes can be
# shot before and after the fix and compared:
#
#   SHUX_BIN=<base binary>  LABEL=before EXPECT_DEFECT=1 .shux/scripts/issue_122_evidence.sh
#   SHUX_BIN=<fixed binary> LABEL=after                  .shux/scripts/issue_122_evidence.sh
#
# Scenes (each shot through shux's own rasterizer, from a real pane running a
# real shell over a real PTY). Every one of them is a thing an application
# actually does, and every one of them starts with a cursor move — which is what
# made the repeat vanish:
#
#   rule            a horizontal rule: address the line, print one character,
#                   repeat it across the width.
#   progress-bar    a bar redrawn in place over three frames. Each frame homes
#                   to its line, erases it, prints one block and repeats it.
#   box             a box drawn with the DEC line-drawing set. The horizontal
#                   edges are REP; the remembered character has to be the
#                   translated line glyph, not the ASCII `q` that carried it.
#   column-zero     the issue's own reproduction, at the position where the
#                   old source had nothing at all to read.
#   pen             the repeats take the pen current at the `CSI b`, not the
#                   colour of the character being repeated. This one is
#                   UNCHANGED by the fix and shoots identically on both
#                   binaries -- it is here because CLAUDE.md requires a colour
#                   probe on every capture, and because a record that stores no
#                   style is the thing that keeps it true.
#
# SYNCHRONISATION. The pane is spawned at the daemon's default geometry and
# resized a moment later, so each scene's script BLOCKS on a go-file the harness
# writes after the resize — otherwise it draws on the wrong screen and its output
# is then reflowed, which is a different picture entirely. It then touches a
# done-file after its last write, and the harness waits for that before it looks
# at the screen: `wait-settled` alone cannot tell "finished" from "not started".
# Both waits are bounded and hard-fail; nothing here swallows a timeout.
#
# ASSERTIONS. Every run asserts, and a failed assertion fails the script -- a
# harness that files a wrong shot as evidence is worse than no harness. Recording
# the pre-fix baseline, where the assertions are supposed to fail, is an explicit
# opt-in: `EXPECT_DEFECT=1`. In that mode the script fails if the defect does
# NOT reproduce, so the baseline arm cannot pass vacuously either.
#
# Output: .shux/out/issue-122/<label>/*.png (+ .txt). Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
label="${LABEL:-after}"
out_dir="${repo_root}/.shux/out/issue-122/${label}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-122-${label}.XXXXXX")"

# Deliberately small panes: the rasterizer draws a fixed cell size, so fewer
# columns means each glyph is a larger share of the frame. These shots are meant
# to be readable at a glance, not to survey a desktop.
cols="${EVID_COLS:-44}"
rows="${EVID_ROWS:-12}"

sessions=()
failures=0
passes=0
expect_defect="${EXPECT_DEFECT:-0}"

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

# A colour probe is EMITTED by every scene, so a monochrome regression cannot
# pass as a clean shot: truecolor, 256-indexed and basic ANSI all present.
probe='\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m'

session_of=""
pane_of=""

# start <session> <script-body>
start() {
  local session="$1" body="$2"
  local script="${runtime}/${session}.sh"
  local done_file="${runtime}/${session}.done"
  local go_file="${runtime}/${session}.go"
  rm -f "${done_file}" "${go_file}"
  {
    printf 'while [ ! -e "%s" ]; do sleep 0.05; done\n' "${go_file}"
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
  : >"${go_file}"

  local deadline=$((SECONDS + 30))
  while [ ! -e "${done_file}" ]; do
    if [ "${SECONDS}" -ge "${deadline}" ]; then
      echo "FATAL: ${session} never finished its script" >&2
      exit 1
    fi
    sleep 0.1
  done
  sx pane wait-settled "${pane}" --quiet 400 --timeout 15000 >/dev/null

  session_of="${session}"
  pane_of="${pane}"
}

shoot() { # shoot <name>
  sx pane snapshot -s "${session_of}" -p "${pane_of}" -o "${out_dir}/$1.png" >/dev/null
  # `pane capture` defaults to --lines 50; ask for the pane's real height so a
  # taller pane is not silently truncated to its last 50 rows.
  sx pane capture -s "${session_of}" -p "${pane_of}" --lines "${rows}" >"${out_dir}/$1.txt"
  # A valid PNG of the right dimensions can still be blank, so the .txt beside
  # it is what the assertions read.
  printf '    %-20s %8s bytes png, %3s lines text\n' \
    "$1" "$(wc -c <"${out_dir}/$1.png")" "$(wc -l <"${out_dir}/$1.txt")"
}

finish() {
  sx session kill "${session_of}" >/dev/null 2>&1 || true
  local kept=() s
  for s in "${sessions[@]}"; do [ "${s}" = "${session_of}" ] || kept+=("${s}"); done
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
    passes=$((passes + 1))
  else
    printf '      FAIL %s (%s.txt)\n' "${desc}" "${name}"
    failures=$((failures + 1))
  fi
  return 0
}

rule_line="$(printf '=%.0s' $(seq 1 $((cols - 1))))"

# shoot_cells <name> -- the canonical cell frame beside the PNG, so a colour
# assertion reads the same frame the picture was rendered from. `pane capture`
# is text only and cannot see a pen at all.
cells_of() {
  sx --format json pane glance "${pane_of}" --cells 2>/dev/null
}

# expect_run <name> <row> <col> <text> <colour-json-fragment> <description>
expect_run() {
  local name="$1" row="$2" col="$3" text="$4" colour="$5" desc="$6"
  local hit
  hit="$(python3 - "${out_dir}/${name}.cells.json" "${row}" "${col}" "${text}" "${colour}" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
frame = doc.get("result", doc).get("cells", doc.get("cells"))
row, col, text, colour = int(sys.argv[2]), int(sys.argv[3]), sys.argv[4], sys.argv[5]
for r in frame["rows"]:
    if r["row"] != row:
        continue
    for start, run_text, style in r["runs"]:
        if start == col and run_text == text and colour in json.dumps(style, sort_keys=True):
            print("1")
            sys.exit()
print("0")
PY
)"
  if [ "${hit}" = "1" ]; then
    printf '      ok   %s\n' "${desc}"
    passes=$((passes + 1))
  else
    printf '      FAIL %s (%s.cells.json)\n' "${desc}" "${name}"
    failures=$((failures + 1))
  fi
  return 0
}

# ── scene 1: a horizontal rule ──────────────────────────────────────────
# Seed the glyph, return to the start of the line, fill. "Draw one, go back,
# fill" is an ordinary way to write a rule routine -- and it puts the `CSI b`
# at column 1, where the screen-derived source had nothing to read.
echo "  scene: rule"
start ev122-rule "$(cat <<EOF
printf '\033[1;1H${probe}'
printf '\033[3;1HRelease notes'
printf '\033[4;1H='
printf '\r\033[$((cols - 1))b'
printf '\033[6;1H  the rule above is one "=" and'
printf '\033[7;1H  a request to repeat it'
EOF
)"
shoot "rule"
expect "rule" yes "${rule_line}" "the rule spans the line"
finish

# ── scene 2: a progress bar redrawn in place ────────────────────────────
# Each frame erases the line, prints one block, homes to the bar's first column
# and repeats. The home is what the old code could not survive.
echo "  scene: progress-bar"
start ev122-bar "$(cat <<'EOF'
printf '\033[1;1H\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m'
printf '\033[3;1HDownloading shux 0.47.0'
for n in 6 14 24 34; do
  printf '\033[5;1H\033[K\033[38;5;208m#'
  printf '\033[5;1H'
  printf '\033[%db' "$n"
  printf '\033[0m'
  printf '\033[7;1H\033[K  %d%% complete' "$(( n * 100 / 34 ))"
done
EOF
)"
shoot "progress-bar"
expect "progress-bar" yes "##################################" "the bar reached full width"
expect "progress-bar" yes "100% complete" "the last frame landed"
finish

# ── scene 3: a box drawn with the line-drawing set ──────────────────────
# The edges are drawn by seeding one line glyph and repeating it from the
# corner column. The remembered character has to be the TRANSLATED glyph, not
# the ASCII `q` that carried it through the character set.
echo "  scene: box"
start ev122-box "$(cat <<EOF
printf '\033[1;1H${probe}'
printf '\033(0'
printf '\033[3;2Hq'
printf '\033[3;2H\033[$((cols - 8))b'
printf '\033[7;2H\033[$((cols - 8))b'
printf '\033[3;1Hl\033[3;$((cols - 6))Hk'
printf '\033[7;1Hm\033[7;$((cols - 6))Hj'
printf '\033[4;1Hx\033[4;$((cols - 6))Hx'
printf '\033[5;1Hx\033[5;$((cols - 6))Hx'
printf '\033[6;1Hx\033[6;$((cols - 6))Hx'
printf '\033(B'
printf '\033[5;4HREP draws the horizontal edges'
EOF
)"
shoot "box"
box_edge="$(printf '─%.0s' $(seq 1 $((cols - 8))))"
expect "box" yes "┌${box_edge}┐" "the top edge is a full run of line glyphs"
expect "box" yes "└${box_edge}┘" "the bottom edge is a full run of line glyphs"
expect "box" no "qqqq" "the ASCII carrier leaked through instead of the line glyph"
finish

# ── scene 4: the issue's own reproduction ───────────────────────────────
echo "  scene: column-zero"
start ev122-colzero "$(cat <<EOF
printf '\033[1;1H${probe}'
printf '\033[3;1Hprintf "X\\\\033[1;1H\\\\033[3b"'
printf '\033[5;1HX'
printf '\033[5;1H\033[3b'
printf '\033[7;1Habove: one X, then "repeat it 3 times"'
EOF
)"
shoot "column-zero"
expect "column-zero" yes "^XXX$" "REP at column 1 wrote its three repeats"
finish

# ── scene 5: the pen belongs to the terminal ────────────────────────────
# No cursor move here, so this scene draws identically on BOTH binaries -- the
# repeats always took the pen in force at the `CSI b`, never the colour of the
# character being repeated. It is a NEGATIVE control: the fix must not disturb
# it, and the "before" and "after" PNGs are byte-identical. Text capture cannot
# see a pen at all, so this one asserts on the canonical cell frame.
echo "  scene: pen"
start ev122-pen "$(cat <<'EOF'
printf '\033[1;1H\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m'
printf '\033[3;1HRed O, then switch pen, then repeat:'
printf '\033[5;1H\033[31mO\033[38;2;120;220;180m\033[20b\033[0m'
printf '\033[7;1HBlue o, then switch pen, then repeat:'
printf '\033[9;1H\033[34mo\033[38;5;208m\033[20b\033[0m'
EOF
)"
shoot "pen"
cells_of >"${out_dir}/pen.cells.json"
expect "pen" yes "OOOOOOOOOOOOOOOOOOOOO" "the first run repeated"
expect_run "pen" 4 0 "O" '"idx": 1' "the original O keeps its own red"
expect_run "pen" 4 1 "OOOOOOOOOOOOOOOOOOOO" '"rgb": [120, 220, 180]' "the repeats take the pen current at the CSI b"
expect_run "pen" 8 1 "oooooooooooooooooooo" '"idx": 208' "the second run's repeats take the current pen too"
finish

# ── verdict ─────────────────────────────────────────────────────────────
echo
echo "  ${passes} passed, ${failures} failed  (${out_dir})"
if [ "${expect_defect}" = "1" ]; then
  if [ "${failures}" -eq 0 ]; then
    echo "FATAL: EXPECT_DEFECT=1 but every assertion passed -- the defect did not reproduce" >&2
    exit 1
  fi
  echo "  baseline recorded: the defect reproduces (${failures} assertions failed, as expected)"
  exit 0
fi
[ "${failures}" -eq 0 ]
