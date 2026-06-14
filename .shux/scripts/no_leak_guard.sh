#!/usr/bin/env bash
# Run a command and hard-fail if it leaves behind any new `shux` process or
# orphaned automation child process. The orphan check protects the macOS PTY
# pool and catches detached test fixtures without sweeping up unrelated OS
# workers that may appear during a run.
#
# This protects automation from leaking daemons while avoiding destructive
# broad cleanup: PIDs that existed before the command are never killed.

set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: .shux/scripts/no_leak_guard.sh <command> [args...]" >&2
  exit 2
fi

shux_pids() {
  pgrep -x shux 2>/dev/null || true
}

orphan_candidate_pids() {
  ps -axo pid=,ppid=,tty=,comm=,args= |
    awk '
      $2 == 1 && ($3 ~ /^(ttys|pts\/)/ || $4 ~ /^(sh|bash|zsh|fish|sleep|yes|python|python3|node|cargo|shux)$/) {
        print $1
      }
    '
}

pid_in_list() {
  local needle="$1"
  local pid
  shift
  for pid in "$@"; do
    if [ "${pid}" = "${needle}" ]; then
      return 0
    fi
  done
  return 1
}

describe_pid() {
  local pid="$1"
  ps -p "${pid}" -o pid=,ppid=,stat=,args= 2>/dev/null || true
}

kill_new_pids() {
  local pid
  for pid in "$@"; do
    kill -TERM "${pid}" >/dev/null 2>&1 || true
  done
  sleep 1
  for pid in "$@"; do
    if kill -0 "${pid}" >/dev/null 2>&1; then
      kill -KILL "${pid}" >/dev/null 2>&1 || true
    fi
  done
}

baseline_shux=($(shux_pids))
baseline_orphans=($(orphan_candidate_pids))

set +e
"$@"
cmd_status=$?
set -e

after_shux=($(shux_pids))
new_shux_pids=()
set +u
for pid in "${after_shux[@]}"; do
  [ -n "${pid}" ] || continue
  if ! pid_in_list "${pid}" "${baseline_shux[@]:-}"; then
    new_shux_pids+=("${pid}")
  fi
done
set -u

if [ "${#new_shux_pids[@]}" -gt 0 ]; then
  echo "shux leak guard: command left new shux process(es): ${new_shux_pids[*]}" >&2
  for pid in "${new_shux_pids[@]}"; do
    describe_pid "${pid}" >&2
  done
  kill_new_pids "${new_shux_pids[@]}"
fi

# Re-scan after daemon cleanup: killing a daemon must not strand pane commands.
after_orphans=($(orphan_candidate_pids))
new_orphan_pids=()
set +u
for pid in "${after_orphans[@]}"; do
  [ -n "${pid}" ] || continue
  if ! pid_in_list "${pid}" "${baseline_orphans[@]:-}"; then
    new_orphan_pids+=("${pid}")
  fi
done
set -u

if [ "${#new_orphan_pids[@]}" -gt 0 ]; then
  echo "shux leak guard: command left new orphan automation process(es): ${new_orphan_pids[*]}" >&2
  for pid in "${new_orphan_pids[@]}"; do
    describe_pid "${pid}" >&2
  done
  kill_new_pids "${new_orphan_pids[@]}"
fi

if [ "${#new_shux_pids[@]}" -gt 0 ] || [ "${#new_orphan_pids[@]}" -gt 0 ]; then
  exit 1
fi

exit "${cmd_status}"
