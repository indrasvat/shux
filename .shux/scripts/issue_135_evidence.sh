#!/usr/bin/env bash
# Visual evidence for issue #135 — `pane list` named no pane, and joined argv
# with a bare space so a quoted argument was indistinguishable from several.
#
# Runs against whichever binary SHUX_BIN points at, so the same scenes can be
# shot on both sides of the fix and compared:
#
#   SHUX_BIN=<base binary>  LABEL=before EXPECT_DEFECT=1 .shux/scripts/issue_135_evidence.sh
#   SHUX_BIN=<fixed binary> LABEL=after                  .shux/scripts/issue_135_evidence.sh
#
# Every scene drives the real CLI surface under test through a real daemon, real
# PTYs and a real shell, and shoots the result through shux's own rasterizer.
#
# WHY THE LIST IS RUN INSIDE A PANE. `--format text` is the human format, and
# `TerminalContext::detect` downgrades it to `plain` the moment stdout is not a
# TTY — so `shux pane list --format text | tee` does NOT render the box, and
# every screenshot taken that way would be of the wrong code path. Each scene
# therefore types the command into a real pane at a known size and photographs
# that pane. That is also the only way the terminal-width budget is exercised at
# all, because the width comes from the TTY.
#
#   titles       the first half of the issue: two panes, one with a manual title
#                and one auto-titled, listed in the human format. Pre-fix the
#                box has a single ID column and names neither.
#   quoting      the second half: one argv with a space-bearing argument beside
#                one with the same words as separate arguments. Pre-fix the two
#                render identically, which is the whole complaint.
#   shell-cmd    the issue's own reproduction — a `--cmd` pane, which since #125
#                is `["<shell>", "-c", "<script>"]` and is where the ambiguity
#                actually bites.
#   narrow       the same list in a 60-column pane. The columns this task adds
#                are the first wide ones the box has ever had, so the frame has
#                to hold; pre-fix there is nothing to truncate and the scene is
#                a control.
#   plain        the script-facing arm, as text: four tab-separated fields, and
#                the third one fed back to a real shell to prove the argument
#                boundaries are recoverable rather than merely present.
#   run-args     `pane.run_command`'s `args`, the sixth spawner. Pre-fix a `;`
#                in an argument ran a second command and an empty argument
#                vanished.
#
# COLOUR PROBE. Every pane emits truecolor + 256-indexed + basic ANSI before the
# scene runs, and the assertions read the PEN via `pane glance --cells`, not the
# word — the words TRUECOLOR/INDEXED/BASIC are ordinary text and a `grep` for
# them would pass on a monochrome screen.
#
# ASSERTIONS. Every scene asserts, and a failed assertion fails the script.
# Shooting the pre-fix baseline, where the assertions are meant to fail, is an
# explicit opt-in: EXPECT_DEFECT=1 inverts the verdict, so that arm fails if the
# defect does NOT reproduce.
#
# Output: .shux/out/issue-135/<label>/*.png (+ .txt, .json). Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
# shellcheck source=lib/shux_harness.sh disable=SC1091
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
label="${LABEL:-after}"
out_dir="${repo_root}/.shux/out/issue-135/${label}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-135-${label}.XXXXXX")"
work="${runtime}/work"
mkdir -p "${work}" "${out_dir}"

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

# SHELL is pinned: a `--cmd` pane is wrapped in `$SHELL -c`, and the evidence
# has to name which shell rather than inherit the operator's.
sx() {
  env -u SHUX_SOCKET \
    XDG_RUNTIME_DIR="${runtime}" \
    PATH="$(cd "$(dirname "${shux_bin}")" && pwd):${PATH}" \
    SHELL=/bin/bash \
    TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 \
    "${shux_bin}" "$@"
}

echo "==> ${label}: $(${shux_bin} --version 2>/dev/null | head -1)"

probe_cmd=$'printf \'\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n\''

viewer_pane=""
viewer_session=""
target_window=""

# The id of a session's FIRST window.
#
# Deliberately not `-w 1`. `window list` shows the default window at index 0
# with the name "1", and `resolve_window_id` tries the index before the name —
# so `-w 1` names the SECOND window, which in these scenes is the viewer, and
# every screenshot would have been of the wrong window. (Pre-existing and
# unrelated to this issue; filed rather than changed here.) An id is unambiguous.
first_window_id() {
  sx --format json window list -s "$1" \
    | python3 -c 'import json,sys;print(json.load(sys.stdin)[0]["id"])'
}

# open_viewer <session> <cols> — a window whose pane is a live shell at a known
# size, colour-probed, ready to be typed into.
open_viewer() {
  local session="$1" cols="$2"
  sx window create -s "${session}" -n viewer >/dev/null
  viewer_session="${session}"
  viewer_pane="$(sx --format json pane list -s "${session}" -w viewer | python3 -c 'import json,sys;print(json.load(sys.stdin)[0]["id"])')"
  sx pane set-size -s "${session}" -w viewer -p "${viewer_pane}" --cols "${cols}" --rows 24 >/dev/null
  type_line "${probe_cmd}"
  sx pane wait-for -s "${session}" -p "${viewer_pane}" -t TRUECOLOR --timeout-ms 25000 >/dev/null || {
    echo "FATAL: viewer pane never printed anything" >&2
    exit 1
  }
}

# type_line <line> — send-keys is byte-verbatim, so the newline travels as
# base64 rather than as a shell escape the CLI would have to interpret.
type_line() {
  local data
  data="$(printf '%s\n' "$1" | base64 -w0)"
  sx pane send-keys -s "${viewer_session}" -w viewer -p "${viewer_pane}" --data "${data}" >/dev/null
}

# shoot <name> <needle> — run the list in the viewer, wait for real content
# (never `wait-settled` alone: a not-yet-started command is quiet), settle, then
# write the PNG, the text and the cells.
shoot() {
  local name="$1" needle="$2"
  sx pane wait-for -s "${viewer_session}" -p "${viewer_pane}" -t "${needle}" --timeout-ms 25000 >/dev/null || {
    echo "FATAL: ${name}: ${needle} never appeared" >&2
    sx pane capture -s "${viewer_session}" -p "${viewer_pane}" >&2 || true
    exit 1
  }
  sx pane wait-settled "${viewer_pane}" --quiet 400 --timeout 15000 >/dev/null
  sx pane snapshot -s "${viewer_session}" -p "${viewer_pane}" -o "${out_dir}/${name}.png" >/dev/null
  sx pane capture -s "${viewer_session}" -p "${viewer_pane}" >"${out_dir}/${name}.txt"
  sx --format json pane glance "${viewer_pane}" --cells >"${out_dir}/${name}.cells.json"
  # The RPC's own answer, which is IDENTICAL on both binaries — this task
  # changed only how it is rendered. `expect_always` reads this, never the
  # screen: a control that asserts something the pre-fix screen cannot show is
  # not a control, it is a second copy of the thing under test.
  sx --format json pane list -s "${viewer_session}" -w "${target_window}" \
    >"${out_dir}/${name}.panes.txt"
  printf '    %-14s %8s bytes png, %3s lines text\n' \
    "${name}" "$(wc -c <"${out_dir}/${name}.png")" "$(wc -l <"${out_dir}/${name}.txt")"
}

finish() {
  local s kept=()
  sx session kill "${viewer_session}" >/dev/null 2>&1 || true
  for s in "${sessions[@]}"; do [ "${s}" = "${viewer_session}" ] || kept+=("${s}"); done
  sessions=("${kept[@]:-}")
  viewer_session=""
  viewer_pane=""
}

_check() { # _check <scene> <yes|no> <pattern> <desc> <invert 0|1>
  local file="${out_dir}/$1.txt" want="$2" pattern="$3" desc="$4" invert="$5"
  local hit=0 ok=1
  # A missing artifact is a HARNESS failure, not a passing "no" check. Without
  # this, a typo in a filename makes every `expect ... no ...` pass on nothing —
  # which is exactly what happened the first time this script ran.
  if [ ! -f "${file}" ]; then
    printf '      FAIL %s — no artifact at %s\n' "${desc}" "${file}"
    failures=$((failures + 1))
    return 0
  fi
  if grep -qF -- "${pattern}" "${file}"; then hit=1; fi
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

# assert_pen <name> <run-text> <style-fragment> — is there a run reading exactly
# <run-text> whose style JSON contains <style-fragment>? Reads the PEN, so a
# screen that merely contains the word cannot pass.
assert_pen() {
  local name="$1" text="$2" want="$3"
  local got
  got="$(python3 - "${out_dir}/${name}.cells.json" "${text}" "${want}" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
frame = doc.get("result", doc).get("cells", doc.get("cells"))
text, want = sys.argv[2], sys.argv[3]
for row in frame["rows"]:
    for run in row["runs"]:
        run_text = run[1]
        style = run[2] if len(run) > 2 else {}
        if text in run_text and want in json.dumps(style, sort_keys=True):
            print("1")
            sys.exit()
print("0")
PY
)"
  if [ "${got}" = "1" ]; then
    printf '      ok   %s carries pen %s\n' "${text}" "${want}"
    passes=$((passes + 1))
  else
    printf '      FAIL %s does not carry pen %s (%s)\n' "${text}" "${want}" "${name}.cells.json"
    failures=$((failures + 1))
  fi
}

# ── scene: titles ───────────────────────────────────────────────────────
echo "  -- titles"
sx session create s-titles -d --cwd "${work}" >/dev/null
sessions+=(s-titles)
target_window="$(first_window_id s-titles)"
sx pane split -s s-titles -w "${target_window}" -- sleep 900 >/dev/null
first_pane="$(sx --format json pane list -s s-titles -w "${target_window}" | python3 -c 'import json,sys;print(json.load(sys.stdin)[0]["id"])')"
sx pane title -s s-titles -w "${target_window}" -p "${first_pane}" -t deploy-log >/dev/null
open_viewer s-titles 100
type_line "shux pane list -s s-titles -w ${target_window} --format text"
shoot titles "Panes"
expect titles yes "TITLE" "the text list has a TITLE column"
expect titles yes "deploy-log" "the manually-titled pane is named"
expect titles yes "COMMAND" "the text list has a COMMAND column"
expect_always titles.panes yes "deploy-log" "the RPC carries the title on both binaries"
expect_always titles yes "Panes" "the box header is drawn on both binaries"
assert_pen titles INDEXED 208
assert_pen titles BASIC '"fg"'
finish

# ── scene: quoting ──────────────────────────────────────────────────────
echo "  -- quoting"
sx session create s-quote -d --cwd "${work}" -- /bin/sh -c "sleep 900" "one two three" >/dev/null
sessions+=(s-quote)
target_window="$(first_window_id s-quote)"
sx pane split -s s-quote -w "${target_window}" -- /bin/sh -c "sleep 900" one two three >/dev/null
open_viewer s-quote 120
type_line "shux pane list -s s-quote -w ${target_window} --format text"
shoot quoting "Panes"
expect quoting yes "'one two three'" "one argument with spaces is quoted"
expect_always quoting.panes yes "/bin/sh" "both panes really run /bin/sh (from the RPC, not the screen)"
assert_pen quoting INDEXED 208
finish

# ── scene: shell-cmd (the issue's own reproduction) ─────────────────────
echo "  -- shell-cmd"
sx session create s-cmd -d --cwd "${work}" --cmd "printf 'hi\n'; exec sleep 900" >/dev/null
sessions+=(s-cmd)
target_window="$(first_window_id s-cmd)"
open_viewer s-cmd 120
type_line "shux pane list -s s-cmd -w ${target_window} --format text"
shoot shell-cmd "Panes"
expect shell-cmd yes "-c 'printf" "the shell script is shown as one argument"
expect_always shell-cmd.panes yes "/bin/bash" "the pane is really shell-wrapped on both binaries (from the RPC)"
assert_pen shell-cmd INDEXED 208
finish

# ── scene: narrow ───────────────────────────────────────────────────────
echo "  -- narrow"
sx session create s-narrow -d --cwd "${work}" --cmd "printf 'hi\n'; exec sleep 900" >/dev/null
sessions+=(s-narrow)
target_window="$(first_window_id s-narrow)"
open_viewer s-narrow 60
type_line "shux pane list -s s-narrow -w ${target_window} --format text"
shoot narrow "Panes"
# The frame is the assertion: every box line the same width, none over 60.
narrow_ok="$(python3 - "${out_dir}/narrow.txt" <<'PY'
import sys, unicodedata
def w(s):
    return sum(2 if unicodedata.east_asian_width(c) in "WF" else (0 if unicodedata.combining(c) else 1) for c in s)
lines = [l.rstrip("\n") for l in open(sys.argv[1])]
box = [l for l in lines if l and l[0] in "╭│╰"]
widths = {w(l) for l in box}
print("1" if box and len(widths) == 1 and max(widths) <= 60 else f"0 {sorted(widths)}")
PY
)"
if [ "${narrow_ok}" = "1" ]; then
  printf '      ok   the 60-column frame is square and fits\n'
  passes=$((passes + 1))
else
  printf '      FAIL the 60-column frame is %s\n' "${narrow_ok}"
  failures=$((failures + 1))
fi
assert_pen narrow INDEXED 208
finish

# ── scene: plain (script-facing) ────────────────────────────────────────
echo "  -- plain"
sx session create s-plain -d --cwd "${work}" -- /bin/sh -c "sleep 900" "one two three" >/dev/null
sessions+=(s-plain)
target_window="$(first_window_id s-plain)"
sx --format plain pane list -s s-plain -w "${target_window}" >"${out_dir}/plain.txt"
sx --format json pane list -s s-plain -w "${target_window}" >"${out_dir}/plain.panes.json"
fields="$(awk -F'\t' 'NR==1{print NF}' "${out_dir}/plain.txt")"
if [ "${expect_defect}" = "1" ]; then want_fields=3; else want_fields=4; fi
if [ "${fields}" = "${want_fields}" ]; then
  printf '      ok   plain arm has %s tab-separated fields\n' "${fields}"
  passes=$((passes + 1))
else
  printf '      FAIL plain arm has %s fields, expected %s\n' "${fields}" "${want_fields}"
  failures=$((failures + 1))
fi
# The claim the quoting makes: the printed line re-splits into the argv it came
# from. Asked of a real shell, not of a second implementation of one.
roundtrip="$(python3 - "${out_dir}/plain.txt" "${out_dir}/plain.panes.json" <<'PY'
import json, subprocess, sys
row = open(sys.argv[1]).read().splitlines()[0].split("\t")
argv = json.load(open(sys.argv[2]))[0]["command"]
out = subprocess.run(["/bin/sh", "-c", f'for a in {row[2]}; do printf "%s\\0" "$a"; done'],
                     capture_output=True)
words = out.stdout.decode().split("\0")[:-1]
print("1" if words == argv else f"0 {words!r} != {argv!r}")
PY
)"
if [ "${expect_defect}" = "1" ]; then
  if [ "${roundtrip}" = "1" ]; then
    printf '      FAIL the pre-fix plain arm round-tripped, so the defect is gone\n'
    failures=$((failures + 1))
  else
    printf '      ok   the pre-fix plain arm does not round-trip (%s)\n' "${roundtrip:0:60}"
    passes=$((passes + 1))
  fi
elif [ "${roundtrip}" = "1" ]; then
  printf '      ok   the plain COMMAND field re-splits into the argv it came from\n'
  passes=$((passes + 1))
else
  printf '      FAIL round trip: %s\n' "${roundtrip}"
  failures=$((failures + 1))
fi
sx session kill s-plain >/dev/null 2>&1 || true
sessions=("${sessions[@]/s-plain/}")

# ── scene: run-args (the sixth spawner) ─────────────────────────────────
echo "  -- run-args"
sx session create s-run -d --cwd "${work}" >/dev/null
sessions+=(s-run)
run_pane="$(sx --format json pane list -s s-run | python3 -c 'import json,sys;print(json.load(sys.stdin)[0]["id"])')"
sx pane send-keys -s s-run -p "${run_pane}" --data "$(printf '%s\n' "${probe_cmd}" | base64 -w0)" >/dev/null
sx pane wait-for -s s-run -p "${run_pane}" -t TRUECOLOR --timeout-ms 25000 >/dev/null
# The argument carries a `;` and a command that leaves a FILE behind. A grep of
# the screen is the wrong assertion here: the shell echoes the line it was
# given, so the injected text is on screen either way, and the injected command
# can fail for its own reasons before printing anything (`A;id` pre-fix ran
# `id B`, which errored on its argument and never printed `uid=`, so a
# `grep uid=` called the injection a pass). The file either exists or it does not.
pwned="${work}/PWNED-135"
rm -f "${pwned}"
sx rpc call pane.run_command --params \
  "$(python3 -c 'import json,sys;print(json.dumps({"pane_id":sys.argv[1],"command":"printf","args":["[%s]","A;>"+sys.argv[2],"","B"],"timeout":10}))' "${run_pane}" "${pwned}")" \
  >/dev/null 2>&1 || true
sleep 3
sx pane capture -s s-run -p "${run_pane}" >"${out_dir}/run-args.txt"
sx pane snapshot -s s-run -p "${run_pane}" -o "${out_dir}/run-args.png" >/dev/null
sx --format json pane glance "${run_pane}" --cells >"${out_dir}/run-args.cells.json"
expect run-args yes "[A;>${pwned}][][B]" "every argument arrives whole, including the empty one"
if [ -e "${pwned}" ]; then injected=1; else injected=0; fi
if [ "${expect_defect}" = "1" ]; then want_injected=1; else want_injected=0; fi
if [ "${injected}" = "${want_injected}" ]; then
  printf '      ok   argument injection: file created=%s (expected %s)\n' "${injected}" "${want_injected}"
  passes=$((passes + 1))
else
  printf '      FAIL argument injection: file created=%s, expected %s\n' "${injected}" "${want_injected}"
  failures=$((failures + 1))
fi
assert_pen run-args INDEXED 208
finish

# ── verdict ─────────────────────────────────────────────────────────────
echo
echo "  ${passes} passed, ${failures} failed  (label=${label}, expect_defect=${expect_defect})"
echo "  artifacts: ${out_dir}"
if [ "${failures}" -ne 0 ]; then
  echo "VERDICT: FAIL"
  exit 1
fi
echo "VERDICT: PASS"
