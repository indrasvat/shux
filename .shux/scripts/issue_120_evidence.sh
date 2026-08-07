#!/usr/bin/env bash
# Evidence for issue #120 — the short id every listing prints was accepted
# nowhere.
#
# Runs against whichever binary `SHUX_BIN` points at, so the same round trip
# can be driven before and after the fix and compared:
#
#   SHUX_BIN=<base binary>  LABEL=before EXPECT_DEFECT=1 .shux/scripts/issue_120_evidence.sh
#   SHUX_BIN=<fixed binary> LABEL=after                  .shux/scripts/issue_120_evidence.sh
#
# WHAT IT CHECKS. One thing, from the user's side: take the id `pane list`,
# `session list` and `window list` PRINT, hand it straight back to the command
# that consumes it, and see whether it lands on the same entity the full uuid
# would. Everything else here is scaffolding for that sentence.
#
# ASSERTIONS. Every check asserts, and a failed assertion fails the script — a
# harness that files a wrong result as evidence is worse than no harness.
# Recording the pre-fix baseline, where the assertions are SUPPOSED to fail, is
# an explicit opt-in: `EXPECT_DEFECT=1`. In that mode the script fails if the
# defect does NOT reproduce, so the baseline arm cannot pass vacuously either.
#
# NO MASKED FAILURES. Nothing here is `|| true`. A daemon that will not start,
# a pane that never prints, a command that hangs — all abort loudly.
#
# SYNCHRONISATION. The fixture pane prints a colour probe and only then parks.
# The harness waits for that text before it looks at anything, because a
# not-yet-started pane and a settled one are indistinguishable to a quiet-window
# wait, and a blank screen would let a colour regression pass.
#
# Output: .shux/out/issue-120/<label>/ (gitignored scratch).

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
label="${LABEL:-after}"
expect_defect="${EXPECT_DEFECT:-0}"
out_dir="${repo_root}/.shux/out/issue-120/${label}"
# Short runtime dir: unix socket paths cap at ~108 bytes and a deep scratch
# path silently blows that.
runtime="$(mktemp -d "/tmp/sx120-${label}.XXXXXX")"

mkdir -p "${out_dir}"
log="${out_dir}/round-trip.txt"
: > "${log}"

session="id120-${label}-$$"
pass=0
fail=0

cleanup() {
  local pid_file="${runtime}/shux/shux.pid"
  if [ -S "${runtime}/shux/shux.sock" ] || [ -f "${pid_file}" ]; then
    env XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" session kill "${session}" \
      >/dev/null 2>&1 || true
  fi
  # By PIDFILE only. `pkill -f shux` matches this script's own argv.
  if [ -f "${pid_file}" ]; then
    local pid
    pid="$(head -n 1 "${pid_file}" | tr -cd '0-9')"
    if [ -n "${pid}" ]; then
      kill "${pid}" >/dev/null 2>&1 || true
      for _ in 1 2 3 4 5 6 7 8 9 10; do
        kill -0 "${pid}" >/dev/null 2>&1 || break
        sleep 0.5
      done
      if kill -0 "${pid}" >/dev/null 2>&1; then
        echo "LEAK: daemon ${pid} survived SIGTERM" >&2
        kill -9 "${pid}" >/dev/null 2>&1 || true
        exit 1
      fi
    fi
  fi
  rm -rf "${runtime}"
}
trap cleanup EXIT

sx() { env XDG_RUNTIME_DIR="${runtime}" RUST_BACKTRACE=0 "${shux_bin}" "$@"; }

say() { printf '%s\n' "$*" | tee -a "${log}"; }

# check <name> <expect: ok|err> <command...>
check() {
  local name="$1" want="$2"; shift 2
  local output status
  set +e
  output="$("$@" 2>&1)"
  status=$?
  set -e
  local got="ok"; [ "${status}" -ne 0 ] && got="err"
  if [ "${got}" = "${want}" ]; then
    say "  PASS  ${name}  (${got})"
    pass=$((pass + 1))
  else
    say "  FAIL  ${name}  wanted ${want}, got ${got} (exit ${status})"
    say "        ${output}"
    fail=$((fail + 1))
  fi
  printf '%s\n' "${output}" >> "${log}"
}

say "issue-120 round trip — ${label}"
say "binary: ${shux_bin}"
say "version: $(sx version 2>&1 | head -n 1)"
say ""

# ── fixture ──────────────────────────────────────────────────────────────
# Trailing argv, not --cmd: --cmd is split on whitespace rather than run
# through a shell (issue #125), which would exec `printf` with `sleep` as an
# argument and leave the pane dead.
probe='printf "\033[38;2;255;0;128mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[31mBASIC\033[0m\n"; sleep 300'
sx session create "${session}" -d -- sh -c "${probe}" >/dev/null

full_pane="$(sx pane list -s "${session}" --format json \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["id"])')"
full_sess="$(sx session list --format json \
  | S="${session}" python3 -c 'import sys, json, os
name = os.environ["S"]
print(next(s["id"] for s in json.load(sys.stdin)["sessions"] if s["name"] == name))')"
full_win="$(sx window list -s "${session}" --format json \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["id"])')"

# Content, THEN settle. `wait-for` on the probe text is the only thing that
# distinguishes "the shell has printed" from "the shell has not started".
sx pane wait-for -s "${session}" -p "${full_pane}" --text TRUECOLOR --timeout-ms 15000 >/dev/null

# The ids as a PERSON receives them: read back off the listings, not sliced
# from the json. If the listing ever stops printing 8 characters, this notices.
short_pane="$(sx --format plain pane list -s "${session}" | head -n 1 | cut -f1)"
short_sess="$(sx --format plain session list | awk -F'\t' -v n="${session}" '$1==n {print $4}')"

say "pane    ${full_pane}   printed as: ${short_pane}"
say "session ${full_sess}   printed as: ${short_sess}"
say "window  ${full_win}"
say ""

[ "${short_pane}" = "${full_pane:0:8}" ] \
  || { say "ABORT: pane list printed '${short_pane}', not the first 8 characters"; exit 1; }
[ "${short_sess}" = "${full_sess:0:8}" ] \
  || { say "ABORT: session list printed '${short_sess}', not the first 8 characters"; exit 1; }

# ── the round trip ───────────────────────────────────────────────────────
say "round trip — feed each printed id back in"
check "glance by printed pane id"        ok  sx pane glance "${short_pane}" --text-only
check "capture by printed pane id"       ok  sx pane capture -s "${session}" -p "${short_pane}"
check "wait-settled by printed pane id"  ok  sx pane wait-settled "${short_pane}" --quiet 100 --timeout 5000
check "checkpoint by printed pane id"    ok  sx pane checkpoint "${short_pane}"
check "title by printed pane id"         ok  sx pane title -s "${session}" -p "${short_pane}" -t probe
check "pane list by printed session id"  ok  sx pane list -s "${short_sess}"
check "pane list by window uuid"         ok  sx pane list -s "${session}" -w "${full_win}"
check "pane list by short window id"     ok  sx pane list -s "${session}" -w "${full_win:0:8}"
say ""

# ── the round trip lands on the RIGHT entity ─────────────────────────────
# Resolving must not merely stop erroring.
if [ "${expect_defect}" != "1" ]; then
  by_short="$(sx pane capture -s "${session}" -p "${short_pane}")"
  by_full="$(sx pane capture -s "${session}" -p "${full_pane}")"
  if [ "${by_short}" = "${by_full}" ]; then
    say "  PASS  short id and full uuid capture the same pane"
    pass=$((pass + 1))
  else
    say "  FAIL  short id and full uuid captured DIFFERENT panes"
    fail=$((fail + 1))
  fi
  case "${by_short}" in
    *TRUECOLOR*)
      say "  PASS  the captured screen carries the colour probe (not blank)"
      pass=$((pass + 1)) ;;
    *)
      say "  FAIL  captured screen has no probe text — a blank pane would pass every check above"
      fail=$((fail + 1)) ;;
  esac
  say ""
fi

# ── things that must still be refused ────────────────────────────────────
# Leniency is not guessing. These hold in BOTH arms, so they are asserted in
# both: the pre-fix binary rejects them too, just for the wrong reason.
say "refusals"
check "three characters is too short"     err sx pane glance abc --text-only
check "non-hex is not an id"              err sx pane glance zzzzzzzz --text-only
check "empty is not an id"                err sx pane glance "" --text-only
check "a prefix matching nothing"         err sx pane glance ffffffffff --text-only
check "an unknown full uuid"              err sx pane glance 00000000-0000-4000-8000-000000000001 --text-only
say ""

say "pass ${pass}  fail ${fail}"

if [ "${expect_defect}" = "1" ]; then
  # The baseline arm must FAIL the round trip. If it does not, either the
  # binary under test already has the fix or the harness is not exercising
  # what it claims to.
  if [ "${fail}" -eq 0 ]; then
    say "EXPECT_DEFECT=1 but every check passed — the defect did not reproduce"
    exit 1
  fi
  say "VERDICT: DEFECT REPRODUCED (${fail} round-trip failures, as expected)"
  exit 0
fi

if [ "${fail}" -ne 0 ]; then
  say "VERDICT: FAIL"
  exit 1
fi
say "VERDICT: PASS"
