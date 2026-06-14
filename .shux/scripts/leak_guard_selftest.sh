#!/usr/bin/env bash
# Regression test for shux automation process hygiene.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
if [ ! -x "${shux_bin}" ]; then
  echo "missing shux binary: ${shux_bin}" >&2
  exit 2
fi

assert_no_new_shux() {
  local baseline_file="$1"
  local current pid
  current="$(pgrep -x shux 2>/dev/null || true)"
  while read -r pid; do
    [ -n "${pid}" ] || continue
    if ! grep -qx "${pid}" "${baseline_file}"; then
      echo "leak guard self-test left a new shux process: ${pid}" >&2
      ps -p "${pid}" -o pid=,ppid=,stat=,args= >&2 || true
      kill -TERM "${pid}" >/dev/null 2>&1 || true
      sleep 1
      kill -KILL "${pid}" >/dev/null 2>&1 || true
      exit 1
    fi
  done <<<"${current}"
}

orphan_tty_pids() {
  ps -axo pid=,ppid=,tty=,comm=,args= |
    awk '
      $2 == 1 && $3 ~ /^(ttys|pts\/)/ {
        print $1
      }
    '
}

assert_no_new_orphan_tty_processes() {
  local baseline_file="$1"
  local current pid
  current="$(orphan_tty_pids || true)"
  while read -r pid; do
    [ -n "${pid}" ] || continue
    if ! grep -qx "${pid}" "${baseline_file}"; then
      echo "leak guard self-test left a new orphan PTY process: ${pid}" >&2
      ps -p "${pid}" -o pid=,ppid=,stat=,tty=,args= >&2 || true
      kill -TERM "${pid}" >/dev/null 2>&1 || true
      sleep 1
      kill -KILL "${pid}" >/dev/null 2>&1 || true
      exit 1
    fi
  done <<<"${current}"
}

baseline="$(mktemp "${TMPDIR:-/tmp}/shux-leak-baseline.XXXXXX")"
orphan_baseline="$(mktemp "${TMPDIR:-/tmp}/shux-orphan-leak-baseline.XXXXXX")"
pgrep -x shux >"${baseline}" 2>/dev/null || true
orphan_tty_pids >"${orphan_baseline}" 2>/dev/null || true
trap 'assert_no_new_shux "${baseline}"; assert_no_new_orphan_tty_processes "${orphan_baseline}"; rm -f "${baseline}" "${orphan_baseline}"' EXIT

set +e
SHUX_HARNESS_TIMEOUT_IMPL=bash shux_harness_timeout 1s bash -lc 'sleep 30'
timeout_status=$?
set -e
if [ "${timeout_status}" -ne 124 ]; then
  echo "expected Bash timeout fallback to return 124, got ${timeout_status}" >&2
  exit 1
fi

set +e
.shux/scripts/no_leak_guard.sh bash -lc "
  set -euo pipefail
  runtime=\"\$(mktemp -d \"\${TMPDIR:-/tmp}/shux-leak-guard-selftest.XXXXXX\")\"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR=\"\${runtime}\" \"${shux_bin}\" --format json \
    session create leak-guard-selftest-\$\$ -d -- sh -lc 'sleep 60' >/dev/null
"
guard_status=$?
set -e

if [ "${guard_status}" -eq 0 ]; then
  echo "no_leak_guard did not fail for an intentionally leaked daemon" >&2
  exit 1
fi

assert_no_new_shux "${baseline}"

set +e
.shux/scripts/no_leak_guard.sh python3 - <<'PY'
import os
import pty
import shutil
import subprocess
import sys
import time

master, slave = pty.openpty()
shell = shutil.which("bash") or "/bin/sh"
subprocess.Popen(
    [shell, "-lc", "sleep 60"],
    stdin=slave,
    stdout=slave,
    stderr=slave,
    pass_fds=(master,),
    start_new_session=True,
    close_fds=True,
)
os.close(slave)
time.sleep(0.25)
sys.exit(0)
PY
orphan_guard_status=$?
set -e

if [ "${orphan_guard_status}" -eq 0 ]; then
  echo "no_leak_guard did not fail for an intentionally orphaned PTY process" >&2
  exit 1
fi

assert_no_new_orphan_tty_processes "${orphan_baseline}"
echo "shux leak guard self-test passed"
