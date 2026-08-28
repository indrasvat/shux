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
# Taken before anything is signalled — see the stranded-children sweep below.
# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
baseline_orphans=($(orphan_candidate_pids))

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
# success — manufacturing precisely what `no_leak_guard.sh` exists to catch. New since
# the snapshot above, so they are attributable to the reap and to nothing else.
# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
after_orphans=($(orphan_candidate_pids))
# A DAEMON that appeared meanwhile is not stranded by us: shux auto-starts one on first
# use, so a concurrent command in this checkout mints a PPID-1 shux process with its cwd
# in the repo — an orphan candidate by every test the sweep applies. Measured: without
# this the sweep killed that daemon, which is the one thing these guards must never do.
# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
live_daemons=($(shux_daemon_pids))
stranded=()
set +u
for pid in "${after_orphans[@]}"; do
  [ -n "${pid}" ] || continue
  pid_in_list "${pid}" "${baseline_orphans[@]:-}" && continue
  pid_in_list "${pid}" "${live_daemons[@]:-}" && continue
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
