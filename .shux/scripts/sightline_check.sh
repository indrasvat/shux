#!/usr/bin/env bash
# Focused Sightline validation. This is intentionally leak-guard friendly:
# callers should run it through .shux/scripts/no_leak_guard.sh.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
source "${REPO_ROOT}/.shux/scripts/lib/shux_harness.sh"

SIGHTLINE="${REPO_ROOT}/plugins/sightline/bin/sightline"
SHUX_BIN="${SHUX_BIN:-${REPO_ROOT}/target/release/shux}"
OUT_DIR="${REPO_ROOT}/.shux/out/sightline/check"
RUNTIME_DIR=""
SESSION="sightline-check"
WINDOW_OUT_DIR="${OUT_DIR}/window-target"

cleanup() {
  if [[ -n "${RUNTIME_DIR}" ]]; then
    shux_harness_kill_plugin "${RUNTIME_DIR}" "${SHUX_BIN}" sightline
    shux_harness_cleanup_runtime "${RUNTIME_DIR}" "${SHUX_BIN}" "${SESSION}"
  fi
}
trap cleanup EXIT

if [[ ! -x "${SIGHTLINE}" ]]; then
  echo "sightline runner is not executable: ${SIGHTLINE}" >&2
  exit 1
fi

"${SIGHTLINE}" self-test

if [[ ! -x "${SHUX_BIN}" ]]; then
  echo "shux binary not found at ${SHUX_BIN}; run make release first or set SHUX_BIN" >&2
  exit 1
fi

rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}"
RUNTIME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sightline-runtime.XXXXXX")"

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${RUNTIME_DIR}" "${SHUX_BIN}" --format json \
  session create "${SESSION}" -d --title sightline-check -- bash --noprofile --norc \
  > "${OUT_DIR}/session.json"

pane_id="$(jq -r '.pane_id' "${OUT_DIR}/session.json")"
if [[ -z "${pane_id}" || "${pane_id}" == "null" ]]; then
  echo "session create did not return pane_id" >&2
  exit 1
fi

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${RUNTIME_DIR}" "${SIGHTLINE}" verify \
  --shux-bin "${SHUX_BIN}" \
  --session "${SESSION}" \
  --pane "${pane_id}" \
  --out-dir "${OUT_DIR}" \
  --viewport 80x24 \
  --viewport 120x40 \
  --color-probe-shell \
  --expect-text SIGHTLINE_COLOR_PROBE_DONE

jq -e '.verdict == "PASS"' "${OUT_DIR}/summary.json" >/dev/null
test -s "${OUT_DIR}/SIGHTLINE.md"
test -s "${OUT_DIR}/pane_80x24.png"
test -s "${OUT_DIR}/pane_120x40.png"
test -s "${OUT_DIR}/color-probe.raw"

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${RUNTIME_DIR}" "${SHUX_BIN}" --format json \
  window create --session "${SESSION}" --name secondary -- bash --noprofile --norc \
  > "${OUT_DIR}/secondary-window.json"

secondary_pane_id="$(jq -r '.pane_id' "${OUT_DIR}/secondary-window.json")"
if [[ -z "${secondary_pane_id}" || "${secondary_pane_id}" == "null" ]]; then
  echo "window create did not return pane_id" >&2
  exit 1
fi

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${RUNTIME_DIR}" "${SIGHTLINE}" verify \
  --shux-bin "${SHUX_BIN}" \
  --session "${SESSION}" \
  --window secondary \
  --out-dir "${WINDOW_OUT_DIR}" \
  --viewport 80x24 \
  --color-probe-shell \
  --expect-text SIGHTLINE_COLOR_PROBE_DONE

jq -e '.verdict == "PASS"' "${WINDOW_OUT_DIR}/summary.json" >/dev/null
jq -e --arg pane_id "${secondary_pane_id}" '.pane_id == $pane_id' "${WINDOW_OUT_DIR}/summary.json" >/dev/null
test -s "${WINDOW_OUT_DIR}/pane_80x24.png"
test -s "${WINDOW_OUT_DIR}/color-probe.raw"

env -u SHUX_SOCKET XDG_RUNTIME_DIR="${RUNTIME_DIR}" "${SHUX_BIN}" plugin install "${REPO_ROOT}/plugins/sightline" --no-watch >/dev/null
env -u SHUX_SOCKET XDG_RUNTIME_DIR="${RUNTIME_DIR}" "${SHUX_BIN}" --format json plugin list \
  | jq -e '.plugins[] | select(.name == "sightline" and .status == "running")' >/dev/null
env -u SHUX_SOCKET XDG_RUNTIME_DIR="${RUNTIME_DIR}" "${SHUX_BIN}" plugin stop sightline >/dev/null

echo "sightline check passed: ${OUT_DIR}"
