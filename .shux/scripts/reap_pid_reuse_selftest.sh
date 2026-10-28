#!/usr/bin/env bash
# The reaper and `terminate_pids` against pid reuse, forced rather than hoped for.
#
# Runs inside a pid namespace of its own, where writing `ns_last_pid` hands a pid that
# just died to the next fork. Its `ps` sees only the namespace, so a reaper defect here
# cannot reach a process outside it, and the kernel kills whatever is left when this
# script, the namespace's init, exits. Linux only; skipped with a note elsewhere.

set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/$(basename "${BASH_SOURCE[0]}")"
if [ "${1:-}" != --in-namespace ]; then
  if [ "$(uname -s)" != Linux ] || ! unshare -Urfp --mount-proc true >/dev/null 2>&1; then
    echo "note: no pid namespace available here; skipping the pid-reuse reap cases" >&2
    exit 0
  fi
  exec unshare -Urfp --mount-proc bash "${script}" --in-namespace
fi

# shellcheck disable=SC2034  # consumed by lib/proc_scope.sh
REPO_ROOT="$(cd "$(dirname "${script}")/../.." && pwd -P)"
# shellcheck source=lib/proc_scope.sh disable=SC1091
. "$(dirname "${script}")/lib/proc_scope.sh"

dir="$(mktemp -d "${TMPDIR:-/tmp}/shux-reap-reuse.XXXXXX")"
trap 'rm -rf "${dir}"' EXIT
failed=0
fail() { echo "reap pid-reuse self-test: $*" >&2; failed=1; }

# A responsive daemon of this checkout, as `shux_daemon_pids` sees one: on TERM it ends
# its pane and exits, so the pane's pid is free while the reaper waits out its grace.
cat >"${dir}/daemon.sh" <<'DAEMON'
trap 'kill -KILL "${pane}"; wait "${pane}"; exit 0' TERM
sleep 400 &
pane=$!
echo "${pane}" >"$1"
wait
DAEMON
# Start times tick at 10ms, and a process born in the same tick as the one whose pid it
# took is indistinguishable from it. A reaped process is never that young.
settle() { sleep 0.1; }

start_daemon() {
  rm -f "${dir}/pane"
  (exec -a "${REPO_ROOT}/target/reap-selftest/shux __daemon" bash "${dir}/daemon.sh" "${dir}/pane") 2>/dev/null &
  daemon=$!
  for _ in $(seq 50); do
    [ -s "${dir}/pane" ] && break
    sleep 0.1
  done
  pane="$(cat "${dir}/pane")"
  settle
}

# Fork the process that will hold `$1` once it has died: TERM-proof, with a child. Only
# builtins run between the death and the fork, so no other fork can take the pid first.
take_pid() {
  local target="$1" spins=0
  while [ -e "/proc/${target}" ]; do
    spins=$((spins + 1))
    [ "${spins}" -lt 2000000 ] || { fail "pid ${target} never freed"; return 1; }
  done
  echo $((target - 1)) >/proc/sys/kernel/ns_last_pid
  (trap '' TERM HUP; sleep 401 & exec sleep 402) &
  victim=$!
  # Its end is this script's to check, not bash's to report as a job.
  disown "${victim}"
  if [ "${victim}" != "${target}" ]; then
    fail "could not hand pid ${target} to a new process (got ${victim}); case proves nothing"
    return 1
  fi
}

state_of() {
  local stat
  { IFS= read -r stat </proc/"$1"/stat; } 2>/dev/null || return 0
  # shellcheck disable=SC2086  # split into stat(5) fields after the parenthesised comm
  set -- ${stat##*) }
  printf '%s' "$1"
}

# The victim and its child must come through the reap untouched: running, not stopped.
check_victim() {
  local case="$1" child
  child="$(ps -o pid= --ppid "${victim}" | tr -d '[:space:]' || true)"
  case "$(state_of "${victim}")" in
    '' | Z*) fail "${case}: the process reusing pid ${victim} was killed" ;;
    T*) fail "${case}: the process reusing pid ${victim} was left stopped" ;;
  esac
  case "${child:+$(state_of "${child}")}" in
    '' | Z*) fail "${case}: the child of the process reusing pid ${victim} was killed" ;;
    T*) fail "${case}: the child of the process reusing pid ${victim} was left stopped" ;;
  esac
  kill -KILL "${victim}" ${child:+"${child}"} >/dev/null 2>&1 || true
}

reap() {
  local status=0
  "$(dirname "${script}")/reap_daemons.sh" >"${dir}/reap.log" 2>&1 || status=$?
  [ "${status}" -eq 0 ] && return 0
  fail "$1: reap_daemons.sh exited ${status}"
  cat "${dir}/reap.log" >&2
  return 1
}

# A pane that dies during the grace period, its pid reused before the second freeze.
start_daemon
reap "pane reused" &
reaper=$!
if take_pid "${pane}"; then
  wait "${reaper}" || failed=1
  check_victim "pane reused"
else
  wait "${reaper}" || true
fi
wait "${daemon}" 2>/dev/null || true

# A daemon that exits on TERM, its pid reused before the reaper's survivor pass.
start_daemon
reap "daemon reused" &
reaper=$!
if wait "${daemon}"; take_pid "${daemon}"; then
  wait "${reaper}" || failed=1
  check_victim "daemon reused"
else
  wait "${reaper}" || true
fi

# `terminate_pids` on a pid that TERM ended and another process took during its grace.
sleep 300 &
gone=$!
settle
terminate_pids "${gone}" &
terminator=$!
if wait "${gone}"; take_pid "${gone}"; then
  wait "${terminator}"
  check_victim "terminate_pids"
else
  wait "${terminator}" || true
fi

[ "${failed}" -eq 0 ] || exit 1
echo "reap pid-reuse self-test passed"
