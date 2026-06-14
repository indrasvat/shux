#!/usr/bin/env bash
# Shared cleanup helpers for shux automation that uses an isolated
# XDG_RUNTIME_DIR. These helpers intentionally avoid broad pkill patterns:
# they only touch the daemon PID recorded inside the supplied runtime dir.

shux_harness_timeout() {
  local duration="$1"
  shift
  if [ "${SHUX_HARNESS_TIMEOUT_IMPL:-auto}" = "bash" ]; then
    shux_harness_bash_timeout "${duration}" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout -k 2s "${duration}" "$@"
  elif command -v timeout >/dev/null 2>&1; then
    timeout -k 2s "${duration}" "$@"
  else
    shux_harness_bash_timeout "${duration}" "$@"
  fi
}

shux_harness_duration_seconds() {
  local duration="$1"
  case "${duration}" in
    *s) printf '%s\n' "${duration%s}" ;;
    *m) awk "BEGIN { print (${duration%m}) * 60 }" ;;
    *h) awk "BEGIN { print (${duration%h}) * 3600 }" ;;
    *) printf '%s\n' "${duration}" ;;
  esac
}

shux_harness_bash_timeout() {
  local duration="$1"
  shift
  local seconds pid timer status errexit_was_set=0
  seconds="$(shux_harness_duration_seconds "${duration}")"

  case "$-" in
    *e*) errexit_was_set=1 ;;
  esac

  "$@" &
  pid=$!

  (
    sleep "${seconds}"
    if kill -0 "${pid}" >/dev/null 2>&1; then
      kill -TERM "${pid}" >/dev/null 2>&1 || true
      sleep 2
      kill -KILL "${pid}" >/dev/null 2>&1 || true
    fi
  ) &
  timer=$!

  set +e
  wait "${pid}"
  status=$?
  set -e

  pkill -TERM -P "${timer}" >/dev/null 2>&1 || true
  kill "${timer}" >/dev/null 2>&1 || true
  wait "${timer}" 2>/dev/null || true

  if [ "${status}" -eq 143 ] || [ "${status}" -eq 137 ]; then
    status=124
  fi

  if [ "${errexit_was_set}" -eq 1 ]; then
    set -e
  else
    set +e
  fi
  return "${status}"
}

shux_harness_pid_file() {
  local runtime="$1"
  printf '%s/shux/shux.pid\n' "${runtime}"
}

shux_harness_daemon_pid() {
  local runtime="$1"
  local pid_file
  pid_file="$(shux_harness_pid_file "${runtime}")"
  if [ -f "${pid_file}" ]; then
    head -n 1 "${pid_file}" | tr -cd '0-9'
  fi
}

shux_harness_kill_session() {
  local runtime="$1"
  local shux_bin="$2"
  local session="$3"
  if [ -z "${session}" ]; then
    return 0
  fi

  local sock="${runtime}/shux/shux.sock"
  local pid_file
  pid_file="$(shux_harness_pid_file "${runtime}")"
  if [ ! -S "${sock}" ] && [ ! -f "${pid_file}" ]; then
    return 0
  fi

  shux_harness_timeout 2s \
    env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" \
      session kill "${session}" >/dev/null 2>&1 || true
}

shux_harness_kill_plugin() {
  local runtime="$1"
  local shux_bin="$2"
  local plugin="$3"
  if [ -z "${plugin}" ]; then
    return 0
  fi

  local sock="${runtime}/shux/shux.sock"
  local pid_file
  pid_file="$(shux_harness_pid_file "${runtime}")"
  if [ ! -S "${sock}" ] && [ ! -f "${pid_file}" ]; then
    return 0
  fi

  shux_harness_timeout 2s \
    env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" \
      plugin kill "${plugin}" >/dev/null 2>&1 || true
}

shux_harness_stop_daemon() {
  local runtime="$1"
  local pid
  pid="$(shux_harness_daemon_pid "${runtime}")"
  if [ -z "${pid}" ]; then
    return 0
  fi

  if ! kill -0 "${pid}" >/dev/null 2>&1; then
    rm -f "$(shux_harness_pid_file "${runtime}")" "${runtime}/shux/shux.sock" "${runtime}/shux/attach.sock" 2>/dev/null || true
    return 0
  fi

  kill -TERM "${pid}" >/dev/null 2>&1 || true
  local i
  for i in $(seq 1 50); do
    if ! kill -0 "${pid}" >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done

  if kill -0 "${pid}" >/dev/null 2>&1; then
    kill -KILL "${pid}" >/dev/null 2>&1 || true
  fi

  for i in $(seq 1 20); do
    if ! kill -0 "${pid}" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done

  if kill -0 "${pid}" >/dev/null 2>&1; then
    printf 'shux daemon did not exit after cleanup: pid=%s runtime=%s\n' "${pid}" "${runtime}" >&2
    return 1
  fi
}

shux_harness_cleanup_runtime() {
  local runtime="$1"
  local shux_bin="$2"
  shift 2

  local session
  for session in "$@"; do
    shux_harness_kill_session "${runtime}" "${shux_bin}" "${session}"
  done

  shux_harness_stop_daemon "${runtime}"
  rm -rf "${runtime}"
}

shux_harness_assert_no_daemon() {
  local runtime="$1"
  local pid
  pid="$(shux_harness_daemon_pid "${runtime}")"
  if [ -n "${pid}" ] && kill -0 "${pid}" >/dev/null 2>&1; then
    printf 'shux daemon still running after cleanup: pid=%s runtime=%s\n' "${pid}" "${runtime}" >&2
    return 1
  fi
}
