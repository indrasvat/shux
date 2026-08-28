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

# An orphaned X server or GUI terminal — the shapes the issue #175 rig owns.
#
# Before `Xvfb`/`kitty` were added to the orphan allowlist this was a silent
# hole: measured, a leaked Xvfb with its cwd inside this repo and no controlling
# tty matched neither branch of `orphan_candidate_pids`, so the guard reported
# success while the server ran on, holding its display number for good.
#
# The leak is a COPY of `sleep` named `Xvfb`, not a real X server. `ps -o comm=`
# reports the executable's basename, which is the whole of what the guard matches
# on, so the copy exercises the real matching path — and this case then runs on a
# machine with no X server installed, which is most of them.
xvfb_shim_dir="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-xvfb.XXXXXX")"
cp "$(command -v sleep)" "${xvfb_shim_dir}/Xvfb"

set +e
.shux/scripts/no_leak_guard.sh python3 - "${xvfb_shim_dir}/Xvfb" <<'XVFBLEAK'
import subprocess
import sys
import time

subprocess.Popen(
    [sys.argv[1], "60"],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    start_new_session=True,
    close_fds=True,
)
time.sleep(0.25)
sys.exit(0)
XVFBLEAK
xvfb_guard_status=$?
set -e
rm -rf "${xvfb_shim_dir}"

if [ "${xvfb_guard_status}" -eq 0 ]; then
  echo "no_leak_guard did not fail for an intentionally orphaned X server" >&2
  exit 1
fi

# A daemon orphaned by an interrupted run is BASELINE for every later guard invocation:
# exempt by construction, and once silently so — four of them sat in `ps` through a full
# green `make check` (issue #179). The guard must name them, and `make reap` must be able
# to select them.
#
# The daemon here is started OUTSIDE the guard, so it is baseline exactly as an orphan is.
# The reaper is exercised with `--dry-run`: a self-test must never be able to kill a
# process it did not create, and a real reap is machine-wide within this checkout.
baseline_runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-baseline.XXXXXX")"
stop_baseline_daemon() {
  local pid
  pid="$(cat "${baseline_runtime}/shux/shux.pid" 2>/dev/null || true)"
  case "${pid}" in ''|*[!0-9]*) return 0 ;; esac
  [ "${pid}" -gt 1 ] || return 0
  terminate_pids "${pid}"
}

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${baseline_runtime}" "${shux_bin}" --format json \
  session create leak-guard-baseline-$$ -d -- sh -lc 'sleep 60' >/dev/null
baseline_pid="$(cat "${baseline_runtime}/shux/shux.pid")"
trap 'stop_baseline_daemon; assert_leaked_daemon_reaped "${selftest_runtime}"; assert_no_new_orphan_automation_processes "${orphan_baseline}"; rm -f "${orphan_baseline}"' EXIT

baseline_warning="$(mktemp "${TMPDIR:-/tmp}/shux-leak-guard-warning.XXXXXX")"
.shux/scripts/no_leak_guard.sh true 2>"${baseline_warning}"

if ! grep -q "EXEMPT" "${baseline_warning}" || ! grep -q "${baseline_pid}" "${baseline_warning}"; then
  echo "no_leak_guard stayed silent about an already-running daemon (${baseline_pid})" >&2
  cat "${baseline_warning}" >&2
  rm -f "${baseline_warning}"
  exit 1
fi
rm -f "${baseline_warning}"

# Capture, then match. Piping into `grep -q` under `pipefail` reports the writer's
# SIGPIPE (141) as the pipeline's status, which reads as "the reaper failed" no matter
# what it printed — this assertion failed that way before it ever ran for real.
reap_listing="$(.shux/scripts/reap_daemons.sh --dry-run)"
case "${reap_listing}" in
  *"${baseline_pid}"*) ;;
  *)
    echo "reap_daemons.sh did not select the already-running daemon ${baseline_pid}" >&2
    echo "${reap_listing}" >&2
    exit 1
    ;;
esac

# Assert with the guard's own instrument, not `kill -0`: an orphan reparented to a
# non-reaping init stays a zombie for a while, and `kill -0` cannot tell that from a
# running daemon. `shux_daemon_pids` can — a zombie's argv is `[shux] <defunct>`, which
# matches neither `__daemon` nor this checkout's path.
stop_baseline_daemon
case "$(shux_daemon_pids)" in
  *"${baseline_pid}"*)
    echo "leak guard self-test could not stop its own baseline daemon ${baseline_pid}" >&2
    describe_pid "${baseline_pid}" >&2
    exit 1
    ;;
esac

# The other half of the contract: with nothing of ours running, the guard says nothing.
# Only assertable on an empty process table — a concurrent session in this checkout is
# entitled to its daemon, and warning about that one is the feature working.
if [ -z "$(shux_daemon_pids)" ]; then
  quiet_warning="$(mktemp "${TMPDIR:-/tmp}/shux-leak-guard-quiet.XXXXXX")"
  .shux/scripts/no_leak_guard.sh true 2>"${quiet_warning}"
  if grep -q "EXEMPT" "${quiet_warning}"; then
    echo "no_leak_guard warned about exempt daemons when none were running" >&2
    cat "${quiet_warning}" >&2
    rm -f "${quiet_warning}"
    exit 1
  fi
  rm -f "${quiet_warning}"
else
  echo "note: other daemons from this checkout are running; skipping the quiet-baseline case" >&2
fi

assert_no_new_orphan_automation_processes "${orphan_baseline}"
echo "shux leak guard self-test passed"
