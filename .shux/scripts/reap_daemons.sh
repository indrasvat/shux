#!/usr/bin/env bash
# Stop shux daemons left behind by an interrupted run (issue #179).
#
# Deliberate, never automatic: `no_leak_guard.sh` must not kill what it did not start,
# so orphans from a SIGKILLed run stay running and are exempt from every later check.
# This is the developer-invoked cleanup the guard's warning points at.
#
# Scope is exactly `shux_daemon_pids` — a `__daemon` whose argv starts with THIS
# checkout's path. A daemon from another checkout, or any client invocation, is never
# a candidate. A daemon of yours that is still in use IS, so run it when no session is.
#
# `--dry-run` lists without stopping.

set -euo pipefail

usage() { echo "usage: .shux/scripts/reap_daemons.sh [--dry-run]" >&2; exit 2; }

dry_run=0
[ "$#" -le 1 ] || usage
case "${1:-}" in
  --dry-run) dry_run=1 ;;
  '') ;;
  *) usage ;;
esac

# shellcheck disable=SC2034  # consumed by lib/proc_scope.sh
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=lib/proc_scope.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib/proc_scope.sh"

# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
pids=($(shux_daemon_pids))

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

# Report on what is actually gone, not on what was asked to go.
# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
survivors=($(shux_daemon_pids))
if [ "${#survivors[@]}" -gt 0 ]; then
  echo "reap: ${#survivors[@]} daemon(s) survived TERM+KILL: ${survivors[*]}" >&2
  exit 1
fi
echo "reaped ${#pids[@]} shux daemon(s)"
