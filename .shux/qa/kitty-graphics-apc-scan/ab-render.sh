#!/usr/bin/env bash
# Render real workloads through real shux panes and snapshot each to PNG.
#
# Built to A/B one shux binary against another: run it twice with different
# `shux` builds and pixel-compare the two output directories. It answers exactly
# one question -- "does this build render the same pixels as that build?" -- so
# every source of nondeterminism has to be pinned, or the comparison measures
# capture timing instead of rendering.
#
# Two properties this harness is careful about, both learned the hard way:
#
#   1. NOTHING IS MASKED. An earlier revision called `pane wait-settled` with
#      the wrong flags and swallowed the error with `|| true`. Every settle call
#      failed, silently, for every run -- the harness had never settled anything
#      and still reported success. `set -euo pipefail` plus no `|| true` on any
#      measurement step is the whole fix.
#
#   2. PANE ECHO IS OFF. Terminal replies to a fixture's own device queries
#      (DA1, DECRQM, DSR, OSC 11) are written back to the PTY master, and a pane
#      with ECHO on paints them into the frame at a position that races the
#      replay. An earlier revision removed this after a 3-run sample suggested
#      the resize race below was the only cause; 3 runs cannot see a 1-in-10
#      flake. Both bugs are real and independent -- fixing the race did NOT make
#      echo-on deterministic. The measurement is recorded in ONE place, the
#      `harness_corrections` entry in evidence-manifest.json, so the two cannot
#      drift apart; do not restate the run counts here.
#
#   3. THE PANE IS SIZED BEFORE THE FIRST BYTE IS REPLAYED. A pane starts at the
#      daemon default (80x24). Spawning the replay and *then* resizing means an
#      arbitrary prefix of the stream is parsed at the wrong width and reflowed,
#      so the snapshot shows 80-column wrapping inside a 120-column frame. The
#      replay therefore blocks on a trigger file that is only created after
#      `pane set-size` has returned.
#
# Completion is signalled out of band (a file on disk), never by printing a
# marker into the pane: a sentinel printed into a full-screen TUI overprints the
# very frame being measured.
#
# Usage: ab-render.sh <shux-binary> <output-dir>
set -euo pipefail

# Resolved, because the leak scan below compares against `readlink /proc/N/exe`,
# which is always absolute with symlinks followed. Invoked as `./shux` or through
# a symlink, an unresolved path matches zero processes and the guard passes for
# work it did not do.
shux_bin="$(readlink -f "$1")"
outdir="$2"
repo_root="$(git rev-parse --show-toplevel)"
mkdir -p "$outdir"

runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-abrender.XXXXXX")"
session_prefix="abrender-$$-${RANDOM}"

sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" "$@"; }

cleanup() {
    local status=$?
    # Sessions first, then the daemon, so no pane child outlives the run.
    while read -r sess; do
        [ -n "${sess}" ] && sx session kill "${sess}" >/dev/null 2>&1 || true
    done <"${runtime}/sessions" 2>/dev/null || true

    # `daemon stop` is allowed to fail (it is idempotent and may run twice), but
    # its failure must not be the last word: the leak check below is what decides.
    sx daemon stop >/dev/null 2>&1 || true
    sleep 0.5

    # Zero leaked daemons is a hard rule, so the harness proves it rather than
    # assuming `daemon stop` worked -- it did not, for a long time, because shux
    # refused to recognise a daemon started by a differently-named binary and
    # said "no daemon running" while leaving it alive.
    #
    # Identified by this run's own socket path in the daemon's argv, which is
    # unique per invocation. A shared needle like `shux` would read other
    # suites' processes, and matching our own argv would invent phantom leaks.
    local leaked=0 pid exe
    for pid in /proc/[0-9]*; do
        pid="${pid#/proc/}"
        exe="$(readlink "/proc/${pid}/exe" 2>/dev/null || true)"
        [ "${exe%% *}" = "${shux_bin}" ] || continue
        if tr '\0' ' ' <"/proc/${pid}/cmdline" 2>/dev/null | grep -qF -- "${runtime}/"; then
            printf 'ab-render: LEAKED daemon pid %s (%s)\n' "${pid}" "${exe}" >&2
            kill -TERM "${pid}" 2>/dev/null || true
            leaked=$((leaked + 1))
        fi
    done

    rm -rf "${runtime}"
    if [ "${leaked}" -gt 0 ]; then
        printf 'ab-render: %d daemon(s) leaked and were reaped; failing the run\n' "${leaked}" >&2
        exit 1
    fi
    exit "${status}"
}
trap cleanup EXIT
: >"${runtime}/sessions"

# ── workloads ────────────────────────────────────────────────────────────────

# Colour probe. Truecolor + indexed + basic are mandatory in any daemon-backed
# capture test, so a monochrome or NO_COLOR regression cannot pass unnoticed.
# Two traps here, both previously fallen into. `printf '\xNN'` is not portable --
# dash's builtin printf leaves it literal -- so escapes are written as real UTF-8
# bytes below, EXCEPT the combining mark, which must stay a decomposed `e` +
# U+0301 to exercise combining-mark storage at all; a precomposed `é` is a single
# scalar and tests nothing. It is emitted via printf's own `\xNN` under `sh -c`
# from bash, which does expand it.
#
# Note on the `wide:` arm: the bundled raster font has no CJK, so `世界` renders
# as tofu. That is base-identical and out of scope here, but it means this arm
# proves wide-cell ACCOUNTING, not glyph rendering -- do not read it as the
# latter.
cat >"${runtime}/colour.sh" <<'SH'
printf '\033[2J\033[H'
printf '\033[38;2;255;96;0mTRUECOLOR-fg\033[0m \033[48;2;0;96;255mTRUECOLOR-bg\033[0m\n'
printf '\033[38;5;208mINDEXED-208\033[0m \033[48;5;27mINDEXED-27\033[0m\n'
printf '\033[31mBASIC-red\033[0m \033[42mBASIC-green-bg\033[0m \033[1mBOLD\033[0m \033[3mITALIC\033[0m \033[4mUNDER\033[0m\n'
printf 'box: ┌─┐│└┘  arrows: ⟠⟐⟁⟡  combining: e\xcc\x81  wide: 世界  zwj: 👩‍💻\n'
printf 'dec-graphics: \033(0lqqqk\033(B ascii-safe\n'
SH

# APC-bearing stream. Every case here is one a byte-stripping splitter would get
# wrong: a well-formed kitty graphics command, an APC aborted by ESC-not-ST, and
# an unterminated APC followed by coloured text that must survive.
cat >"${runtime}/apc.sh" <<'SH'
printf '\033[2J\033[H'
printf 'before-apc\n'
printf '\033_Ga=T,f=32,o=z,s=4,v=4,t=d,i=1,p=1,C=1,q=2,m=1;QUJD\033\\'
printf '\033_Gm=0;REVG\033\\'
printf '\033[38;2;0;255;128mafter-apc-truecolor\033[0m\n'
printf '\033_Ga=T;AAAA\033[31mabort-esc-then-red\033[0m\n'
printf '\033_Gbroken-unterminated'
printf '\033\\'
printf '\033[33mtail-yellow\033[0m\n'
SH

# ── driver ───────────────────────────────────────────────────────────────────

# Spawn a pane blocked on a trigger, size it, release it, then wait for the
# out-of-band done file. Returns only once the workload has finished writing.
render() {
    local label="$1" cols="$2" rows="$3" body="$4"
    local sess="${session_prefix}-${label}"
    local trigger="${runtime}/${label}.go"
    local done="${runtime}/${label}.done"
    local pane json

    json="$(sx --format json session create "${sess}" -d --title "${label}" -- \
        sh -c "stty -echo || { echo 'ab-render: stty -echo failed' >&2; exit 97; }; while [ ! -f '${trigger}' ]; do sleep 0.02; done; ${body}; : >'${done}'; sleep 600")"
    echo "${sess}" >>"${runtime}/sessions"
    pane="$(jq -r '.pane_id' <<<"${json}")"

    # Size FIRST. set-size is synchronous, so the next snapshot sees the new
    # dims and the replay below starts at the right geometry.
    sx pane set-size -s "${sess}" -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
    : >"${trigger}"

    local waited=0
    while [ ! -f "${done}" ]; do
        sleep 0.05
        waited=$((waited + 1))
        if [ "${waited}" -gt 600 ]; then
            printf 'ab-render: %s never finished writing\n' "${label}" >&2
            exit 1
        fi
    done

    # Positional <PANE> (a full pane UUID, so no -s/--session), and --quiet
    # takes a human duration. Deliberately unguarded: a settle failure must
    # abort the run, not be absorbed by `|| true`.
    sx pane wait-settled "${pane}" --quiet 400ms --timeout 15s >/dev/null

    sx pane snapshot -s "${sess}" -p "${pane}" -o "${outdir}/${label}.png" >/dev/null
    sx pane capture -s "${sess}" -p "${pane}" --lines "${rows}" >"${outdir}/${label}.txt"
    sx session kill "${sess}" >/dev/null
}

render colour-80x24 80 24 "bash '${runtime}/colour.sh'"
render colour-120x40 120 40 "bash '${runtime}/colour.sh'"
render apc-80x24 80 24 "sh '${runtime}/apc.sh'"
render apc-120x40 120 40 "sh '${runtime}/apc.sh'"

# Rich TUIs, replayed from the committed corpus at the geometry it was RECORDED
# at. rich-tui/manifest.json declares cols=120 rows=36; replaying at any other
# size measures reflow, not the recording.
corpus_cols="$(jq -r '.cols' "${repo_root}/.shux/fixtures/vt-corpus/rich-tui/manifest.json")"
corpus_rows="$(jq -r '.rows' "${repo_root}/.shux/fixtures/vt-corpus/rich-tui/manifest.json")"
while read -r name raw; do
    render "tui-${name}" "${corpus_cols}" "${corpus_rows}" \
        "cat '${repo_root}/.shux/fixtures/vt-corpus/rich-tui/${raw}'"
done < <(jq -r '.fixtures[] | "\(.name) \(.raw)"' \
    "${repo_root}/.shux/fixtures/vt-corpus/rich-tui/manifest.json")

ls "$outdir"
