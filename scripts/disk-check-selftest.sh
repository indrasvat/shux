#!/usr/bin/env bash
# Prove scripts/disk-check.sh warns below its threshold and is silent above it,
# says so loudly when df gives it nothing to read, and never exits non-zero as a hook.
# df is replaced by a stub on PATH, so every case is independent of this disk.
set -euo pipefail

CHECK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/disk-check.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/shux-disk-check-selftest.XXXXXX")"
trap 'find "${work}" -delete' EXIT
mkdir -p "${work}/bin"

die() { echo "✗ disk-check self-test: $*" >&2; exit 1; }

# stub_df <stdout> <exit>
stub_df() {
  printf '#!/bin/sh\nprintf "%%b" "%s"\nexit %s\n' "$1" "$2" >"${work}/bin/df"
  chmod +x "${work}/bin/df"
}
LOW='Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100000000 90000000 10485760 90%% /mnt/data disk\n'
HIGH='Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100000000 10000000 90000000 10%% /\n'

# expect <want rc> <stdout needle, or "" for silent> <env…> -- <args…>
expect() {
  local rc_want="$1" needle="$2" rc=0 out
  shift 2
  local envs=()
  while [[ "$1" != -- ]]; do envs+=("$1"); shift; done
  shift
  out="$(env PATH="${work}/bin:${PATH}" "${envs[@]+"${envs[@]}"}" "${BASH}" "${CHECK}" "$@" 2>&1)" || rc=$?
  [[ "${rc}" -eq "${rc_want}" ]] || die "args '$*' ${envs[*]+"${envs[*]}"}: exit ${rc}, wanted ${rc_want}: ${out}"
  if [[ -z "${needle}" ]]; then
    [[ -z "${out}" ]] || die "args '$*' should be silent, printed: ${out}"
  else
    [[ "${out}" == *"${needle}"* ]] || die "args '$*' printed '${out}', wanted '${needle}'"
  fi
  LAST="${out}"
}

stub_df "${LOW}" 0
expect 0 "10.0 GiB free on /mnt/data disk (below 40 GiB)" --
expect 0 "make prune" -- --hook session
expect 0 "" SHUX_DISK_MIN_GIB=5 --
expect 0 '"systemMessage": "⚠ disk: 10.0 GiB free' -- --hook stop
if command -v jq >/dev/null 2>&1; then
  printf '%s' "${LAST}" | jq -e '.systemMessage | contains("make prune")' >/dev/null ||
    die "--hook stop output is not the JSON Claude Code reads: ${LAST}"
fi
stub_df "${HIGH}" 0
expect 0 "" --
expect 0 "" -- --hook stop

# Nothing to read, a failing df, a bad threshold: loud, and fatal only outside a hook.
stub_df "" 0
expect 1 "could not read free space" --
expect 0 "could not read free space" -- --hook session
stub_df "df: boom" 1
expect 1 "df -Pk failed: df: boom" --
expect 0 '"systemMessage": "⚠ disk-check: df -Pk failed' -- --hook stop
stub_df "${HIGH}" 0
expect 1 "is not a whole number" SHUX_DISK_MIN_GIB=lots --
expect 2 "usage" -- --hook
expect 2 "usage" -- --hook sometimes

echo "✓ disk-check self-test: warns below the threshold, silent above, loud on unreadable df, hooks exit 0"
