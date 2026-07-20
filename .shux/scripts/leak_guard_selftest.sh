#!/usr/bin/env bash
# Regression test for shux automation process hygiene.

set -euo pipefail

# shellcheck disable=SC2034  # consumed by lib/proc_scope.sh
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=lib/proc_scope.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib/proc_scope.sh"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
if [ ! -x "${shux_bin}" ]; then
  echo "missing shux binary: ${shux_bin}" >&2
  exit 2
fi

# Assert the guard cleaned up the daemon THIS self-test intentionally leaked.
#
# Attributable by construction: the self-test owns the runtime dir, so it checks that
# dir's pidfile and nothing else. It used to compare a machine-wide `pgrep -x shux`
# against a baseline and TERM+KILL anything new — which SIGKILLed a concurrent session's
# in-flight `shux lens gate` during task 085. A self-test must never be able to kill a
# process it did not create.
assert_leaked_daemon_reaped() {
  local runtime="$1" pidfile pid
  pidfile="${runtime}/shux/shux.pid"
  [ -f "${pidfile}" ] || return 0
  pid="$(cat "${pidfile}" 2>/dev/null || true)"
  case "${pid}" in ''|*[!0-9]*) return 0 ;; esac
  [ "${pid}" -gt 1 ] || return 0
  kill -0 "${pid}" >/dev/null 2>&1 || return 0
  echo "leak guard did not reap the daemon it reported: ${pid} (${runtime})" >&2
  ps -p "${pid}" -o pid=,ppid=,stat=,args= >&2 || true
  kill -TERM "${pid}" >/dev/null 2>&1 || true
  sleep 1
  kill -KILL "${pid}" >/dev/null 2>&1 || true
  exit 1
}

assert_no_new_orphan_automation_processes() {
  local baseline_file="$1"
  local current pid
  current="$(orphan_candidate_pids || true)"
  while read -r pid; do
    [ -n "${pid}" ] || continue
    if ! grep -qx "${pid}" "${baseline_file}"; then
      echo "leak guard self-test left a new orphan automation process: ${pid}" >&2
      ps -p "${pid}" -o pid=,ppid=,stat=,tty=,args= >&2 || true
      kill -TERM "${pid}" >/dev/null 2>&1 || true
      sleep 1
      kill -KILL "${pid}" >/dev/null 2>&1 || true
      exit 1
    fi
  done <<<"${current}"
}

orphan_baseline="$(mktemp "${TMPDIR:-/tmp}/shux-orphan-leak-baseline.XXXXXX")"
orphan_candidate_pids >"${orphan_baseline}" 2>/dev/null || true
# The runtime dir this test's intentional leak will live in — owned here so the
# post-condition is attributable to exactly one daemon.
selftest_runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-selftest.XXXXXX")"
trap 'assert_leaked_daemon_reaped "${selftest_runtime}"; assert_no_new_orphan_automation_processes "${orphan_baseline}"; rm -f "${orphan_baseline}"' EXIT

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
  env -u SHUX_SOCKET XDG_RUNTIME_DIR=\"${selftest_runtime}\" \"${shux_bin}\" --format json \
    session create leak-guard-selftest-\$\$ -d -- sh -lc 'sleep 60' >/dev/null
"
guard_status=$?
set -e

if [ "${guard_status}" -eq 0 ]; then
  echo "no_leak_guard did not fail for an intentionally leaked daemon" >&2
  exit 1
fi

assert_leaked_daemon_reaped "${selftest_runtime}"

set +e
.shux/scripts/no_leak_guard.sh python3 - <<'PY'
import subprocess
import sys
import time

subprocess.Popen(
    ["sleep", "60"],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    start_new_session=True,
    close_fds=True,
)
time.sleep(0.25)
sys.exit(0)
PY
child_guard_status=$?
set -e

if [ "${child_guard_status}" -eq 0 ]; then
  echo "no_leak_guard did not fail for an intentionally orphaned automation process" >&2
  exit 1
fi

assert_no_new_orphan_automation_processes "${orphan_baseline}"

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

assert_no_new_orphan_automation_processes "${orphan_baseline}"
echo "shux leak guard self-test passed"
