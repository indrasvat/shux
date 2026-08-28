#!/usr/bin/env bash
# Regression test for shux automation process hygiene.

set -euo pipefail

# shellcheck disable=SC2034  # consumed by lib/proc_scope.sh
# `pwd -P`: a daemon's argv is always the physical path (see lib/proc_scope.sh).
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
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
# ONE exit path. The asserts below `exit 1` from inside the trap, which abandons the rest
# of a `;`-joined trap command — so the cleanup that used to trail them was skipped on
# exactly the failure paths the trap exists for, and every run leaked its temp dirs even
# when green. Each assert runs in a subshell (it kills what it reports either way, and
# keeps no state), so a failure is recorded instead of fatal and cleanup always runs.
on_exit() {
  local rc=0
  [ -z "${baseline_runtime:-}" ] || stop_baseline_daemon
  [ -z "${foreign_root:-}" ] || stop_foreign_daemon
  (assert_leaked_daemon_reaped "${selftest_runtime}") || rc=1
  (assert_no_new_orphan_automation_processes "${orphan_baseline}") || rc=1
  rm -f "${orphan_baseline}"
  rm -rf "${selftest_runtime}"
  [ -z "${baseline_runtime:-}" ] || rm -rf "${baseline_runtime}"
  [ -z "${foreign_root:-}" ] || rm -rf "${foreign_root}"
  [ "${rc}" -eq 0 ] || exit 1
}
trap on_exit EXIT

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

# ── issue #179: an orphan from an interrupted run is baseline, and must be NAMED ──
#
# The daemon here is started OUTSIDE the guard, so it is baseline exactly as an orphan
# is. A second daemon is started from a path outside REPO_ROOT: the reaper's central
# safety claim is that it never selects one, and only asserting inclusion left dropping
# the scope filter entirely invisible. The reaper is only ever run with `--dry-run` —
# a self-test must never be able to kill a process it did not create — so this also has
# to prove `--dry-run` kills nothing, which nothing downstream would notice.
baseline_runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-baseline.XXXXXX")"
foreign_root="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-foreign.XXXXXX")"

stop_daemon_at() {
  local runtime="$1" pid
  pid="$(cat "${runtime}/shux/shux.pid" 2>/dev/null || true)"
  case "${pid}" in '' | *[!0-9]*) return 0 ;; esac
  [ "${pid}" -gt 1 ] || return 0
  terminate_pids "${pid}"
  # A daemon that had to be SIGKILLed leaves its pidfile behind, and the pid is free to
  # be recycled the moment it dies. Drop the file so a later call cannot signal whatever
  # inherited the number — the rule lib/proc_scope.sh was written to record.
  rm -f "${runtime}/shux/shux.pid"
}
stop_baseline_daemon() { stop_daemon_at "${baseline_runtime}"; }
stop_foreign_daemon() { stop_daemon_at "${foreign_root}/run"; }

# Match a pid as a whole field, never as a substring: `ps` lines carry socket paths and
# `mktemp` suffixes full of digits, so `case $listing in *${pid}*` passes on the wrong
# process, and against a list of bare pids `456` matches `1456`.
names_pid() {
  local pid="$1" text="$2" fields
  fields="$(printf '%s\n' "${text}" | awk '{print $1}')"
  # shellcheck disable=SC2086  # deliberate split: these are pids
  pid_in_list "${pid}" ${fields}
}

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${baseline_runtime}" "${shux_bin}" --format json \
  session create leak-guard-baseline-$$ -d -- sh -lc 'sleep 60' >/dev/null
baseline_pid="$(cat "${baseline_runtime}/shux/shux.pid")"

mkdir -p "${foreign_root}/bin" "${foreign_root}/run"
cp "${shux_bin}" "${foreign_root}/bin/shux"
(cd "${foreign_root}" && env -u SHUX_SOCKET XDG_RUNTIME_DIR="${foreign_root}/run" \
  "${foreign_root}/bin/shux" --format json \
  session create leak-guard-foreign-$$ -d -- sh -lc 'sleep 60' >/dev/null)
foreign_pid="$(cat "${foreign_root}/run/shux/shux.pid")"

set +e
baseline_warning="$(.shux/scripts/no_leak_guard.sh true 2>&1 >/dev/null)"
guard_status=$?
set -e
if [ "${guard_status}" -ne 0 ]; then
  echo "no_leak_guard failed on a no-op command (status ${guard_status})" >&2
  echo "${baseline_warning}" >&2
  exit 1
fi
case "${baseline_warning}" in
  *EXEMPT*) ;;
  *)
    echo "no_leak_guard stayed silent about an already-running daemon (${baseline_pid})" >&2
    echo "${baseline_warning}" >&2
    exit 1
    ;;
esac
if ! names_pid "${baseline_pid}" "${baseline_warning}"; then
  echo "no_leak_guard's exempt list does not name daemon ${baseline_pid}" >&2
  echo "${baseline_warning}" >&2
  exit 1
fi

# A CLIENT that merely mentions `__daemon` is not a daemon. It was one while the match
# was a substring of the whole argv, which made it exempt here and a kill target there.
env -u SHUX_SOCKET XDG_RUNTIME_DIR="${baseline_runtime}" "${shux_bin}" \
  events watch --filter 'pane.__daemonx' >/dev/null 2>&1 &
client_pid=$!
sleep 1
if names_pid "${client_pid}" "$(shux_daemon_pids)"; then
  echo "a shux client with __daemon in its argv was classified as a daemon (${client_pid})" >&2
  describe_pid "${client_pid}" >&2
  kill -KILL "${client_pid}" >/dev/null 2>&1 || true
  exit 1
fi
kill -TERM "${client_pid}" >/dev/null 2>&1 || true
wait "${client_pid}" 2>/dev/null || true

reap_listing="$(.shux/scripts/reap_daemons.sh --dry-run)"
if ! names_pid "${baseline_pid}" "${reap_listing}"; then
  echo "reap_daemons.sh did not select the already-running daemon ${baseline_pid}" >&2
  echo "${reap_listing}" >&2
  exit 1
fi
if names_pid "${foreign_pid}" "${reap_listing}"; then
  echo "reap_daemons.sh selected a daemon outside this checkout (${foreign_pid})" >&2
  echo "${reap_listing}" >&2
  exit 1
fi
for pid in "${baseline_pid}" "${foreign_pid}"; do
  if pid_is_gone "${pid}"; then
    echo "reap_daemons.sh --dry-run stopped daemon ${pid}" >&2
    exit 1
  fi
done

# Reached through a symlink the scoping must still hold: the daemon's argv is the
# physical path, so a logical REPO_ROOT matched nothing and the reaper reported — out
# loud — that no daemons of this checkout were running.
symlinked_root="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-link.XXXXXX")/link"
ln -sfn "${REPO_ROOT}" "${symlinked_root}"
symlinked_listing="$(cd "${symlinked_root}" && .shux/scripts/reap_daemons.sh --dry-run)"
rm -rf "$(dirname "${symlinked_root}")"
if ! names_pid "${baseline_pid}" "${symlinked_listing}"; then
  echo "reap_daemons.sh missed daemon ${baseline_pid} when reached through a symlink" >&2
  echo "${symlinked_listing}" >&2
  exit 1
fi

# The warning is advisory; a failed write to stderr must not stop the wrapped command.
# Under `set -e` it did — the guard aborted before running anything and exited 1, its own
# leaked-daemon status, for a run that executed nothing.
guard_marker="$(mktemp "${TMPDIR:-/tmp}/shux-leak-guard-marker.XXXXXX")"
rm -f "${guard_marker}"
set +e
.shux/scripts/no_leak_guard.sh sh -c "touch '${guard_marker}'; exit 7" 2>&-
closed_stderr_status=$?
set -e
if [ "${closed_stderr_status}" -ne 7 ] || [ ! -f "${guard_marker}" ]; then
  echo "no_leak_guard with stderr closed: status ${closed_stderr_status} (want 7), command ran: $([ -f "${guard_marker}" ] && echo yes || echo no)" >&2
  rm -f "${guard_marker}"
  exit 1
fi
rm -f "${guard_marker}"

# Assert with the guard's own instrument, not `kill -0`: an orphan reparented to a
# non-reaping init stays a zombie for a while, and `kill -0` cannot tell that from a
# running daemon.
stop_baseline_daemon
if names_pid "${baseline_pid}" "$(shux_daemon_pids)"; then
  echo "leak guard self-test could not stop its own baseline daemon ${baseline_pid}" >&2
  describe_pid "${baseline_pid}" >&2
  exit 1
fi
stop_foreign_daemon

# The stranded-child sweep is the most destructive code in the reaper, so it is exercised
# for REAL — but only once the daemons this test made are gone and no other daemon of this
# checkout is running, so a real reap cannot reach a concurrent session's work.
if [ -z "$(shux_daemon_pids)" ]; then
  wedge_runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-wedge.XXXXXX")"
  bystander_needle="leak-guard-bystander-$$"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${wedge_runtime}" "${shux_bin}" --format json \
    session create leak-guard-wedge-$$ -d -- sh -lc 'sleep 400' >/dev/null
  wedge_pid="$(cat "${wedge_runtime}/shux/shux.pid")"
  wedge_children="$(ps -axo pid=,ppid= | awk -v p="${wedge_pid}" '$2 == p { print $1 }')"
  if [ -z "${wedge_children}" ]; then
    echo "leak guard self-test: wedged daemon ${wedge_pid} had no pane child to strand" >&2
    kill -KILL "${wedge_pid}" >/dev/null 2>&1 || true
    rm -rf "${wedge_runtime}"
    exit 1
  fi
  # Too wedged to service SIGTERM, so the reaper must escalate to SIGKILL — the only path
  # that strands pane children, because SIGKILL runs none of the daemon's teardown.
  kill -STOP "${wedge_pid}"
  # A process this checkout orphans DURING the reap window, owned by nobody the reap
  # touched. Attribution by "appeared between two snapshots" killed it.
  #
  # It reports its own pid: `setsid` forks, so `$!` is a process that has already exited,
  # and killing that one leaves the real bystander — and its `sleep` — running. Cleanup
  # signals the whole session `setsid` made, so nothing outlives this block.
  bystander_pidfile="$(mktemp "${TMPDIR:-/tmp}/shux-leak-guard-bystander.XXXXXX")"
  # Detached in a subshell so bash does not report our own deliberate kill as a job.
  (setsid sh -c "echo \$\$ >'${bystander_pidfile}'; sleep 300; : ${bystander_needle}" \
    >/dev/null 2>&1 &)
  sleep 0.5
  bystander_pid="$(cat "${bystander_pidfile}")"
  rm -f "${bystander_pidfile}"

  set +e
  .shux/scripts/reap_daemons.sh >/dev/null
  wedge_reap_status=$?
  set -e
  bystander_alive=1
  pid_is_gone "${bystander_pid}" && bystander_alive=0
  kill -KILL -- "-${bystander_pid}" >/dev/null 2>&1 || true
  rm -rf "${wedge_runtime}"

  if [ "${wedge_reap_status}" -ne 0 ]; then
    echo "reap_daemons.sh failed on a wedged daemon (status ${wedge_reap_status})" >&2
    exit 1
  fi
  for pid in ${wedge_children}; do
    pid_is_gone "${pid}" && continue
    echo "reap_daemons.sh left the wedged daemon's pane child running (${pid})" >&2
    describe_pid "${pid}" >&2
    kill -KILL "${pid}" >/dev/null 2>&1 || true
    exit 1
  done
  if [ "${bystander_alive}" -ne 1 ]; then
    echo "reap_daemons.sh killed a process it did not strand (${bystander_pid})" >&2
    exit 1
  fi
else
  echo "note: other daemons from this checkout are running; skipping the reap sweep case" >&2
fi

# The other half of the contract: with nothing of ours running, the guard says nothing.
# Only assertable on an empty process table — a concurrent session in this checkout is
# entitled to its daemon, and warning about that one is the feature working.
if [ -z "$(shux_daemon_pids)" ]; then
  set +e
  quiet_warning="$(.shux/scripts/no_leak_guard.sh true 2>&1 >/dev/null)"
  quiet_status=$?
  set -e
  if [ "${quiet_status}" -ne 0 ] || [ -n "${quiet_warning}" ]; then
    echo "no_leak_guard was not quiet with no daemons running (status ${quiet_status})" >&2
    echo "${quiet_warning}" >&2
    exit 1
  fi
else
  echo "note: other daemons from this checkout are running; skipping the quiet-baseline case" >&2
fi

assert_no_new_orphan_automation_processes "${orphan_baseline}"
echo "shux leak guard self-test passed"
