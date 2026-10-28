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
daemon_at=()
for pid in "${pids[@]:-}"; do
  [ -z "${pid}" ] || daemon_at+=("$(start_time "${pid}")")
done

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
frozen=()
frozen_at=()
resume_frozen() {
  local i=0
  while [ "${i}" -lt "${#frozen[@]}" ]; do
    if is_process "${frozen[${i}]}" "${frozen_at[${i}]}"; then
      kill -CONT "${frozen[${i}]}" >/dev/null 2>&1 || true
    fi
    i=$((i + 1))
  done
}
# Interrupted, the reap leaves what it froze running again rather than stopped for good.
trap 'resume_frozen; exit 130' INT TERM HUP
# Re-stopping what is already frozen matters: see the SIGCONT note below. The tree is
# rebuilt from the processes that are still the ones recorded, so a pid that died and was
# reused names neither a process to stop nor a parent whose children are ours.
freeze_tree() {
  local pid i=0
  members=" "
  while [ "${i}" -lt "${#pids[@]}" ]; do
    if is_process "${pids[${i}]}" "${daemon_at[${i}]}"; then members="${members}${pids[${i}]} "; fi
    i=$((i + 1))
  done
  i=0
  while [ "${i}" -lt "${#frozen[@]}" ]; do
    pid="${frozen[${i}]}"
    if is_process "${pid}" "${frozen_at[${i}]}"; then
      kill -STOP "${pid}" >/dev/null 2>&1 || true
      members="${members}${pid} "
    fi
    i=$((i + 1))
  done
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
}
freeze_tree

for pid in "${pids[@]}"; do
  kill -TERM "${pid}" >/dev/null 2>&1 || true
done
sleep 1

# Closing a pane's last PTY master hangs the terminal up, and the kernel then sends its
# session leader SIGCONT: the freeze is undone. A daemon's graceful teardown closes its
# masters, and SIGKILL closes them too. So the tree is frozen again here, and what a
# daemon still running after the grace period strands is killed BEFORE that daemon, while
# it is frozen and still has a terminal that has not hung up.
freeze_tree

stranded=()
stranded_at=()
i=0
while [ "${i}" -lt "${#frozen[@]}" ]; do
  pid="${frozen[${i}]}"
  if is_process "${pid}" "${frozen_at[${i}]}" && ! pid_is_gone "${pid}"; then
    stranded+=("${pid}")
    stranded_at+=("${frozen_at[${i}]}")
  fi
  i=$((i + 1))
done

if [ "${#stranded[@]}" -gt 0 ]; then
  echo "reap: stopping ${#stranded[@]} process(es) stranded by the reaped daemon(s)"
  for pid in "${stranded[@]}"; do
    describe_pid "${pid}"
  done
  terminate_pids "${stranded[@]}"
fi

# Only the daemons that outlived their grace, by identity: a pid freed by a graceful exit
# may be another process's by now.
wedged=()
wedged_at=()
i=0
while [ "${i}" -lt "${#pids[@]}" ]; do
  if is_process "${pids[${i}]}" "${daemon_at[${i}]}" && ! pid_is_gone "${pids[${i}]}"; then
    wedged+=("${pids[${i}]}")
    wedged_at+=("${daemon_at[${i}]}")
  fi
  i=$((i + 1))
done
[ "${#wedged[@]}" -eq 0 ] || terminate_pids "${wedged[@]}"

# Report on what is actually gone, and only about the pids we targeted. Re-scanning the
# machine instead counted a daemon that STARTED during the reap — shux auto-starts one on
# first use, so any concurrent command in this checkout did it — as one that "survived
# TERM+KILL".
survivors=()
i=0
while [ "${i}" -lt "${#wedged[@]}" ]; do
  if is_process "${wedged[${i}]}" "${wedged_at[${i}]}" && ! pid_is_gone "${wedged[${i}]}"; then
    survivors+=("${wedged[${i}]}")
  fi
  i=$((i + 1))
done
if [ "${#survivors[@]}" -gt 0 ]; then
  echo "reap: ${#survivors[@]} daemon(s) survived TERM+KILL: ${survivors[*]}" >&2
  for pid in "${survivors[@]}"; do
    describe_pid "${pid}" >&2
  done
  resume_frozen
  exit 1
fi

left=()
i=0
while [ "${i}" -lt "${#stranded[@]}" ]; do
  if is_process "${stranded[${i}]}" "${stranded_at[${i}]}" && ! pid_is_gone "${stranded[${i}]}"; then
    left+=("${stranded[${i}]}")
  fi
  i=$((i + 1))
done
if [ "${#left[@]}" -gt 0 ]; then
  echo "reap: ${#left[@]} stranded process(es) survived KILL: ${left[*]}" >&2
  exit 1
fi
trap - INT TERM HUP

echo "reaped ${#pids[@]} shux daemon(s)"
