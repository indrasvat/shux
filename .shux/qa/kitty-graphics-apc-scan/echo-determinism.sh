#!/usr/bin/env bash
# Measure whether the nvim corpus replay is deterministic with pane echo ON vs OFF.
#
# This exists because the answer is counter-intuitive and was got wrong once.
# `ab-render.sh` disables pane echo, and an earlier revision removed that on the
# strength of a 3-run sample. Three runs cannot see a 1-in-10 flake. The recorded
# fixtures replay their own device queries -- nvim.raw carries 10 (DA1, DECRQM,
# DSR, OSC 11), lazygit.raw 5 -- shux answers them by writing to the PTY master,
# and a pane with ECHO on paints those answers into the frame at a position that
# races the replay.
#
# Committed so the claim is reproducible rather than asserted. If you are about
# to remove `stty -echo` from ab-render.sh, run this first, with n >= 20.
#
# Usage: echo-determinism.sh <shux-binary> [runs] [on|off]
#
# Measured on this machine, one binary, nothing else varying:
#   echo on  : 35 runs -> 3 distinct images
#   echo off : 37 runs -> 1 distinct image
set -euo pipefail

bin="$(readlink -f "$1")"
runs="${2:-20}"
echo_mode="${3:-on}"
repo_root="$(git rev-parse --show-toplevel)"
raw="${repo_root}/.shux/fixtures/vt-corpus/rich-tui/nvim.raw"
cols="$(jq -r '.cols' "${repo_root}/.shux/fixtures/vt-corpus/rich-tui/manifest.json")"
rows="$(jq -r '.rows' "${repo_root}/.shux/fixtures/vt-corpus/rich-tui/manifest.json")"
out="$(mktemp -d "${TMPDIR:-/tmp}/shux-echodet.XXXXXX")"
trap 'rm -rf "${out}"' EXIT

for i in $(seq 1 "${runs}"); do
    runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-echodet-rt.XXXXXX")"
    session="echodet-$$-${i}"
    sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" "$@"; }

    trigger="${runtime}/go"
    done_file="${runtime}/done"
    prelude=""
    if [ "${echo_mode}" = off ]; then
        prelude="stty -echo || exit 97; "
    fi

    json="$(sx --format json session create "${session}" -d -- \
        sh -c "${prelude}while [ ! -f '${trigger}' ]; do sleep 0.02; done; cat '${raw}'; : >'${done_file}'; sleep 300")"
    pane="$(jq -r '.pane_id' <<<"${json}")"

    # Size before the first replayed byte, or the prefix is parsed at 80x24.
    sx pane set-size -s "${session}" -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
    : >"${trigger}"

    waited=0
    while [ ! -f "${done_file}" ]; do
        sleep 0.05
        waited=$((waited + 1))
        if [ "${waited}" -gt 600 ]; then
            printf 'echo-determinism: run %s never finished replaying\n' "${i}" >&2
            exit 1
        fi
    done

    sx pane wait-settled "${pane}" --quiet 400ms --timeout 15s >/dev/null
    sx pane snapshot -s "${session}" -p "${pane}" -o "${out}/run${i}.png" >/dev/null
    sx session kill "${session}" >/dev/null
    sx daemon stop >/dev/null 2>&1 || true

    # Reap by THIS run's unique runtime dir, never a shared needle like `shux`:
    # a shared one reads other suites' processes, and matching our own argv
    # invents phantom leaks.
    for proc in /proc/[0-9]*; do
        pid="${proc#/proc/}"
        [ "$(readlink "${proc}/exe" 2>/dev/null || true)" = "${bin}" ] || continue
        if tr '\0' ' ' <"${proc}/cmdline" 2>/dev/null | grep -qF -- "${runtime}/"; then
            kill -TERM "${pid}" 2>/dev/null || true
        fi
    done
    rm -rf "${runtime}"
done

distinct="$(sha256sum "${out}"/*.png | awk '{print $1}' | sort -u | wc -l)"
printf 'echo=%s runs=%s distinct=%s\n' "${echo_mode}" "${runs}" "${distinct}"
sha256sum "${out}"/*.png | awk '{print substr($1, 1, 12)}' | sort | uniq -c
[ "${echo_mode}" = off ] && [ "${distinct}" -ne 1 ] && {
    printf 'echo-determinism: echo-off was expected to be deterministic\n' >&2
    exit 1
}
exit 0
