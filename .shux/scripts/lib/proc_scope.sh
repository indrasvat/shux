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

# `pwd -P`, not `pwd`: the daemon's argv comes from `current_exe()`, which is always the
# PHYSICAL path, so a logical REPO_ROOT taken through a symlink can never prefix-match it.
# Reached through a symlinked checkout — routine on macOS, where `/tmp` is `/private/tmp`
# — every daemon of this very checkout went unseen.
: "${REPO_ROOT:="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"}"

# `ps` is the one external tool these helpers cannot do without. A guard whose tool is
# missing must say so and exit non-zero, never report clean for work it did not do.
if ! command -v ps >/dev/null 2>&1; then
  echo "⚠ leak guard: \`ps\` is not on PATH, so no process can be attributed." >&2
  exit 3
fi

# A process's working directory, or empty when it cannot be determined.
#
# `/proc` first (Linux, no external tool — CI runners do not necessarily install `lsof`),
# then `lsof` (macOS, where it is always present).
pid_cwd() {
  local pid="$1" cwd=""
  if [ -r "/proc/${pid}/cwd" ]; then
    cwd="$(readlink "/proc/${pid}/cwd" 2>/dev/null || true)"
  elif command -v lsof >/dev/null 2>&1; then
    cwd="$(lsof -a -p "${pid}" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -1)"
  fi
  printf '%s' "${cwd}"
}

# True when PID's working directory is inside this repository.
#
# FAILS CLOSED. If the cwd cannot be determined at all — no `/proc`, no `lsof` — the pid is
# treated as a candidate rather than skipped. Skipping made the guard emit NOTHING on such a
# host, so every leak passed silently (codex review of PR #95): a guard that cannot attribute
# must over-report, never under-report. A one-shot warning says why.
pid_cwd_in_repo() {
  local pid="$1" cwd
  cwd="$(pid_cwd "${pid}")"
  if [ -z "${cwd}" ]; then
    if [ ! -r "/proc/self/cwd" ] && ! command -v lsof >/dev/null 2>&1; then
      if [ -z "${_PROC_SCOPE_WARNED:-}" ]; then
        echo "⚠ leak guard: neither /proc nor lsof is available; cannot attribute processes" >&2
        echo "  to this repository, so ALL orphan candidates are reported (fail closed)." >&2
        _PROC_SCOPE_WARNED=1
      fi
      return 0
    fi
    # The tool exists but told us nothing — the process probably exited between listing
    # and probing. Not ours to report.
    return 1
  fi
  case "${cwd}" in
    "${REPO_ROOT}" | "${REPO_ROOT}"/*) return 0 ;;
    *) return 1 ;;
  esac
}

# Leaked shux DAEMONS belonging to this repository.
#
# Only a daemon can leak — a CLIENT invocation is transient and exits on its own, so it
# is never anyone's leak — and only one running this checkout's binary is our business.
#
# `__daemon` is matched in the SUBCOMMAND slot, not anywhere in the argv. As a substring
# it also matched live clients (`shux events watch --filter __daemon`), which
# `reap_daemons.sh` would then TERM+KILL. Everything before ` __daemon` must be this
# checkout's binary; a client that merely mentions the word leaves its own subcommands
# in there and stops matching.
#
# `ps`, not `pgrep`: `pgrep -x shux 2>/dev/null || true` cannot tell "no daemons" from
# "no pgrep on this host", and answered "clean" to both.
#
# A zombie is excluded for free — its argv reads `[shux] <defunct>` — which is right:
# it holds no socket, no PTY and no runtime dir.
shux_daemon_pids() {
  ps -axo pid=,args= |
    while read -r pid args; do
      case "${args}" in
        *" __daemon"*) ;;
        *) continue ;;
      esac
      case "${args%% __daemon*}" in
        "${REPO_ROOT}"/*/shux | "${REPO_ROOT}"/shux) printf '%s\n' "${pid}" ;;
      esac
    done
}

# Orphaned automation processes (PPID 1) that belong to this repository.
#
# `ps -o comm=` prints a PATH on macOS, so matching it against bare names like `python3`
# never fired — that branch was dead and the guard was weaker than it read. Compare the
# BASENAME, and require the working directory to be inside this repo so a concurrent
# session in another checkout is never a candidate.
#
# `Xvfb`/`kitty` are here for the GUI-terminal rig (issue #175), the first automation in
# this tree to own processes that are neither a shell nor a shux binary. They were
# invisible: measured, an orphaned Xvfb with its cwd inside the repo and a tty of `?`
# matched neither branch, so `no_leak_guard.sh` reported success while the X server ran
# on — and each leaked server holds a display number for good.
orphan_candidate_pids() {
  ps -axo pid=,ppid=,tty=,comm= |
    awk '
      $2 == 1 {
        n = split($4, parts, "/")
        base = parts[n]
        if ($3 ~ /^(ttys|pts\/)/ || base ~ /^(sh|bash|zsh|fish|sleep|yes|python|python[0-9.]*|node|cargo|shux|Xvfb|kitty)$/) {
          print $1
        }
      }
    ' |
    while read -r pid; do
      if pid_cwd_in_repo "${pid}"; then printf '%s\n' "${pid}"; fi
    done
}

# One-line `ps` description of a pid, or nothing when it has already exited.
describe_pid() {
  local pid="$1"
  ps -p "${pid}" -o pid=,ppid=,stat=,args= 2>/dev/null || true
}

# True when a pid is no longer a running process. A zombie counts as gone: it is waiting
# to be reaped and owns nothing. `kill -0` alone cannot tell the two apart, and under an
# init that does not reap promptly it calls a dead daemon alive.
pid_is_gone() {
  local pid="$1" state
  kill -0 "${pid}" >/dev/null 2>&1 || return 0
  state="$(ps -p "${pid}" -o stat= 2>/dev/null | tr -d '[:space:]')"
  case "${state}" in '' | Z*) return 0 ;; *) return 1 ;; esac
}

# Whether a pid appears in a list of pids.
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

# TERM, then KILL what survives. Callers must have attributed every pid first —
# this helper does no scoping of its own.
terminate_pids() {
  local pid waited
  for pid in "$@"; do
    kill -TERM "${pid}" >/dev/null 2>&1 || true
  done
  sleep 1
  for pid in "$@"; do
    if kill -0 "${pid}" >/dev/null 2>&1; then
      kill -KILL "${pid}" >/dev/null 2>&1 || true
    fi
  done
  # KILL is asynchronous. Return before the kernel has torn the process down and a
  # survivor scan on the caller's next line still sees it, with its full argv — a
  # successful kill reported as "survived TERM+KILL".
  waited=0
  while [ "${waited}" -lt 50 ]; do
    for pid in "$@"; do
      pid_is_gone "${pid}" || { waited=$((waited + 1)); sleep 0.1; continue 2; }
    done
    return 0
  done
}
