#!/usr/bin/env bash
# Stop shux daemons left behind by an interrupted run (issue #179).
#
# Scope is exactly `shux_daemon_pids` — a `__daemon` whose argv starts with THIS
# checkout's path. A daemon from another checkout, or any client invocation, is never a
# candidate. A daemon of yours that is still in use IS one, which is why this is
# developer-invoked and never automatic: run it when no session of yours is working.

set -euo pipefail

usage() { echo "usage: .shux/scripts/reap_daemons.sh [--dry-run]" >&2; exit 2; }

# Count the arguments rather than matching `''`, which cannot tell "no argument" from an
# empty one — `reap_daemons.sh ""` fell through to a full destructive reap.
dry_run=0
case "$#" in
  0) ;;
  1) [ "$1" = "--dry-run" ] || usage; dry_run=1 ;;
  *) usage ;;
esac

# shellcheck disable=SC2034  # consumed by lib/proc_scope.sh
# `pwd -P`: a daemon's argv is always the physical path (see lib/proc_scope.sh).
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# shellcheck source=lib/proc_scope.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib/proc_scope.sh"

# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
pids=($(shux_daemon_pids))

# When a pid started, which with the pid names one process: pids are reused.
start_time() {
  ps -p "$1" -o lstart= 2>/dev/null | tr -s ' ' || true
}

# The live children of any pid in a space-padded set, not already in it.
children_outside() {
  ps -axo pid=,ppid= | awk -v set="$1" 'index(set, " " $2 " ") && !index(set, " " $1 " ") { print $1 }'
}

if [ "${#pids[@]}" -eq 0 ]; then
  echo "no shux daemons from ${REPO_ROOT} are running"
  exit 0
fi

for pid in "${pids[@]}"; do
  describe_pid "${pid}"
done

if [ "${dry_run}" -eq 1 ]; then
  echo "--dry-run: ${#pids[@]} daemon(s) would be stopped"
  exit 0
fi

# A daemon too wedged to service SIGTERM is SIGKILLed, and SIGKILL runs none of its pane
# teardown: its pane children are reparented to init, holding the PTYs the daemon held.
# Reaping without this turns one leaked daemon into several leaked shells and calls it
# success — manufacturing precisely what `no_leak_guard.sh` exists to catch.
#
# So every descendant is frozen BEFORE any daemon is signalled, while the daemon is still
# the root of their tree. Attribution is LINEAGE, not timing: anything else in this
# checkout that orphans a process during the reap looks identical to a stranded pane, and
# shux auto-starts a daemon on first use. Frozen, a process can neither fork a child past
# the walk nor exit and hand its children to init; stopped parents keep their children's
# ppid pointing at them, so the walk reaches a fixpoint. The daemons themselves keep
# running, so a responsive one still shuts down gracefully.
members=" ${pids[*]} "
frozen=()
frozen_at=()
resume_frozen() {
  local pid
  for pid in "${frozen[@]:-}"; do
    [ -n "${pid}" ] && kill -CONT "${pid}" >/dev/null 2>&1 || true
  done
}
# Interrupted, the reap leaves what it froze running again rather than stopped for good.
trap 'resume_frozen; exit 130' INT TERM HUP
while :; do
  # shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
  pending=($(children_outside "${members}"))
  [ "${#pending[@]}" -gt 0 ] || break
  for pid in "${pending[@]}"; do
    kill -STOP "${pid}" >/dev/null 2>&1 || true
    members="${members}${pid} "
    frozen+=("${pid}")
    frozen_at+=("$(start_time "${pid}")")
  done
done

terminate_pids "${pids[@]}"

# Report on what is actually gone, and only about the pids we targeted. Re-scanning the
# machine instead counted a daemon that STARTED during the reap — shux auto-starts one on
# first use, so any concurrent command in this checkout did it — as one that "survived
# TERM+KILL".
survivors=()
for pid in "${pids[@]}"; do
  pid_is_gone "${pid}" || survivors+=("${pid}")
done
if [ "${#survivors[@]}" -gt 0 ]; then
  echo "reap: ${#survivors[@]} daemon(s) survived TERM+KILL: ${survivors[*]}" >&2
  for pid in "${survivors[@]}"; do
    describe_pid "${pid}" >&2
  done
  resume_frozen
  exit 1
fi

# Each still has the start time it had when frozen, so it is that process and not one
# reusing its pid; a process it would be is frozen, so cannot have been replaced.
stranded=()
i=0
while [ "${i}" -lt "${#frozen[@]}" ]; do
  pid="${frozen[${i}]}"
  if ! pid_is_gone "${pid}" && [ "$(start_time "${pid}")" = "${frozen_at[${i}]}" ]; then
    stranded+=("${pid}")
  fi
  i=$((i + 1))
done

if [ "${#stranded[@]}" -gt 0 ]; then
  echo "reap: stopping ${#stranded[@]} process(es) stranded by the reaped daemon(s)"
  for pid in "${stranded[@]}"; do
    describe_pid "${pid}"
  done
  terminate_pids "${stranded[@]}"
  left=()
  for pid in "${stranded[@]}"; do
    pid_is_gone "${pid}" || left+=("${pid}")
  done
  if [ "${#left[@]}" -gt 0 ]; then
    echo "reap: ${#left[@]} stranded process(es) survived KILL: ${left[*]}" >&2
    exit 1
  fi
fi
trap - INT TERM HUP

echo "reaped ${#pids[@]} shux daemon(s)"
