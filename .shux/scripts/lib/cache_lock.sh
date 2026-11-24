# shellcheck shell=bash
# Pid liveness and the vt-neutrality cache lock, shared by scripts/vt-neutrality.sh
# and scripts/prune.sh. Sourced; defines functions only.

# pid_state <pid>: prints alive, dead, or unknown when ps cannot say.
pid_state() {
  local out rc=0
  out="$(ps -p "$1" -o pid= 2>&1)" || rc=$?
  if [[ "${out//[[:space:]]/}" == "$1" ]]; then
    echo alive
  elif [[ "${rc}" -eq 1 && -z "${out//[[:space:]]/}" ]]; then
    echo dead
  else
    echo unknown
  fi
}

# pidfile_pid <file>: the pid a pidfile names, in PIDFILE_PID. Fails (status 2)
# when it is unreadable, and (3) when it is not exactly one line of digits.
# An empty file sets PIDFILE_PID="" and succeeds.
pidfile_pid() {
  local c
  PIDFILE_PID=""
  [[ -f "$1" && -r "$1" ]] || return 2
  c="$(cat -- "$1" 2>/dev/null && printf x)" || return 2
  c="${c%x}"
  c="${c%$'\n'}"
  [[ -z "${c}" || "${c}" =~ ^[0-9]+$ ]] || return 3
  PIDFILE_PID="${c}"
}

# cache_lock_break <lock dir> <dead pid>: remove the lock only if it is still the
# one that dead pid holds. The rename is the claim: of two callers that both saw
# the dead holder, the second renames whatever lock is there now, sees it is not
# the dead one's, and puts it back (rename(2), which refuses a non-empty target).
cache_lock_break() {
  local lock="$1" dead="$2" claimed="$1.stale.$$.${RANDOM}"
  mv "${lock}" "${claimed}" 2>/dev/null || return 1
  if pidfile_pid "${claimed}/pid" && [[ "${PIDFILE_PID}" == "${dead}" ]]; then
    rm -f "${claimed}/pid"
    rmdir "${claimed}" 2>/dev/null || echo "⚠ cache lock: could not remove ${claimed}" >&2
    return 0
  fi
  python3 -c 'import os, sys; os.rename(sys.argv[1], sys.argv[2])' "${claimed}" "${lock}" ||
    echo "⚠ cache lock: moved a live lock to ${claimed} and could not put it back" >&2
  return 1
}

# cache_lock_take <lock dir> <tries>: mkdir the lock with our pid in it, trying
# once a second. A lock whose recorded pid is dead was left by a killed run and
# is broken (cache_lock_break).
cache_lock_take() {
  local lock="$1" tries="$2" n=0
  until mkdir "${lock}" 2>/dev/null; do
    if pidfile_pid "${lock}/pid" && [[ -n "${PIDFILE_PID}" ]] &&
      [[ "$(pid_state "${PIDFILE_PID}")" == dead ]] && cache_lock_break "${lock}" "${PIDFILE_PID}"; then
      continue
    fi
    n=$((n + 1))
    [[ "${n}" -lt "${tries}" ]] || return 1
    sleep 1
  done
  echo "$$" >"${lock}/pid"
}

cache_lock_release() { rm -f "$1/pid" && rmdir "$1"; }
