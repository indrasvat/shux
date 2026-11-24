#!/usr/bin/env bash
# Prove scripts/disk-check.sh warns below its threshold and is silent above it,
# reads df -Pk whatever the filesystem is called, says so loudly when df gives it
# nothing to read or the threshold is malformed, and that the hook commands in
# .claude/settings.json always exit 0 and print valid JSON at Stop, even when
# make itself fails. df is a stub on PATH, so no case depends on this disk.
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
CHECK="${SRC}/scripts/disk-check.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/shux-disk-check-selftest.XXXXXX")"
trap 'find "${work}" -delete' EXIT
mkdir -p "${work}/bin"
command -v jq >/dev/null 2>&1 || { echo "✗ disk-check self-test: jq is required to check hook JSON" >&2; exit 1; }

die() { echo "✗ disk-check self-test: $*" >&2; exit 1; }

# stub_df <stdout> <exit> [stderr]
stub_df() {
  printf '#!/bin/sh\nprintf "%%b" "%s"\nprintf "%%b" "%s" >&2\nexit %s\n' "$1" "${3:-}" "$2" >"${work}/bin/df"
  chmod +x "${work}/bin/df"
}
HEAD='Filesystem 1024-blocks Used Available Capacity Mounted on\n'
LOW="${HEAD}/dev/x 100000000 90000000 10485760 90% /mnt/data disk\n"
HIGH="${HEAD}/dev/x 100000000 10000000 90000000 10% /\n"
# 5 GiB free on a share whose name has spaces: Used must not be read as Available.
SPACED="${HEAD}//server/My Share 100000000 95000000 5242880 95% /Volumes/My Share\n"

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
json() { printf '%s' "${LAST}" | jq -e "$1" >/dev/null || die "not the JSON Claude Code reads ($1): ${LAST}"; }

stub_df "${LOW}" 0
expect 0 "10.0 GiB free on /mnt/data disk (below 40 GiB)" --
expect 0 "make prune" -- --hook session
expect 0 "" SHUX_DISK_MIN_GIB=5 --
expect 0 '"systemMessage": "⚠ disk: 10.0 GiB free' -- --hook stop
json '.systemMessage | contains("make prune")'
stub_df "${SPACED}" 0
expect 0 "5.0 GiB free on /Volumes/My Share" --
stub_df "${LOW}" 0 'df: warning: a stale mount\n'
expect 0 "10.0 GiB free on /mnt/data disk" --
stub_df "${HIGH}" 0
expect 0 "" --
expect 0 "" -- --hook stop

# Nothing to read, a failing df, a bad threshold: loud, and fatal only outside a hook.
stub_df "" 0
expect 1 "could not read free space" --
expect 0 "could not read free space" -- --hook session
stub_df "" 1 'df: boom\n'
expect 1 "df -Pk failed: df: boom" --
expect 0 '"systemMessage": "⚠ disk-check: df -Pk failed' -- --hook stop
json '.systemMessage | contains("boom")'
stub_df "${HIGH}" 0
for bad in lots 08 010 99999999999999 -1 '' ' 4' $'4\n' $'bad\tval\r"\\'; do
  [[ -n "${bad}" ]] || continue  # empty means the default
  expect 1 "is not a whole number" "SHUX_DISK_MIN_GIB=${bad}" --
  expect 0 "is not a whole number" "SHUX_DISK_MIN_GIB=${bad}" -- --hook stop
  json '.systemMessage | contains("not a whole number")'
done
expect 2 "usage" -- --hook
expect 2 "usage" -- --hook sometimes

# The exact hook commands, run as Claude Code runs them: through bash, in any project dir.
hook_cmd() {
  jq -er --arg ev "$1" '.hooks[$ev][].hooks[] | select(.command | contains("disk-check")) | .command' \
    "${SRC}/.claude/settings.json"
}
STOP_CMD="$(hook_cmd Stop)" || die "no disk-check Stop hook in .claude/settings.json"
SESSION_CMD="$(hook_cmd SessionStart)" || die "no disk-check SessionStart hook in .claude/settings.json"
# Project dirs whose make misbehaves; each has the hook script, as a real checkout would.
for d in broken old info noisy; do
  mkdir -p "${work}/${d}/scripts"
  cp "${SRC}/scripts/disk-check-hook.sh" "${work}/${d}/scripts/"
done
printf '<<<<<<< a merge conflict\noops\n' >"${work}/broken/Makefile"
printf 'all:\n\t@true\n' >"${work}/old/Makefile"
# shellcheck disable=SC2016  # make syntax, written literally
printf '$(info loading)\ndisk-check:\n\t@echo half; false\n' >"${work}/info/Makefile"
# shellcheck disable=SC2016  # make syntax, written literally
printf '$(info noise)\ndisk-check:\n\t@echo '"'"'{"systemMessage": "from make"}'"'"'\n' >"${work}/noisy/Makefile"
# run_hook <cmd> <project dir> [env…]
run_hook() {
  local cmd="$1" dir="$2" rc=0
  shift 2
  LAST="$(env PATH="${work}/bin:${PATH}" CLAUDE_PROJECT_DIR="${dir}" "$@" bash -c "${cmd}" 2>/dev/null)" || rc=$?
  [[ "${rc}" -eq 0 ]] || die "hook '${cmd}' in ${dir} exited ${rc}"
}
# Stop stdout is nothing, or exactly one line that is one JSON document.
one_json() {
  [[ -n "${LAST}" ]] || return 0
  [[ "${LAST}" != *$'\n'* ]] || die "Stop hook in $1 printed more than one line: ${LAST}"
  json '.systemMessage | type == "string"'
}
stub_df "${LOW}" 0
for dir in "${SRC}" "${work}/broken" "${work}/old" "${work}/info" "${work}/noisy" "${work}/missing"; do
  for env in SHUX_DISK_MIN_GIB=40 SHUX_DISK_MIN_GIB=08 $'SHUX_DISK_MIN_GIB=x\ny\t"z'; do
    run_hook "${STOP_CMD}" "${dir}" "${env}"
    one_json "${dir}"
    [[ "${dir}" == "${SRC}" ]] || [[ -n "${LAST}" ]] || die "Stop hook in ${dir} printed nothing"
    run_hook "${SESSION_CMD}" "${dir}" "${env}"
    [[ "${dir}" == "${work}/noisy" || -n "${LAST}" ]] || die "SessionStart hook in ${dir} with ${env} printed nothing"
  done
done
run_hook "${STOP_CMD}" "${SRC}" SHUX_DISK_MIN_GIB=40
json '.systemMessage | contains("make prune")'
run_hook "${STOP_CMD}" "${work}/noisy"
json '.systemMessage == "from make"'

# The first df's own error is the one reported.
cat >"${work}/bin/df" <<EOF
#!/bin/sh
if [ -e "${work}/df.once" ]; then printf '%b' "${HIGH}"; exit 0; fi
: >"${work}/df.once"; echo "df: first failure" >&2; exit 1
EOF
expect 1 "df -Pk failed: df: first failure" --

echo "✓ disk-check self-test: warns below the threshold, reads any df -Pk, loud on bad input, hooks always exit 0"
