#!/usr/bin/env bash
# Visual evidence for issue #125 — `--cmd` was documented as a shell command
# and delivered a whitespace split.
#
# Runs against whichever binary SHUX_BIN points at, so the same scenes can be
# shot on both sides of the fix and compared:
#
#   SHUX_BIN=<base binary>  LABEL=before EXPECT_DEFECT=1 .shux/scripts/issue_125_evidence.sh
#   SHUX_BIN=<fixed binary> LABEL=after                  .shux/scripts/issue_125_evidence.sh
#
# Every scene drives the real CLI surface under test — `shux session create
# --cmd` / `shux window create --cmd` — through a real daemon, a real PTY and a
# real shell, and shoots the pane through shux's own rasterizer.
#
#   semicolon    the issue's own reproduction. `printf 'X\n'; sleep 300` is two
#                commands. Split on whitespace it is one printf with three
#                arguments, and printf says so on screen.
#   quoting      `echo 'hello world'` — the quotes belong to the shell, not to
#                echo's argument.
#   pipeline     `echo … | tr a-z A-Z` — a pipe only exists if a shell reads it.
#   redirect     `echo … > f; cat f` — two commands and a file between them.
#   window-cmd   the same command through `shux window create --cmd`, which
#                additionally has to RECORD what it ran: pre-fix the pane's
#                command column was blank.
#   titles       a composed three-pane window, shot with borders and titles.
#                One pane per ingress: `--cmd`, a string `command` on
#                `pane.split` (silently ignored pre-fix — that pane came up as a
#                bare login shell) and the `-- sh -c` escape hatch (titled `sh`
#                pre-fix, `cat` now). Its content is a file of colour bytes, so
#                the panes stay alive and the probe is in the picture.
#   errors       what a caller sees when the command cannot work. Pre-fix all
#                three of these succeeded — two silently ran the default shell
#                and one left a session whose pane never spawned.
#   argv         the escape hatch `-- sh -c '…'`. Unchanged by the fix, shot on
#                both binaries, and the control that proves the harness is not
#                simply reporting whatever it sees.
#
# NO RESIZE, DELIBERATELY. On the pre-fix binary the pane's command exits
# immediately — that is half the defect — so its PTY is gone and `pane set-size`
# fails outright. Both arms therefore shoot at the daemon's default 80x24, which
# also makes the two sides pixel-comparable.
#
# COLOUR PROBE. Every scene emits truecolor + 256-indexed + basic ANSI, and the
# assertions read the PEN, not the text. That distinction is load-bearing here:
# on the pre-fix binary printf's own error message quotes the unconsumed
# arguments back at the screen, so the literal words INDEXED and BASIC appear as
# plain uncoloured text inside the warning. A `grep INDEXED` would have called
# that a colour probe. `pane glance --cells` gives style runs, so the assertion
# is "a run reading INDEXED carries 256-colour 208", which the mangled screen
# cannot satisfy.
#
# ASSERTIONS. Every scene asserts, and a failed assertion fails the script — a
# harness that files a wrong shot as evidence is worse than no harness. Shooting
# the pre-fix baseline, where the assertions are meant to fail, is an explicit
# opt-in: EXPECT_DEFECT=1. In that mode the verdict is inverted, so the baseline
# arm fails if the defect does NOT reproduce.
#
# Output: .shux/out/issue-125/<label>/*.png (+ .txt, .json). Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
# shellcheck source=lib/shux_harness.sh disable=SC1091
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
label="${LABEL:-after}"
out_dir="${repo_root}/.shux/out/issue-125/${label}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-125-${label}.XXXXXX")"
work="${runtime}/work"
mkdir -p "${work}"
export WORKDIR="${work}"

rows=24

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

# SHELL is pinned: the whole point of the string form is that it reaches a
# shell, so the evidence must name which one rather than inherit the operator's.
sx() {
  env -u SHUX_SOCKET \
    XDG_RUNTIME_DIR="${runtime}" \
    PATH="$(cd "$(dirname "${shux_bin}")" && pwd):${PATH}" \
    SHELL=/bin/bash \
    TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 \
    "${shux_bin}" "$@"
}

mkdir -p "${out_dir}"
echo "==> ${label}: $(${shux_bin} --version 2>/dev/null | head -1)"

probe=$'printf \'\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n\''

session_of=""
pane_of=""

# Every scene's shell string opens with the colour probe. `printf` interprets
# `\033` on both binaries, so TRUECOLOR reaches the screen either way and both
# arms have a real thing to wait for — the pre-fix arm just never gets past it.
lead() { printf '%s' "${probe}"; }

wait_for_probe() {
  sx pane wait-for -s "$1" -p "$2" -t TRUECOLOR --timeout-ms 25000 >/dev/null || {
    echo "FATAL: pane never printed anything" >&2
    sx pane capture -s "$1" -p "$2" --lines "${rows}" >&2 || true
    exit 1
  }
}

# start_cmd <session> <shell-command>  — the flag under test, verbatim.
start_cmd() {
  local session="$1" body="$2"
  sx session create "${session}" -d --cwd "${work}" --cmd "${body}" >/dev/null
  sessions+=("${session}")
  pane_of="$(sx --format json pane list -s "${session}" | jq -r '.[0].id')"
  session_of="${session}"
}

# shoot <name> — waits for real content, settles, then writes PNG + text +
# `pane list` JSON. A valid PNG of the right size can be blank, so every
# assertion below reads the text, never the image.
shoot() {
  local name="$1"
  wait_for_probe "${session_of}" "${pane_of}"
  sx pane wait-settled "${pane_of}" --quiet 400 --timeout 15000 >/dev/null
  sx pane snapshot -s "${session_of}" -p "${pane_of}" -o "${out_dir}/${name}.png" >/dev/null
  sx pane capture -s "${session_of}" -p "${pane_of}" --lines "${rows}" >"${out_dir}/${name}.txt"
  sx --format json pane list -s "${session_of}" >"${out_dir}/${name}.panes.json"
  sx --format json pane glance "${pane_of}" --cells >"${out_dir}/${name}.cells.json"
  printf '    %-14s %8s bytes png, %3s lines text\n' \
    "${name}" "$(wc -c <"${out_dir}/${name}.png")" "$(wc -l <"${out_dir}/${name}.txt")"
}

finish() {
  sx session kill "${session_of}" >/dev/null 2>&1 || true
  local kept=() s
  for s in "${sessions[@]}"; do [ "${s}" = "${session_of}" ] || kept+=("${s}"); done
  sessions=("${kept[@]:-}")
  session_of=""
  pane_of=""
}

_check() { # _check <file> <yes|no> <pattern> <desc> <invert 0|1>
  local file="$1" want="$2" pattern="$3" desc="$4" invert="$5"
  local hit=0 ok=1
  if grep -qF -- "${pattern}" "${out_dir}/${file}"; then hit=1; fi
  if [ "${want}" = "yes" ] && [ "${hit}" = "0" ]; then ok=0; fi
  if [ "${want}" = "no" ] && [ "${hit}" = "1" ]; then ok=0; fi
  if [ "${invert}" = "1" ]; then ok=$((1 - ok)); fi
  if [ "${ok}" = "1" ]; then
    printf '      ok   %s\n' "${desc}"
    passes=$((passes + 1))
  else
    printf '      FAIL %s (%s)\n' "${desc}" "${file}"
    failures=$((failures + 1))
  fi
  return 0
}

# Describes the FIXED behaviour; inverted under EXPECT_DEFECT=1.
expect() { _check "$1" "$2" "$3" "$4" "${expect_defect}"; }
# Holds on BOTH binaries. Never inverted — this is what keeps the harness honest.
expect_always() { _check "$1" "$2" "$3" "$4" 0; }

# _pen <name> <run-text> <style-fragment> — is there a run reading exactly
# <run-text> whose style JSON contains <style-fragment>?
_pen() {
  python3 - "${out_dir}/$1.cells.json" "$2" "$3" <<'PY'
import json, sys

# A run is [col, text] when it carries the default pen and [col, text, style]
# when it does not — so an unconditional 3-way unpack raises on exactly the
# uncoloured rows this check exists to reject.
doc = json.load(open(sys.argv[1]))
frame = doc.get("result", doc).get("cells", doc.get("cells"))
text, want = sys.argv[2], sys.argv[3]
for row in frame["rows"]:
    for run in row["runs"]:
        run_text = run[1]
        style = run[2] if len(run) > 2 else {}
        if run_text == text and want in json.dumps(style, sort_keys=True):
            print("1")
            sys.exit()
print("0")
PY
}

_check_pen() { # _check_pen <name> <text> <fragment> <desc> <invert>
  local ok=1
  [ "$(_pen "$1" "$2" "$3")" = "1" ] || ok=0
  if [ "$5" = "1" ]; then ok=$((1 - ok)); fi
  if [ "${ok}" = "1" ]; then
    printf '      ok   %s\n' "$4"
    passes=$((passes + 1))
  else
    printf '      FAIL %s (%s.cells.json)\n' "$4" "$1"
    failures=$((failures + 1))
  fi
  return 0
}

expect_pen() { _check_pen "$1" "$2" "$3" "$4" "${expect_defect}"; }
expect_pen_always() { _check_pen "$1" "$2" "$3" "$4" 0; }

# ── scene 1: the issue's own reproduction ───────────────────────────────
echo "  scene: semicolon"
start_cmd ev125-semicolon "$(lead); printf 'X\\n'; exec sleep 900"
shoot semicolon
expect_always semicolon.txt yes "TRUECOLOR" "the pane printed something"
expect semicolon.txt no "ignoring excess arguments" "printf was not handed the rest of the line"
expect_pen_always semicolon "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
expect_pen semicolon "INDEXED" '"idx": 208' "256-indexed pen reached the grid"
expect_pen semicolon "BASIC" '"idx": 4' "basic-ANSI pen reached the grid"
finish

# ── scene 2: quoting ────────────────────────────────────────────────────
#
# The one scene that puts its payload FIRST. Everywhere else the leading colour
# probe is the only thing the pre-fix binary ever runs, which is the defect —
# but it also means `echo` never executes, and the quotes-reach-echo symptom the
# issue reports needs `echo` to be argv[0]. So the probe trails here, and on the
# pre-fix binary it is echoed back as literal text with no pen at all: the
# TRUECOLOR pen assertion is a fix assertion in this scene, not an invariant.
echo "  scene: quoting"
start_cmd ev125-quoting "echo 'hello world'; $(lead); exec sleep 900"
shoot quoting
expect_always quoting.txt yes "hello world" "the argument reached echo"
expect quoting.txt no "'hello world'" "the quotes were consumed by the shell"
expect_pen quoting "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
expect_pen quoting "INDEXED" '"idx": 208' "256-indexed pen reached the grid"
expect_pen quoting "BASIC" '"idx": 4' "basic-ANSI pen reached the grid"
finish

# ── scene 3: a pipeline ─────────────────────────────────────────────────
echo "  scene: pipeline"
start_cmd ev125-pipeline "$(lead); echo shux | tr a-z A-Z; exec sleep 900"
shoot pipeline
expect_always pipeline.txt yes "TRUECOLOR" "the pane printed something"
expect pipeline.txt yes "SHUX" "the pipe connected two programs"
expect_pen_always pipeline "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
expect_pen pipeline "INDEXED" '"idx": 208' "256-indexed pen reached the grid"
expect_pen pipeline "BASIC" '"idx": 4' "basic-ANSI pen reached the grid"
finish

# ── scene 4: redirection between two commands ───────────────────────────
echo "  scene: redirect"
start_cmd ev125-redirect "$(lead); echo PERSISTED > note.txt; cat note.txt; exec sleep 900"
shoot redirect
expect_always redirect.txt yes "TRUECOLOR" "the pane printed something"
expect redirect.txt yes "PERSISTED" "the file was written and read back"
expect_pen_always redirect "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
expect_pen redirect "INDEXED" '"idx": 208' "256-indexed pen reached the grid"
expect_pen redirect "BASIC" '"idx": 4' "basic-ANSI pen reached the grid"
finish

# ── scene 5: the same thing through `window create --cmd` ───────────────
#
# This verb wrapped in `sh -c` client-side, so its OUTPUT was already right —
# what it never did was record the command, so `pane list` showed a blank.
echo "  scene: window-cmd"
sx session create ev125-window -d --cwd "${work}" >/dev/null
sessions+=("ev125-window")
sx window create -s ev125-window -n w1 \
  --cmd "$(lead); echo 'window works'; exec sleep 900" >/dev/null
pane_of="$(sx --format json pane list -s ev125-window -w w1 | jq -r '.[0].id')"
session_of="ev125-window"
shoot window-cmd
sx --format json pane list -s ev125-window -w w1 >"${out_dir}/window-cmd.panes.json"
expect_always window-cmd.txt yes "window works" "the command ran on both binaries"
expect_pen_always window-cmd "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
expect_pen_always window-cmd "INDEXED" '"idx": 208' "256-indexed pen reached the grid"
expect_pen_always window-cmd "BASIC" '"idx": 4' "basic-ANSI pen reached the grid"
expect window-cmd.panes.json yes '"-c"' "window create recorded the argv it ran"
finish

# ── scene 6: titles and the composed window ─────────────────────────────
#
# Three ways to give a pane a command, in one picture:
#   left   `--cmd "tail -f colour.txt"`  — identical on both binaries (no shell
#          syntax to lose), so it anchors the comparison and carries the probe.
#   right  `pane.split {"command": "cat …"}` — a STRING. Pre-fix this RPC
#          matched arrays only, so the string was dropped and the pane came up
#          as a bare login shell. The title says which.
#   bottom `-- sh -c "cat …"` — the documented escape hatch. Pre-fix it titled
#          the pane after the shell; the wrapper is unwrapped now.
echo "  scene: titles"
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n' >"${work}/colour.txt"
printf 'a file of colour bytes\n' >>"${work}/colour.txt"

sx session create ev125-titles -d --cwd "${work}" --cmd "tail -f colour.txt" >/dev/null
sessions+=("ev125-titles")
session_of="ev125-titles"
pane_of="$(sx --format json pane list -s ev125-titles | jq -r '.[0].id')"
wait_for_probe ev125-titles "${pane_of}"

sx rpc call pane.split --params "$(jq -nc --arg p "${pane_of}" \
  '{pane_id:$p, direction:"vertical", cwd:$ENV.WORKDIR, command:"cat colour.txt; exec sleep 900"}')" \
  >/dev/null 2>&1 || true
sx rpc call pane.split --params "$(jq -nc --arg p "${pane_of}" \
  '{pane_id:$p, direction:"horizontal", cwd:$ENV.WORKDIR, command:["sh","-c","cat colour.txt; exec sleep 900"]}')" \
  >/dev/null 2>&1 || true
sleep 2
sx pane wait-settled "${pane_of}" --quiet 500 --timeout 15000 >/dev/null || true

sx window snapshot -s ev125-titles -w 0 --cols 96 --rows 24 -o "${out_dir}/titles.png" >/dev/null
sx pane capture -s ev125-titles -p "${pane_of}" --lines "${rows}" >"${out_dir}/titles.txt"
sx --format json pane list -s ev125-titles -w 0 >"${out_dir}/titles.panes.json"
sx --format json pane glance "${pane_of}" --cells >"${out_dir}/titles.cells.json"
printf '    %-14s %8s bytes png, %s panes\n' titles \
  "$(wc -c <"${out_dir}/titles.png")" "$(jq length <"${out_dir}/titles.panes.json")"

expect_always titles.panes.json yes '"title": "tail"' "--cmd pane is titled after the program on both binaries"
expect_pen_always titles "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
expect_pen_always titles "INDEXED" '"idx": 208' "256-indexed pen reached the grid"
expect_pen_always titles "BASIC" '"idx": 4' "basic-ANSI pen reached the grid"
expect titles.panes.json yes '"title": "cat"' "the split panes are titled after the program, not the shell"
finish

# ── scene 7: what a rejection looks like ────────────────────────────────
#
# Shot from inside a pane so the picture is the operator's own terminal, not a
# transcript. Each line is a `command` that cannot do what it says; pre-fix each
# one was accepted.
echo "  scene: errors"
sx session create ev125-errors -d --cwd "${work}" --cmd "$(lead); \
  shux rpc call session.create --params '{\"name\":\"a\",\"command\":42}' 2>&1 | jq -r '.error.data.detail // .' | fold -w 78; \
  shux rpc call session.create --params '{\"name\":\"b\",\"command\":[\"vim\",null]}' 2>&1 | jq -r '.error.data.detail // .' | fold -w 78; \
  shux session create c -d -- no-such-binary-xyz 2>&1 | head -1; \
  exec sleep 900" >/dev/null
sessions+=("ev125-errors")
session_of="ev125-errors"
pane_of="$(sx --format json pane list -s ev125-errors | jq -r '.[0].id')"
shoot errors
expect errors.txt yes "must be a string" "a number is refused, with a reason"
expect errors.txt yes "command[1]" "the offending array element is named"
expect errors.txt yes "spawn" "a program that cannot start is refused"
expect_pen_always errors "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
finish

# ── scene 8: the argv escape hatch (control) ────────────────────────────
#
# Unchanged by the fix. It shoots identically on both binaries, which is what
# makes it a control: a harness that reported a difference here would be
# measuring itself.
echo "  scene: argv (control)"
sx session create ev125-argv -d --cwd "${work}" -- \
  sh -c "$(lead); echo 'argv works'; exec sleep 900" >/dev/null
sessions+=("ev125-argv")
pane_of="$(sx --format json pane list -s ev125-argv | jq -r '.[0].id')"
session_of="ev125-argv"
shoot argv
expect_always argv.txt yes "argv works" "trailing argv is unchanged on both binaries"
expect_always argv.txt no "'argv works'" "trailing argv still reaches a real shell"
expect_pen_always argv "TRUECOLOR" '"rgb": [120, 220, 180]' "truecolor pen reached the grid"
expect_pen_always argv "INDEXED" '"idx": 208' "256-indexed pen reached the grid"
expect_pen_always argv "BASIC" '"idx": 4' "basic-ANSI pen reached the grid"
finish

# ── verdict ─────────────────────────────────────────────────────────────
echo
if [ "${expect_defect}" = "1" ]; then
  echo "  (EXPECT_DEFECT=1 — fix assertions inverted; a pass means the defect reproduced)"
fi
printf '  %s: %d passed, %d failed\n' "${label}" "${passes}" "${failures}"
if [ "${failures}" -gt 0 ]; then
  echo "VERDICT: FAIL"
  exit 1
fi
echo "VERDICT: PASS"
