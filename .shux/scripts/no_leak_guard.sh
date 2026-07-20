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

# The repository this guard is protecting (085 F8). Every orphan candidate must belong to
# it: this guard runs alongside other repositories' agents, councils and test suites, and
# reaping one of those costs someone else real work.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# True when PID's working directory is inside this repository.
pid_cwd_in_repo() {
  local pid="$1" cwd
  cwd="$(lsof -a -p "${pid}" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -1)"
  [ -n "${cwd}" ] || return 1
  case "${cwd}" in
    "${REPO_ROOT}" | "${REPO_ROOT}"/*) return 0 ;;
    *) return 1 ;;
  esac
}

# Orphaned automation processes (PPID 1) that this run is responsible for.
#
# Two corrections over the original rule (085 F8):
#   * `ps -o comm=` prints a PATH on macOS (`/opt/homebrew/.../python3.13`), so matching it
#     against bare names like `python3` never fired — that whole branch was dead, and only
#     the tty test was doing any work. Compare the BASENAME instead.
#   * Neither test said anything about WHOSE process it is. Require the working directory to
#     be inside this repo, so a concurrent session in another checkout is never a candidate.
orphan_candidate_pids() {
  ps -axo pid=,ppid=,tty=,comm= |
    awk '
      $2 == 1 {
        n = split($4, parts, "/")
        base = parts[n]
        if ($3 ~ /^(ttys|pts\/)/ || base ~ /^(sh|bash|zsh|fish|sleep|yes|python|python[0-9.]*|node|cargo|shux)$/) {
          print $1
        }
      }
    ' |
    while read -r pid; do
      if pid_cwd_in_repo "${pid}"; then printf '%s\n' "${pid}"; fi
    done
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

# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
baseline_shux=($(shux_pids))
# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
baseline_orphans=($(orphan_candidate_pids))

set +e
"$@"
cmd_status=$?
set -e

# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
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
# shellcheck disable=SC2207  # mapfile is bash 4+; macOS ships bash 3.2
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
