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

# Children of a pid, listed while it is still their parent.
child_pids() {
  ps -axo pid=,ppid= | awk -v parent="$1" '$2 == parent { print $1 }'
}

if [ "${#pids[@]}" -eq 0 ]; then
  echo "no shux daemons from ${REPO_ROOT} are running"
  exit 0
fi

for pid in "${pids[@]}"; do
  describe_pid "${pid}"
done

# Recorded before anything is signalled: SIGKILL destroys the only evidence of who owned
# these. Once the daemon is gone its panes read as PPID 1, with nothing connecting them
# to it. See the sweep below.
daemon_children=()
for pid in "${pids[@]}"; do
  for child in $(child_pids "${pid}"); do
    daemon_children+=("${child}")
  done
done

if [ "${dry_run}" -eq 1 ]; then
  echo "--dry-run: ${#pids[@]} daemon(s) would be stopped"
  exit 0
fi

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
  exit 1
fi

# A daemon too wedged to service SIGTERM is SIGKILLed, and SIGKILL runs none of its pane
# teardown: its pane children are reparented to init, holding the PTYs the daemon held.
# Reaping without this turns one leaked daemon into several leaked shells and calls it
# success — manufacturing precisely what `no_leak_guard.sh` exists to catch.
#
# Attribution is LINEAGE, not timing. "Appeared between two snapshots" is not ownership:
# anything else in this checkout that orphans a process during the reap window looks
# identical, and since shux auto-starts a daemon on first use, a concurrent command mints
# one that passes every test a timing sweep applies — measured, an earlier version of
# this killed exactly that. Each pid below was a child of a daemon we stopped, and is an
# orphan now because we stopped it.
stranded=()
set +u
for pid in "${daemon_children[@]:-}"; do
  [ -n "${pid}" ] || continue
  pid_is_gone "${pid}" && continue
  [ "$(ps -p "${pid}" -o ppid= 2>/dev/null | tr -d '[:space:]')" = "1" ] || continue
  stranded+=("${pid}")
done
set -u

if [ "${#stranded[@]}" -gt 0 ]; then
  echo "reap: stopping ${#stranded[@]} process(es) stranded by the reaped daemon(s)"
  for pid in "${stranded[@]}"; do
    describe_pid "${pid}"
  done
  terminate_pids "${stranded[@]}"
fi

echo "reaped ${#pids[@]} shux daemon(s)"
