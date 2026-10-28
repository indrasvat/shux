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
# Everything a wedge case below creates, on every exit path: the stopped daemon,
# its pane tree's process groups, the subreaper, the bystander's session and the
# runtime dir. The case clears each variable once it has cleaned that part up.
cleanup_wedge() {
  local pid
  [ -z "${wedge_pid:-}" ] || kill -KILL "${wedge_pid}" >/dev/null 2>&1 || true
  for pid in ${wedge_tree:-}; do
    kill -KILL -- "-${pid}" >/dev/null 2>&1 || kill -KILL "${pid}" >/dev/null 2>&1 || true
  done
  [ -z "${holder:-}" ] || kill -KILL "${holder}" >/dev/null 2>&1 || true
  [ -z "${bystander_pid:-}" ] || kill -KILL -- "-${bystander_pid}" >/dev/null 2>&1 || true
  rm -rf "${wedge_runtime}"
}
on_exit() {
  local rc=0
  [ -z "${wedge_runtime:-}" ] || cleanup_wedge
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
# The leak is `sleep` exec'd through a SYMLINK named `Xvfb`, not a real X server.
# `ps -o comm=` reports the exec'd path's basename, which is the whole of what the
# guard matches on, so the link exercises the real matching path — and this case
# then runs on a machine with no X server installed, which is most of them. Not a
# copy: macOS SIGKILLs a copied Apple platform binary at exec, so a copied
# `sleep` died at once, left nothing behind, and this case failed on every Mac.
xvfb_shim_dir="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-xvfb.XXXXXX")"
ln -s "$(command -v sleep)" "${xvfb_shim_dir}/Xvfb"

# The fixture records the pid it orphaned, and the guard has to NAME it: a
# fixture that failed to launch also makes the guard exit non-zero, and that
# is not detection.
set +e
xvfb_guard_err="$(.shux/scripts/no_leak_guard.sh python3 - "${xvfb_shim_dir}/Xvfb" "${xvfb_shim_dir}/pid" 2>&1 >/dev/null <<'XVFBLEAK'
import subprocess
import sys
import time

p = subprocess.Popen(
    [sys.argv[1], "60"],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    start_new_session=True,
    close_fds=True,
)
with open(sys.argv[2], "w") as f:
    f.write(str(p.pid))
time.sleep(0.25)
sys.exit(0)
XVFBLEAK
)"
xvfb_guard_status=$?
set -e
xvfb_pid="$(cat "${xvfb_shim_dir}/pid" 2>/dev/null || true)"
rm -rf "${xvfb_shim_dir}"
if [ -n "${xvfb_pid}" ]; then
  kill -KILL -- "-${xvfb_pid}" >/dev/null 2>&1 || true
fi

if [ -z "${xvfb_pid}" ]; then
  echo "the orphaned-X-server fixture never started (guard status ${xvfb_guard_status}):" >&2
  echo "${xvfb_guard_err}" >&2
  exit 1
fi
if [ "${xvfb_guard_status}" -eq 0 ] \
  || ! grep -Eq "orphan automation process\(es\):(.* )?${xvfb_pid}( |$)" <<<"${xvfb_guard_err}"; then
  echo "no_leak_guard did not report the orphaned X server ${xvfb_pid} (status ${xvfb_guard_status}):" >&2
  echo "${xvfb_guard_err}" >&2
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

# Every descendant of a pid (mirrors reap_daemons.sh, which this must not trust).
tree_pids() {
  local pid
  for pid in $(ps -axo pid=,ppid= | awk -v p="$1" '$2 == p { print $1 }'); do
    printf '%s\n' "${pid}"
    tree_pids "${pid}"
  done
}

# One wedged daemon, reaped for real.
#
# The pane's shell backgrounds a `sleep` that ignores SIGHUP before it execs, so a
# daemon's SIGKILL strands a grandchild that outlives the pane's own shell. With
# `subreaper`, the daemon is started below a child subreaper, as under `systemd --user`
# on a desktop: its orphans are adopted by that, not by init, and nothing reaps them.
# With `forking`, the pane keeps forking children that ignore HUP and TERM, so some are
# born after the reaper's first walk of the tree. With `handoff`, a pane process waits
# for the daemon to die, then forks a child and exits: the child's parent is gone and
# no lineage leads back to the daemon unless the tree was frozen before the kill.
reap_wedged_daemon() {
  local mode="$1" wedge_runtime wedge_pid wedge_tree="" bystander_needle
  local bystander_pid="" wedge_reap_status bystander_alive pid holder="" ready
  wedge_runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-leak-guard-wedge.XXXXXX")"
  wedge_pid=""
  # Every early exit goes through here, so a failed setup leaks nothing.
  abort_wedge() {
    echo "leak guard self-test: $1" >&2
    local victim
    for victim in ${wedge_pid:+$(tree_pids "${wedge_pid}")} ${wedge_pid} ${holder}; do
      kill -KILL "${victim}" >/dev/null 2>&1 || true
    done
    [ -z "${holder}" ] || wait "${holder}" 2>/dev/null || true
    wedge_pid="" holder=""
    rm -rf "${wedge_runtime}"
    exit 1
  }
  bystander_needle="leak-guard-bystander-$$"
  local pane="trap '' HUP; sleep 401 & exec sleep 400"
  # Unique to this run, so the survivor scan below cannot count another suite's sleeps.
  local fork_needle="7$$"
  [ "${mode}" != forking ] ||
    pane="trap '' HUP TERM; while :; do sleep ${fork_needle} & sleep 0.05; done"
  if [ "${mode}" = handoff ]; then
    # `read` is a builtin, so the daemon's death is seen before a reaper's next step.
    cat >"${wedge_runtime}/handoff.sh" <<HANDOFF
trap '' HUP TERM
while read -r _ _ state _ </proc/\$1/stat 2>/dev/null && [ "\${state}" != Z ]; do sleep 0.01; done
sleep ${fork_needle} &
HANDOFF
    pane="trap '' HUP TERM; sh '${wedge_runtime}/handoff.sh' \$PPID & exec sleep 400"
  fi
  set -- env -u SHUX_SOCKET XDG_RUNTIME_DIR="${wedge_runtime}" "${shux_bin}" --format json \
    session create leak-guard-wedge-$$ -d -- sh -c "${pane}"
  if [ "${mode}" = subreaper ]; then
    ready="${wedge_runtime}/subreaper-ready"
    python3 -c '
import ctypes, subprocess, sys, time
assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0
subprocess.run(sys.argv[2:], check=True, stdout=subprocess.DEVNULL)
open(sys.argv[1], "w").close()
time.sleep(600)
' "${ready}" "$@" &
    holder=$!
    for _ in $(seq 100); do
      [ -e "${ready}" ] && break
      sleep 0.1
    done
    if [ ! -e "${ready}" ]; then
      wedge_pid="$(cat "${wedge_runtime}/shux/shux.pid" 2>/dev/null || true)"
      abort_wedge "the subreaper ${holder} never reported its session created"
    fi
  else
    "$@" >/dev/null || {
      wedge_pid="$(cat "${wedge_runtime}/shux/shux.pid" 2>/dev/null || true)"
      abort_wedge "could not create the wedged daemon's session"
    }
  fi
  wedge_pid="$(cat "${wedge_runtime}/shux/shux.pid" 2>/dev/null || true)"
  [ -n "${wedge_pid}" ] || abort_wedge "the wedged daemon wrote no pidfile"
  if [ -n "${holder}" ] && [ "$(ps -p "${wedge_pid}" -o ppid= | tr -d '[:space:]')" != "${holder}" ]; then
    abort_wedge "daemon ${wedge_pid} is not below the subreaper ${holder}"
  fi
  # The backgrounded sleep is forked after the session is created; wait for it.
  for _ in $(seq 50); do
    wedge_tree="$(tree_pids "${wedge_pid}")"
    [ "$(printf '%s\n' "${wedge_tree}" | grep -c .)" -ge 2 ] && break
    sleep 0.1
  done
  if [ "$(printf '%s\n' "${wedge_tree}" | grep -c .)" -lt 2 ]; then
    abort_wedge "wedged daemon ${wedge_pid} had no pane tree to strand"
  fi
  # Too wedged to service SIGTERM, so the reaper must escalate to SIGKILL — the only path
  # that strands pane children, because SIGKILL runs none of the daemon's teardown.
  kill -STOP "${wedge_pid}"
  # A process this checkout orphans DURING the reap window, owned by nobody the reap
  # touched. Attribution by "appeared between two snapshots" killed it.
  #
  # A new session leader, orphaned as soon as its launcher exits. python3 rather than
  # `setsid(1)`, which macOS does not ship — there the bystander was never created and
  # this case failed with an empty pid. Cleanup signals the whole session, so nothing
  # outlives this block.
  bystander_pid="$(python3 - "${bystander_needle}" <<'BYSTANDER'
import subprocess
import sys

p = subprocess.Popen(
    ["sh", "-c", "sleep 300; : " + sys.argv[1]],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    start_new_session=True,
    close_fds=True,
)
print(p.pid)
BYSTANDER
)"
  if [ -z "${bystander_pid}" ] || pid_is_gone "${bystander_pid}"; then
    echo "could not start the bystander process this case needs" >&2
    exit 1
  fi

  set +e
  .shux/scripts/reap_daemons.sh >/dev/null
  wedge_reap_status=$?
  set -e
  bystander_alive=1
  pid_is_gone "${bystander_pid}" && bystander_alive=0
  kill -KILL -- "-${bystander_pid}" >/dev/null 2>&1 || true
  bystander_pid=""
  pid_is_gone "${wedge_pid}" && wedge_pid=""
  # Checked before the subreaper exits, which would hand its orphans to init.
  local survivors=""
  for pid in ${wedge_tree}; do
    pid_is_gone "${pid}" || survivors="${survivors} ${pid}"
  done
  if [ "${mode}" = forking ] || [ "${mode}" = handoff ]; then
    # shellcheck disable=SC2009  # ps|grep is required by the process-hygiene rule
    for pid in $(ps -axo pid=,args= | grep -E "^ *[0-9]+ sleep ${fork_needle}\$" | awk '{ print $1 }'); do
      case " ${survivors} " in *" ${pid} "*) ;; *) pid_is_gone "${pid}" || survivors="${survivors} ${pid}" ;; esac
    done
  fi
  for pid in ${survivors}; do
    echo "reap_daemons.sh left the wedged daemon's pane process running (${pid})" >&2
    describe_pid "${pid}" >&2
    kill -KILL "${pid}" >/dev/null 2>&1 || true
  done
  if [ -n "${holder}" ]; then
    kill -TERM "${holder}" >/dev/null 2>&1 || true
    wait "${holder}" 2>/dev/null || true
  fi
  rm -rf "${wedge_runtime}"
  wedge_tree="" holder="" wedge_runtime=""
  [ -z "${survivors}" ] || exit 1

  if [ "${wedge_reap_status}" -ne 0 ]; then
    echo "reap_daemons.sh failed on a wedged daemon (status ${wedge_reap_status})" >&2
    exit 1
  fi
  if [ "${bystander_alive}" -ne 1 ]; then
    echo "reap_daemons.sh killed a process it did not strand" >&2
    exit 1
  fi
}

# The stranded-child sweep is the most destructive code in the reaper, so it is exercised
# for REAL — but only once the daemons this test made are gone and no other daemon of this
# checkout is running, so a real reap cannot reach a concurrent session's work.
if [ -z "$(shux_daemon_pids)" ]; then
  reap_wedged_daemon plain
  reap_wedged_daemon forking
  # PR_SET_CHILD_SUBREAPER is Linux-only, and `handoff` watches /proc.
  if [ "$(uname -s)" = "Linux" ]; then
    reap_wedged_daemon subreaper
    reap_wedged_daemon handoff
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
