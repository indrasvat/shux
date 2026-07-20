#!/usr/bin/env bash
# Shared process-scoping helpers for the leak guards (085 F8).
#
# These live in ONE place on purpose. The rule was originally written twice — in
# `no_leak_guard.sh` and again in `leak_guard_selftest.sh` — and when the guard was
# hardened only one copy was updated. The stale copy kept a bare machine-wide
# `pgrep -x shux` followed by `kill -TERM`/`kill -KILL`, and it SIGKILLed another
# session's in-flight `shux lens gate` during this very task. Duplicated kill logic
# diverges; a shared helper cannot.
#
# Callers must set REPO_ROOT before sourcing, or it is derived from this file.

: "${REPO_ROOT:="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"}"

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

# Leaked shux DAEMONS belonging to this repository.
#
# Only a daemon can leak — a CLIENT invocation is transient and exits on its own, so it
# is never anyone's leak — and only one running this checkout's binary is our business.
shux_daemon_pids() {
  local pid args
  for pid in $(pgrep -x shux 2>/dev/null || true); do
    # `ps -o args=` pads with leading whitespace; strip it or the prefix match never fires.
    args="$(ps -p "${pid}" -o args= 2>/dev/null | sed 's/^[[:space:]]*//' || true)"
    case "${args}" in
      *"__daemon"*) ;;
      *) continue ;;
    esac
    case "${args}" in
      "${REPO_ROOT}"/*) printf '%s\n' "${pid}" ;;
      *) ;;
    esac
  done
}

# Orphaned automation processes (PPID 1) that belong to this repository.
#
# `ps -o comm=` prints a PATH on macOS, so matching it against bare names like `python3`
# never fired — that branch was dead and the guard was weaker than it read. Compare the
# BASENAME, and require the working directory to be inside this repo so a concurrent
# session in another checkout is never a candidate.
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
