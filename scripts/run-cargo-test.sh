#!/usr/bin/env bash
set -euo pipefail

binary_timeout_seconds="${SHUX_TEST_BINARY_TIMEOUT_SECONDS:-45}"
binary_retries="${SHUX_TEST_BINARY_RETRIES:-2}"

cargo_args=()
test_args=()
after_separator=0

for arg in "$@"; do
  if [[ "${after_separator}" -eq 0 && "${arg}" == "--" ]]; then
    after_separator=1
    continue
  fi

  if [[ "${after_separator}" -eq 0 ]]; then
    cargo_args+=("${arg}")
  else
    test_args+=("${arg}")
  fi
done

if [[ "${#cargo_args[@]}" -eq 0 ]]; then
  echo "usage: scripts/run-cargo-test.sh <cargo-test-selection-args...> [-- <libtest-args...>]" >&2
  exit 2
fi

if [[ "${#test_args[@]}" -eq 0 ]]; then
  test_args=(--test-threads=1)
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/shux-test-bins.XXXXXX")"
binary_list="${tmpdir}/binaries.txt"
build_log="${tmpdir}/build.jsonl"

cleanup() {
  rm -rf "${tmpdir}"
}
trap cleanup EXIT

kill_tree() {
  local pid="$1"
  local child

  while read -r child; do
    [[ -n "${child}" ]] || continue
    kill_tree "${child}"
  done < <(pgrep -P "${pid}" 2>/dev/null || true)

  kill -TERM "${pid}" 2>/dev/null || true
}

kill_tree_hard() {
  local pid="$1"
  local child

  while read -r child; do
    [[ -n "${child}" ]] || continue
    kill_tree_hard "${child}"
  done < <(pgrep -P "${pid}" 2>/dev/null || true)

  kill -KILL "${pid}" 2>/dev/null || true
}

run_with_timeout() {
  local binary="$1"

  "${binary}" "${test_args[@]}" &
  local pid=$!

  (
    sleep "${binary_timeout_seconds}"
    if kill -0 "${pid}" 2>/dev/null; then
      echo "error: ${binary} exceeded ${binary_timeout_seconds}s; terminating process tree" >&2
      kill_tree "${pid}"
      sleep 2
      kill_tree_hard "${pid}"
    fi
  ) &
  local watchdog_pid=$!

  set +e
  wait "${pid}"
  local status=$?
  set -e

  kill "${watchdog_pid}" 2>/dev/null || true
  wait "${watchdog_pid}" 2>/dev/null || true

  if [[ "${status}" -eq 143 || "${status}" -eq 137 ]]; then
    return 124
  fi

  return "${status}"
}

echo "▶ Building test binaries..."
cargo test "${cargo_args[@]}" --no-run --message-format=json >"${build_log}"

python3 - "${build_log}" >"${binary_list}" <<'PY'
import json
import sys

seen = set()
for line in open(sys.argv[1], encoding="utf-8"):
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue
    if msg.get("reason") != "compiler-artifact":
        continue
    executable = msg.get("executable")
    if not executable or executable in seen:
        continue
    if not msg.get("profile", {}).get("test"):
        continue
    seen.add(executable)
    print(executable)
PY

if [[ ! -s "${binary_list}" ]]; then
  echo "✓ No test binaries produced"
  exit 0
fi

while read -r binary; do
  [[ -n "${binary}" ]] || continue

  attempt=0
  while true; do
    attempt=$((attempt + 1))
    echo "▶ Running ${binary}"

    if run_with_timeout "${binary}"; then
      break
    fi

    status=$?
    if [[ "${status}" -ne 124 || "${attempt}" -gt "${binary_retries}" ]]; then
      echo "error: ${binary} failed with status ${status}" >&2
      exit "${status}"
    fi

    echo "warning: ${binary} timed out during startup/run; retrying (${attempt}/${binary_retries})" >&2
  done
done <"${binary_list}"
