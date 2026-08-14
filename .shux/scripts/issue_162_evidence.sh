#!/usr/bin/env bash
# Evidence for issue #162 — a pane that exits reports its exit status with an
# empty screen, so the documented agent loop (`events.watch` → `PaneExited` →
# `pane.capture`) captures nothing.
#
#   SHUX_BIN=<binary> LABEL=before EXPECT_DEFECT=1 .shux/scripts/issue_162_evidence.sh
#   SHUX_BIN=<binary> LABEL=after                  .shux/scripts/issue_162_evidence.sh
#
# WHAT IT CHECKS. One thing, from an agent's side: run a short command that
# prints colour and exits, wait for the exit status the way an agent waits for
# `PaneExited`, then capture. The captured screen must carry the output the
# command printed.
#
# WHY A DARWIN-EMULATING BINARY. The bytes are lost in `drain_read`, which
# discards everything it has already read when the read that follows fails.
# On Linux `EIO` (the PTY master's EOF) is mapped to EOF before it reaches that
# arm; on macOS it was not, so only macOS could reach it. The `before` arm is
# therefore built from HEAD with `is_pty_eof_errno` forced to its
# `not(target_os = "linux")` branch — the exact code macOS compiles.
#
# ASSERTIONS. Every rep asserts. `EXPECT_DEFECT=1` inverts the verdict, so the
# baseline arm fails if the defect does NOT reproduce and cannot pass vacuously.
# No check is written `|| true`.
#
# Output: .shux/out/issue-162/<label>/ (gitignored scratch).

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
label="${LABEL:-after}"
expect_defect="${EXPECT_DEFECT:-0}"
reps="${REPS:-10}"
out_dir="${repo_root}/.shux/out/issue-162/${label}"
# Short runtime dir: unix socket paths cap at ~108 bytes.
runtime="$(mktemp -d "/tmp/sx162-${label}.XXXXXX")"

mkdir -p "${out_dir}"
log="${out_dir}/exit-then-capture.txt"
: > "${log}"

session="ex162-${label}-$$"
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

# Truecolor + indexed + basic, so a monochrome regression cannot pass either.
probe='printf "\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n"'

say "issue-162 exit-then-capture — ${label}"
say "binary: ${shux_bin}"
say "version: $(sx version 2>&1 | head -n 1)"
say "reps:   ${reps}"
say ""

sx session create "${session}" -d -- sleep 600 >/dev/null

for rep in $(seq 1 "${reps}"); do
  # A window per rep: one short command that prints colour and exits.
  win="$(sx window create -s "${session}" -n "r${rep}" --format json -- \
    sh -c "${probe}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["id"])')"
  pane="$(sx pane list -s "${session}" -w "${win}" --format json \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["id"])')"

  # Wait the way an agent waits: on the exit status, which is what
  # `PaneExited` carries. Nothing here waits for content — that is the point.
  status=""
  for _ in $(seq 1 200); do
    status="$(sx pane list -s "${session}" -w "${win}" --format json \
      | P="${pane}" python3 -c 'import sys, json, os
pane = os.environ["P"]
p = next((p for p in json.load(sys.stdin) if p["id"] == pane), None)
print("" if p is None or p.get("exit_status") is None else p["exit_status"])')"
    [ -n "${status}" ] && break
    sleep 0.05
  done
  if [ -z "${status}" ]; then
    say "  rep ${rep}: ABORT — pane never reported an exit status"
    exit 1
  fi

  screen="$(sx pane capture -s "${session}" -p "${pane}")"
  if printf '%s' "${screen}" | grep -q TRUECOLOR \
    && printf '%s' "${screen}" | grep -q INDEXED \
    && printf '%s' "${screen}" | grep -q BASIC; then
    say "  rep ${rep}: PASS  exit_status=${status}  screen carries the probe"
    pass=$((pass + 1))
  else
    say "  rep ${rep}: FAIL  exit_status=${status}  screen is [$(printf '%s' "${screen}" | tr -d '\n')]"
    fail=$((fail + 1))
  fi
done

say ""
say "captures with the command's output: ${pass}/${reps}   empty: ${fail}/${reps}"

if [ "${expect_defect}" = "1" ]; then
  if [ "${fail}" -eq 0 ]; then
    say "VERDICT: the defect did NOT reproduce — this baseline proves nothing"
    exit 1
  fi
  say "VERDICT: defect reproduced (${fail}/${reps} panes reported an exit status with an empty screen)"
  exit 0
fi

if [ "${fail}" -ne 0 ]; then
  say "VERDICT: FAIL — ${fail}/${reps} panes reported an exit status with an empty screen"
  exit 1
fi
say "VERDICT: PASS — every exited pane's screen carried its output"
