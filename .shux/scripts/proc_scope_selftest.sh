#!/usr/bin/env bash
# The leak guards' process scoping claims this checkout's daemons and orphans,
# and nothing that belongs to a checkout nested inside it (an agent's git
# worktree under .claude/worktrees/): `make reap` there would kill another
# run's daemon, and the guard would blame its own command for the leak.
#
# Real processes, real `ps`: each stand-in is `sleep` with a daemon's argv.

set -euo pipefail

root="$(mktemp -d "${TMPDIR:-/tmp}/shux-proc-scope.XXXXXX")"
root="$(cd "${root}" && pwd -P)"
pids=()
cleanup() {
  local pid
  for pid in "${pids[@]}"; do kill -KILL "${pid}" >/dev/null 2>&1 || true; done
  rm -rf "${root}"
}
trap cleanup EXIT

mkdir -p "${root}/target/debug" "${root}/.claude/worktrees/agent/target/debug"
touch "${root}/.claude/worktrees/agent/.git"

# shellcheck disable=SC2034  # consumed by lib/proc_scope.sh
REPO_ROOT="${root}"
# shellcheck source=lib/proc_scope.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib/proc_scope.sh"

# A parked process whose argv reads `<bin> __daemon …`, with `cwd` as its cwd.
# Its pid lands in `pids`; no command substitution, which would wait on it.
standin() {
  local bin="$1" cwd="$2"
  (cd "${cwd}" && exec -a "${bin} __daemon" sleep 600) </dev/null >/dev/null 2>&1 &
  pids+=("$!")
  disown "$!"
}
standin "${root}/target/debug/shux" "${root}/target"
standin "${root}/.claude/worktrees/agent/target/debug/shux" \
  "${root}/.claude/worktrees/agent/target"
ours="${pids[0]}"
nested="${pids[1]}"

# `exec -a` takes effect once the subshell has exec'd; wait on the argv itself.
deadline=$((SECONDS + 10))
until ps -p "${ours}" -o args= | grep -q " __daemon" &&
  ps -p "${nested}" -o args= | grep -q " __daemon"; do
  if [ "${SECONDS}" -ge "${deadline}" ]; then
    echo "proc_scope selftest: the stand-ins never took their daemon argv" >&2
    exit 1
  fi
done

fail=0
claimed="$(shux_daemon_pids)"
if ! grep -qx "${ours}" <<<"${claimed}"; then
  echo "FAIL: this checkout's daemon ${ours} is not claimed" >&2
  fail=1
fi
if grep -qx "${nested}" <<<"${claimed}"; then
  echo "FAIL: a nested worktree's daemon ${nested} is claimed as this checkout's" >&2
  fail=1
fi
if ! pid_cwd_in_repo "${ours}"; then
  echo "FAIL: a process working in ${root}/target is not in this checkout" >&2
  fail=1
fi
if pid_cwd_in_repo "${nested}"; then
  echo "FAIL: a process working in a nested worktree is counted as this checkout's" >&2
  fail=1
fi
[ "${fail}" -eq 0 ] || exit 1
echo "✓ proc_scope claims this checkout's processes and no nested worktree's"
