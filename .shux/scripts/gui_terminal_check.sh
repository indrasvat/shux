#!/usr/bin/env bash
# Photograph shux in a real GUI terminal — kitty under Xvfb (issue #175).
#
#   SHUX_BIN=<binary> .shux/scripts/gui_terminal_check.sh [--scenario S] [--out DIR]
#   --help for the options; `make test-gui-terminal` runs it under the leak guard.
#
# Why a foreign terminal has to draw it, what the assertions are, and what this
# rig cannot see: docs/agents/visual-testing.md § "The GUI-terminal rig".
#
# Process hygiene: isolated short XDG_RUNTIME_DIR / CONFIG / STATE / CACHE / DATA;
# Xvfb and kitty tracked by the pid recorded at launch, never by an argv substring;
# the daemon stopped through lib/shux_harness.sh, which reads its pidfile. The rig
# asserts its own teardown, because `no_leak_guard.sh` learned about X servers only
# in this same change.
#
# Output: .shux/out/gui-terminal/<scenario>/. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
# shellcheck source=lib/shux_harness.sh disable=SC1091  # path is runtime-resolved
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

# The orphan half of the leak guard only claims a process whose working
# directory is inside this repository (lib/proc_scope.sh). Launching kitty from
# a scratch directory would put its `shux session attach` child outside that
# attribution and make a leaked client invisible.
cd "${repo_root}"

lib_dir="${repo_root}/.shux/scripts/lib"
verdict_py="${lib_dir}/kitty_frame_verdict.py"
payload_py="${lib_dir}/kitty_image_payload.py"
workload_sh="${lib_dir}/gui_terminal_workload.sh"
launch_sh="${lib_dir}/kitty_attach_launch.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
scenario="plain"
frames_per_phase=3
out_dir=""

usage() {
    cat <<'USAGE'
gui_terminal_check.sh — photograph shux in a real GUI terminal (issue #175).

  SHUX_BIN=<binary> .shux/scripts/gui_terminal_check.sh [options]

  --scenario S   plain            (default) shux alone, at four window sizes
                 image-contained  an injected image bounded by a cell box; must pass
                 image-overflow   the same image unbounded; MUST fail on containment
                 image-pane       the SAME unbounded image, drawn by a PANE and
                                  re-emitted by shux; must pass. Paired with
                                  image-overflow that is the whole claim: identical
                                  bytes, contained only when shux emits them
  --frames N     frames per phase (default 3)
  --out DIR      where frames and run.json go
                 (default .shux/out/gui-terminal/<scenario>)

Exit: 0 pass · 1 an assertion failed · 2 usage or precondition · 3 a tool is
missing or broken, so nothing was checked.

Run it through `make test-gui-terminal`, which proves the rig can fail first and
puts it under the leak guard. `docs/agents/visual-testing.md` has the rest.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --scenario)
            scenario="${2:?--scenario needs a value}"
            shift 2
            ;;
        --frames)
            frames_per_phase="${2:?--frames needs a value}"
            shift 2
            ;;
        --out)
            out_dir="${2:?--out needs a value}"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "gui_terminal_check: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

case "${scenario}" in
    plain | image-contained | image-overflow | image-pane) ;;
    *)
        echo "gui_terminal_check: unknown scenario: ${scenario}" >&2
        echo "  expected one of: plain, image-contained, image-overflow, image-pane" >&2
        exit 2
        ;;
esac

case "${frames_per_phase}" in
    '' | *[!0-9]*)
        echo "gui_terminal_check: --frames must be a positive integer, got '${frames_per_phase}'" >&2
        exit 2
        ;;
esac
if [ "${frames_per_phase}" -lt 1 ]; then
    echo "gui_terminal_check: --frames must be at least 1" >&2
    exit 2
fi

out_dir="${out_dir:-${repo_root}/.shux/out/gui-terminal/${scenario}}"

# ── Fiducials ───────────────────────────────────────────────────────────────
#
# Geometry is MEASURED off the photograph rather than derived from font metrics,
# so the frame has to carry landmarks. Each of these is pinned by the isolated
# config below and is used nowhere else in the picture; shux's default theme
# paints the status bar accent in the same sapphire as the pane border, which
# would make "find the border" ambiguous.
BORDER_RGB="255,0,0"
STATUS_RGB="0,0,255"
CONTENT_RGB="255,0,212"
IMAGE_RGB="0,255,135"

BLOCK_COLS=40
BLOCK_ROWS=5

# Xvfb screen. It must hold the LARGEST phase window: X clips a window at the
# screen edge and `import` returns only the screen, so a window grown past it
# hands the containment assertion a frame with the overflow region cropped out —
# a vacuous pass on the one defect this rig exists to catch. Asserted below
# rather than left to whoever edits the phase table.
SCREEN_W=1400
SCREEN_H=900

# Window sizes in pixels, in the order the run walks them.
PHASE_NAMES=(initial grow shrink restore)
PHASE_SIZES=("1000x640" "1240x820" "860x520" "1000x640")

fail() {
    echo "✗ $*" >&2
    exit 1
}

for size in "${PHASE_SIZES[@]}"; do
    if [ "${size%x*}" -gt "${SCREEN_W}" ] || [ "${size#*x}" -gt "${SCREEN_H}" ]; then
        echo "✗ gui_terminal_check: phase window ${size} does not fit the" >&2
        echo "  ${SCREEN_W}x${SCREEN_H} screen. X would clip it and the capture would" >&2
        echo "  silently lose the region an overflow lands in." >&2
        exit 2
    fi
done

# ── Preflight ───────────────────────────────────────────────────────────────
#
# A guard whose tool is missing must say so and exit non-zero. Reporting success
# for work it did not do is the one thing it must never do — so every tool is
# named, all failures are reported at once, and the exit code is distinct from an
# assertion failure so the selftest can tell the two apart.
#
# Each tool is RUN, not looked up. `command -v` answers "is there a file with
# this name on PATH", which is a different question: it skips a non-executable
# entry and keeps searching, and it says nothing at all about a tool that is
# installed and broken. The likeliest broken tool here is the comparator itself —
# a `uv run --script` that resolves pillow and numpy, which fails on a cache-cold
# offline machine — and its failure would otherwise surface as "shux's chrome
# never appeared", blaming shux for a missing wheel.
missing=()
probe_tool() {
    local tool="$1" status=0
    shift
    "$@" >/dev/null 2>&1 || status=$?
    if [ "${status}" -ne 0 ]; then
        missing+=("missing: ${tool} (probe '$*' exited ${status})")
    fi
}

probe_tool kitty kitty --version
probe_tool Xvfb Xvfb -help
probe_tool xdotool xdotool --version
probe_tool import import -version
probe_tool python3 python3 -c pass
probe_tool uv uv --version

if [ ! -x "${shux_bin}" ]; then
    missing+=("missing: ${shux_bin} — build it with 'make release'")
fi
for helper in "${verdict_py}" "${payload_py}" "${workload_sh}" "${launch_sh}"; do
    [ -f "${helper}" ] || missing+=("missing: ${helper}")
done

# The comparator, exercised rather than assumed — against a picture with a
# rectangle in it, and required to FIND it.
#
# Probing a black PNG and expecting "no rectangle" cannot work: the comparator
# reports that with exit 1, and a comparator that could not start reports its own
# failure with exit 1 as well. Measured — with `UV_OFFLINE=1` and a cold cache,
# `uv` cannot resolve pillow and exits 1, which a black-PNG probe reads as a
# healthy "no". Demanding a rectangle back makes the two distinguishable, and
# proves the detector works rather than only that the interpreter starts.
if [ "${#missing[@]}" -eq 0 ]; then
    probe_png="$(mktemp "${TMPDIR:-/tmp}/shux-guiterm-probe.XXXXXX.png")"
    python3 -c '
import struct, sys, zlib
w, h = 40, 30
px = [[(0, 0, 0)] * w for _ in range(h)]
for x in range(2, 38):
    px[2][x] = px[27][x] = (255, 0, 0)
for y in range(2, 28):
    px[y][2] = px[y][37] = (255, 0, 0)
raw = b"".join(b"\x00" + bytes(v for cell in row for v in cell) for row in px)
def chunk(tag, data):
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
open(sys.argv[1], "wb").write(
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw))
    + chunk(b"IEND", b""))
' "${probe_png}"
    comparator_status=0
    comparator_out="$("${verdict_py}" --probe "${probe_png}" --border-rgb "${BORDER_RGB}" 2>&1)" ||
        comparator_status=$?
    if [ "${comparator_status}" -ne 0 ] || [ "${comparator_out}" != "2 2 37 27" ]; then
        missing+=("missing: a working ${verdict_py} — probing a 40x30 PNG with a red rectangle at (2,2)-(37,27) exited ${comparator_status} and said '${comparator_out}'")
    fi
    rm -f "${probe_png}"
fi

if [ "${#missing[@]}" -gt 0 ]; then
    echo "✗ gui_terminal_check: cannot run — nothing was checked." >&2
    for m in "${missing[@]}"; do
        echo "    ${m}" >&2
    done
    cat >&2 <<'HINT'
  This rig needs a real GUI terminal and an X server it can paint into. On
  Debian/Ubuntu the packages are the terminal, the X automation tool, the
  headless X server and ImageMagick.
HINT
    exit 3
fi

# ── Isolated state ──────────────────────────────────────────────────────────
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-guiterm.XXXXXX")"
config_home="${runtime}/config"
state_home="${runtime}/state"
mkdir -p "${config_home}/shux" "${state_home}/shux" "${out_dir}"
rm -f "${out_dir}"/*.png "${out_dir}"/*.json "${out_dir}"/*.log 2>/dev/null || true

cat >"${config_home}/shux/config.toml" <<TOML
[appearance]
border_style = "rounded"
# The rig's font is DejaVu Sans Mono, which has no Nerd Font glyphs. Left on,
# every status-bar icon renders as tofu and the bar's pixels stop being the
# landmark this run measures against.
nerd_fonts = false

[theme]
border_focused = "#ff0000"
border_unfocused = "#ff0000"
status_bg = "#0000ff"
status_fg = "#ffffff"
status_accent = "#ffffff"
status_muted = "#ffffff"
status_branch = "#ffffff"
TOML

# The first-attach welcome toast covers the pane, which is exactly where the
# assertions look. It is state, not config: pre-seed it as already seen.
printf '{"prefix_discovered":true,"welcome_toast_seen":true}\n' \
    >"${state_home}/shux/onboarding.json"

marker="GUI-RIG-READY-${RANDOM}${RANDOM}"
session="guiterm-${scenario}-$$"
display=""
xvfb_pid=""
kitty_pid=""
window_id=""
geometry_json="${out_dir}/run.json"
inject_log="${out_dir}/inject.log"
kitty_log="${out_dir}/kitty.log"
go_file="${runtime}/inject.go"
payload_file="${runtime}/inject.esc"

# TERM, wait, then KILL — by pid recorded at launch, never by an argv substring.
reap() {
    local pid="$1"
    [ -n "${pid}" ] || return 0
    kill -0 "${pid}" 2>/dev/null || return 0
    kill -TERM "${pid}" 2>/dev/null || true
    local _
    for _ in $(seq 1 20); do
        kill -0 "${pid}" 2>/dev/null || return 0
        sleep 0.1
    done
    kill -KILL "${pid}" 2>/dev/null || true
    return 0
}

# Every step ends in `|| true` and the function ends in `return 0`. `|| true` is
# forbidden in a MEASUREMENT path and required in a TEARDOWN one: under
# `set -e`, the first kill that returns non-zero — which is the normal case, the
# process having already exited — aborts the trap and every later step is
# skipped. Measured on this pattern: the X server leaked, reparented to pid 1,
# and the guard that is supposed to notice cannot see an Xvfb at all.
cleanup() {
    local status=$?
    reap "${kitty_pid}" || true
    shux_harness_kill_session "${runtime}" "${shux_bin}" "${session}" || true
    shux_harness_stop_daemon "${runtime}" || true
    reap "${xvfb_pid}" || true
    # The rig owns these two, so it asserts its own hygiene rather than
    # delegating to a guard that does not know their names.
    local leaked=""
    if [ -n "${kitty_pid}" ] && kill -0 "${kitty_pid}" 2>/dev/null; then
        leaked="${leaked} kitty=${kitty_pid}"
    fi
    if [ -n "${xvfb_pid}" ] && kill -0 "${xvfb_pid}" 2>/dev/null; then
        leaked="${leaked} Xvfb=${xvfb_pid}"
    fi
    rm -rf "${runtime}" || true
    if [ -n "${leaked}" ]; then
        echo "✗ gui_terminal_check: left processes running:${leaked}" >&2
        exit 1
    fi
    exit "${status}"
}
# `trap cleanup EXIT INT TERM` — the idiom 39 scripts in this tree use — runs
# cleanup TWICE on a signal and exits 0. Measured: SIGINT gave exit 0 with two
# cleanups; the form below gives exit 130 with one. A Ctrl-C'd run reporting
# success is the worst outcome available to a guard.
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

sx() {
    env -u SHUX_SOCKET \
        XDG_RUNTIME_DIR="${runtime}" \
        XDG_CONFIG_HOME="${config_home}" \
        XDG_STATE_HOME="${state_home}" \
        "${shux_bin}" "$@"
}

echo "▶ GUI-terminal rig — scenario ${scenario}"
echo "    shux:  $(${shux_bin} version 2>/dev/null | head -1)"
echo "    kitty: $(kitty --version 2>/dev/null | head -1)"
echo "    out:   ${out_dir}"

# ── X server ────────────────────────────────────────────────────────────────
#
# `-displayfd` rather than a hardcoded `:99`: Xvfb picks a free display itself
# and writes the number back, which is race-free. Scanning /tmp/.X<N>-lock and
# then claiming the winner is a TOCTOU with any other X server starting at the
# same moment, and this suite shares a machine with whatever else the developer
# is running.
# `-noreset`: without it the server resets — destroying every window — the moment
# its last client disconnects, and between phases the only client is kitty.
Xvfb -displayfd 3 -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp -noreset \
    >"${out_dir}/xvfb.log" 2>&1 3>"${runtime}/display" &
xvfb_pid=$!

for _ in $(seq 1 100); do
    if [ -s "${runtime}/display" ]; then
        break
    fi
    kill -0 "${xvfb_pid}" 2>/dev/null || fail "Xvfb exited during startup: $(cat "${out_dir}/xvfb.log")"
    sleep 0.1
done
[ -s "${runtime}/display" ] || fail "Xvfb never reported a display number"
display=":$(tr -cd '0-9' <"${runtime}/display")"
export DISPLAY="${display}"
echo "    display: ${display} (Xvfb pid ${xvfb_pid})"

# ── The session under photograph ────────────────────────────────────────────
# A short, FIXED title. The pane title is drawn in the border colour on the top
# border row, so a long or cwd-derived one eats into the rule the rectangle
# detector measures. Twelve characters is under 15% of the narrowest phase.
# Which payload goes where. Decided BEFORE the session exists, because the
# workload is launched with it: the sidecar shares kitty's terminal to put bytes
# on screen that shux never saw, while `image-pane` is the opposite -- the bytes
# are the pane's own output and reach the screen only through shux's emit.
launch_payload="-"
pane_payload=""
case "${scenario}" in
    image-contained | image-overflow) launch_payload="${payload_file}" ;;
    image-pane) pane_payload="${payload_file}" ;;
esac

sx session create "${session}" -d --title "shux-gui-rig" -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
    HOME="${runtime}" bash "${workload_sh}" "${CONTENT_RGB//,/;}" \
    "${BLOCK_COLS}" "${BLOCK_ROWS}" "${marker}" "${pane_payload}" >/dev/null

# ONE pane, asserted rather than assumed. `find_rect` takes the outermost rules
# of the border mask, so with two panes it measures the UNION of their outlines
# and an image spilling out of one pane into its neighbour scores zero pixels
# outside. That is a real blind spot, and the rig is single-pane by design: it
# fails loudly here rather than reporting a containment pass it cannot make.
pane_count="$(sx --format json pane list -s "${session}" |
    python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d if isinstance(d,list) else d["panes"]))')"
if [ "${pane_count}" != "1" ]; then
    echo "✗ gui_terminal_check: session has ${pane_count} panes; this rig measures one." >&2
    exit 2
fi
pane_id="$(sx --format json pane list -s "${session}" |
    python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,list) else d["panes"])[0]["id"])')"
[ -n "${pane_id}" ] || fail "no pane in session ${session}"

# Content first. The marker is printed by the workload after everything else is
# on the screen and cannot be echoed back by a shell, so it means what it says.
sx pane wait-for -s "${session}" -p "${pane_id}" -t "${marker}" --timeout-ms 30000 >/dev/null ||
    fail "workload never reached its marker"

# ── The real GUI terminal ───────────────────────────────────────────────────
# Mesa software GL: there is no GPU in CI or in the cloud container.
# `--config NONE` so a developer's kitty.conf cannot move the geometry every
# measurement depends on — and HOME/XDG_CACHE_HOME/XDG_DATA_HOME isolated as
# well, because kitty writes `~/.cache/kitty` and `~/.config/kitty` regardless of
# it. Two concurrent runs would otherwise share kitty's run/ socket directory.
LIBGL_ALWAYS_SOFTWARE=1 \
    env -u SHUX_SOCKET \
    HOME="${runtime}" \
    XDG_RUNTIME_DIR="${runtime}" \
    XDG_CONFIG_HOME="${config_home}" \
    XDG_STATE_HOME="${state_home}" \
    XDG_CACHE_HOME="${runtime}/cache" \
    XDG_DATA_HOME="${runtime}/data" \
    kitty --config NONE \
    -o font_family="DejaVu Sans Mono" \
    -o font_size=12 \
    -o remember_window_size=no \
    -o initial_window_width="${PHASE_SIZES[0]%x*}" \
    -o initial_window_height="${PHASE_SIZES[0]#*x}" \
    -o cursor_blink_interval=0 \
    -e bash "${launch_sh}" "${go_file}" "${launch_payload}" "${inject_log}" \
    "${shux_bin}" "${session}" >"${kitty_log}" 2>&1 &
kitty_pid=$!

# ── Helpers ─────────────────────────────────────────────────────────────────

# Photograph the X root window — the whole screen, not just kitty's window, so
# anything painted OUTSIDE the emulator is in the picture too.
#
# Bounded: measured, `import` against a SIGSTOPped X server never returns, and a
# hung rig gets SIGKILLed by whatever is above it, which is the one path where
# the trap does not run and both servers leak. `status=$?` is captured on the
# first line of the failure branch, before any diagnostic can overwrite it.
shot() {
    local path="$1" status=0
    if [ -n "${xvfb_pid}" ] && ! kill -0 "${xvfb_pid}" 2>/dev/null; then
        fail "the X server died before this capture (log: ${out_dir}/xvfb.log)"
    fi
    shux_harness_timeout 20s import -window root "${path}" \
        2>>"${out_dir}/import.log" || status=$?
    if [ "${status}" -eq 124 ]; then
        fail "X server unresponsive: import timed out capturing ${path}"
    fi
    if [ "${status}" -ne 0 ]; then
        fail "import failed with status ${status} (log: ${out_dir}/import.log)"
    fi
    [ -s "${path}" ] || fail "import wrote an empty file: ${path}"
}

# The pane's own idea of its grid, and where the payload block sits in it.
# `pane capture --format json` is shux's model, which is one half of the
# cross-path assertion; the other half is measured off the photograph.
pane_state() {
    sx --format json pane capture -s "${session}" -p "${pane_id}" --lines 60 |
        python3 -c '
import json, sys
d = json.load(sys.stdin)
lines = d["text"].split("\n")
rows = [i for i, ln in enumerate(lines) if "~" in ln]
if not rows:
    print("nan nan nan nan nan nan")
    raise SystemExit(0)
cols = [ (ln.index("~"), ln.rindex("~")) for ln in lines if "~" in ln ]
print(d["cols"], d["rows"], min(r for r in rows), max(r for r in rows),
      min(c[0] for c in cols), max(c[1] for c in cols))
'
}

# Ask X how big the window actually is, rather than trusting the size that was
# requested: `xdotool windowsize` is a request, and what the server granted is
# the number every geometry assertion below is made against.
window_geometry() {
    local wid="$1" geom status=0
    geom="$(xdotool getwindowgeometry --shell "${wid}" 2>&1)" || status=$?
    if [ "${status}" -ne 0 ]; then
        fail "xdotool getwindowgeometry ${wid} failed (${status}): ${geom}"
    fi
    printf '%s\n' "${geom}" |
        awk -F= '/^WIDTH=/ {w=$2} /^HEIGHT=/ {h=$2} END {print w+0, h+0}'
}

# kitty maps a short-lived window while it brings up GL, and it is gone by the
# time the first frame is drawn. Taking the first id `xdotool search` ever
# returns hands you that one, and every later call against it dies with a
# BadWindow abort inside xdotool — measured: the id found at t+0.3s was destroyed
# before the first phase. So resolve AFTER the chrome is on screen, take the
# largest window that still answers a geometry query, and reject anything too
# small to be a terminal.
resolve_window() {
    local wid geom w h area best="" best_area=0 status found=0 seen=""
    while read -r wid; do
        [ -n "${wid}" ] || continue
        status=0
        geom="$(xdotool getwindowgeometry --shell "${wid}" 2>/dev/null)" || status=$?
        [ "${status}" -eq 0 ] || continue
        w="$(printf '%s\n' "${geom}" | awk -F= '/^WIDTH=/ {print $2+0}')"
        h="$(printf '%s\n' "${geom}" | awk -F= '/^HEIGHT=/ {print $2+0}')"
        area=$((w * h))
        if [ "${w}" -ge 200 ] && [ "${h}" -ge 200 ]; then
            found=$((found + 1))
            seen="${seen} ${wid}(${w}x${h})"
            if [ "${area}" -gt "${best_area}" ]; then
                best="${wid}"
                best_area="${area}"
            fi
        fi
    done < <(xdotool search --onlyvisible --class kitty 2>/dev/null)
    [ -n "${best}" ] || fail "no usable kitty window on ${display} (log: ${kitty_log})"
    if [ "${found}" -ne 1 ]; then
        fail "expected exactly one usable kitty window on ${display}, found ${found}:${seen}"
    fi
    printf '%s\n' "${best}"
}

# ── P5: kitty accepts unknown `-o` keys silently and exits 0, so a typo or a
# rename in a newer kitty moves the window under you with nothing reported. Both
# halves are checked: the message kitty logs, and the geometry X actually granted.
assert_requested_geometry() {
    local wid="$1" want="$2" geom got_w got_h
    if grep -q 'Ignoring unknown config key' "${kitty_log}" 2>/dev/null; then
        fail "kitty ignored a config key this rig depends on: $(grep -m3 'Ignoring unknown config key' "${kitty_log}")"
    fi
    geom="$(window_geometry "${wid}")"
    got_w="${geom%% *}"
    got_h="${geom##* }"
    if [ "${got_w}" != "${want%x*}" ] || [ "${got_h}" != "${want#*x}" ]; then
        fail "kitty window is ${got_w}x${got_h}, not the ${want} this rig asked for"
    fi
}

# Require content, THEN settle. `wait-settled` alone races a slow starter and
# photographs a blank screen that every later assertion then agrees with, and a
# frame that merely exists proves nothing: this waits for shux's chrome to be
# detectable by the SAME rectangle finder the assertions use.
# Require content, THEN settle — and settle on the PICTURE, not on the pane's
# byte stream. `wait-settled` alone races a slow starter and photographs a blank
# screen that every later assertion then agrees with; and kitty repaints a resize
# over several frames, so a capture taken mid-flight carries a border rectangle
# belonging to neither the old geometry nor the new one (measured: one frame held
# the old box with its origin a pixel over). Two consecutive captures agreeing on
# the rectangle is what "settled" means here. It deliberately reuses the SAME
# detector the assertions use, so the thing waited for is the thing measured.
wait_for_stable_chrome() {
    local attempts="$1" gap="$2"
    local probe="${runtime}/probe.png"
    local last="" now status i
    for i in $(seq 1 "${attempts}"); do
        kill -0 "${kitty_pid}" 2>/dev/null ||
            fail "kitty exited while its chrome was settling: $(cat "${kitty_log}")"
        shot "${probe}"
        status=0
        now="$("${verdict_py}" --probe "${probe}" --border-rgb "${BORDER_RGB}" 2>"${runtime}/probe.err")" ||
            status=$?
        # Exit 2 is a usage error, not "not drawn yet". Retrying it spins out the
        # whole budget and then reports that shux never drew its chrome.
        if [ "${status}" -eq 2 ]; then
            fail "the comparator rejected the probe arguments: $(cat "${runtime}/probe.err")"
        fi
        if [ "${status}" -eq 0 ] && [ -n "${now}" ] && [ "${now}" = "${last}" ]; then
            return 0
        fi
        last="${now}"
        sleep "${gap}"
    done
    if [ -z "${last}" ]; then
        cp -f "${probe}" "${out_dir}/no-chrome.png" 2>/dev/null || true
        fail "no border rectangle ever appeared in the emulator — either shux drew no chrome, or the window is larger than the screen and X clipped it (last frame: ${out_dir}/no-chrome.png)"
    fi
    fail "the border rectangle never stopped moving (last: ${last})"
}

# After a window resize, wait for shux to have RESIZED THE PANE rather than for a
# wall-clock guess: the daemon's own report of the grid is the event, and it is
# also the number every geometry assertion is made against.
wait_for_grid_change() {
    local before="$1" now
    for _ in $(seq 1 60); do
        now="$(pane_state | cut -d' ' -f1,2)"
        if [ "${now}" != "${before}" ] && [ "${now}" != "nan nan" ]; then
            printf '%s\n' "${now}"
            return 0
        fi
        sleep 0.25
    done
    fail "pane grid stayed ${before} after the emulator window was resized"
}

phase_records="${runtime}/phases.jsonl"
: >"${phase_records}"

record_phase() {
    local name="$1" require_image="$2"
    local geom state win_w win_h cols rows row0 row1 col0 col1
    wait_for_stable_chrome 40 0.3
    geom="$(window_geometry "${window_id}")"
    win_w="${geom%% *}"
    win_h="${geom##* }"
    state="$(pane_state)"
    read -r cols rows row0 row1 col0 col1 <<<"${state}"
    [ "${cols}" != "nan" ] || fail "phase ${name}: the payload block is not in the pane's text"

    local frames=()
    local i path
    for i in $(seq 1 "${frames_per_phase}"); do
        path="$(printf '%s/f_%s_%02d.png' "${out_dir}" "${name}" "${i}")"
        shot "${path}"
        frames+=("${path}")
        sleep 0.4
    done

    # shux's own render of the same pane at the same moment: the evidence side by
    # side with what the emulator drew.
    sx pane snapshot -s "${session}" -p "${pane_id}" --output \
        "${out_dir}/shux_${name}.png" >/dev/null 2>&1 ||
        fail "phase ${name}: pane snapshot failed"

    python3 -c '
import json, sys
name, win_w, win_h, cols, rows, r0, r1, c0, c1, req = sys.argv[1:11]
print(json.dumps({
    "name": name,
    "window": {"w": int(win_w), "h": int(win_h)},
    "pane": {"cols": int(cols), "rows": int(rows)},
    "status_rows": 1,
    "block": {"row0": int(r0), "row1": int(r1), "col0": int(c0), "col1": int(c1)},
    "require_image": req == "1",
    "frames": sys.argv[11:],
}))
' "${name}" "${win_w}" "${win_h}" "${cols}" "${rows}" "${row0}" "${row1}" \
        "${col0}" "${col1}" "${require_image}" "${frames[@]}" >>"${phase_records}"

    echo "    ${name}: window ${win_w}x${win_h}, pane ${cols}x${rows} cells, ${frames_per_phase} frames"
}

# ── The run ─────────────────────────────────────────────────────────────────

wait_for_stable_chrome 60 0.5
# A short quiet window on top of an assertion that has already passed: the chrome
# is up, this only lets the first paint finish.
sx pane wait-settled "${pane_id}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
window_id="$(resolve_window)"
window_owner="$(xdotool getwindowpid "${window_id}" 2>/dev/null || true)"
if [ "${window_owner}" != "${kitty_pid}" ]; then
    fail "window ${window_id} belongs to pid ${window_owner:-unknown}, not this rig's kitty (${kitty_pid})"
fi
assert_requested_geometry "${window_id}" "${PHASE_SIZES[0]}"
echo "    kitty window ${window_id} (pid ${kitty_pid})"

if [ "${scenario}" = "plain" ]; then
    grid="$(pane_state | cut -d' ' -f1,2)"
    for idx in "${!PHASE_NAMES[@]}"; do
        name="${PHASE_NAMES[${idx}]}"
        size="${PHASE_SIZES[${idx}]}"
        if [ "${idx}" -gt 0 ]; then
            xdotool windowsize "${window_id}" "${size%x*}" "${size#*x}"
            grid="$(wait_for_grid_change "${grid}")"
            assert_requested_geometry "${window_id}" "${size}"
            sx pane wait-settled "${pane_id}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
        fi
        record_phase "${name}" 0
    done
else
    record_phase "initial" 0

    inject_state="$(pane_state)"
    read -r cols rows _ _ _ _ <<<"${inject_state}"
    if [ "${cols}" -lt 20 ] || [ "${rows}" -lt 16 ]; then
        fail "pane ${cols}x${rows} is too small to place an injection payload in"
    fi

    if [ "${scenario}" = "image-overflow" ] || [ "${scenario}" = "image-pane" ]; then
        # Natural pixel size from a cell near the bottom-right of the pane. At
        # this font 320x240 px is roughly 32x13 cells, so it runs off the right
        # border and down through the status bar — the #175 defect.
        #
        # `image-pane` uses the SAME unbounded payload deliberately. Sent to the
        # emulator it overflows; sent to a PANE it must not, because shux clips
        # it into a source rectangle before re-emitting. Identical bytes, and
        # the only difference is who emits them.
        "${payload_py}" --rgb "${IMAGE_RGB}" --px 320x240 \
            --at "$((rows - 2)),$((cols - 8))" --out "${payload_file}"
    else
        # The fix: a destination box in CELLS. kitty scales the same image into
        # 10x5 cells and it cannot leave them.
        "${payload_py}" --rgb "${IMAGE_RGB}" --px 320x240 \
            --at "9,4" --cell-box 10x5 --out "${payload_file}"
    fi

    if [ "${scenario}" = "image-pane" ]; then
        # The workload is waiting on this receipt. From here the bytes are pane
        # OUTPUT, so everything that puts them on screen is shux's own emit.
        : >"${payload_file}.ready"
        sx pane wait-settled "${pane_id}" --quiet 400 --timeout 15000 >/dev/null 2>&1 || true
        record_phase "pane" 1
    else
        : >"${go_file}"
        for _ in $(seq 1 80); do
            [ -e "${go_file}.done" ] && break
            kill -0 "${kitty_pid}" 2>/dev/null ||
                fail "kitty exited during injection: $(cat "${kitty_log}")"
            sleep 0.25
        done
        [ -e "${go_file}.done" ] ||
            fail "the injector never wrote its receipt (log: ${inject_log})"
        sleep 1

        record_phase "inject" 1
    fi
fi

# ── Verdict ─────────────────────────────────────────────────────────────────
python3 -c '
import json, sys
phases = [json.loads(line) for line in open(sys.argv[2]) if line.strip()]
json.dump({
    "border_rgb": [int(c) for c in sys.argv[3].split(",")],
    "status_rgb": [int(c) for c in sys.argv[4].split(",")],
    "content_rgb": [int(c) for c in sys.argv[5].split(",")],
    "image_rgb": [int(c) for c in sys.argv[6].split(",")],
    # The other half of each palette that bounds what may legitimately appear
    # outside the pane: white, because every status colour is pinned to it by the
    # config above, and black, the kitty default background — which is both the
    # terminal behind the border ring and the desktop behind the window.
    "status_fg_rgb": [255, 255, 255],
    "background_rgb": [0, 0, 0],
    # The truecolor / indexed / basic probes AS KITTY RENDERS THEM: the
    # truecolor triple verbatim, indexed 208 and SGR 34 from the default palette
    # of kitty 0.32.2. Measured off a real frame at 103, 92 and 50 px. A kitty
    # whose palette differs fails the probe assertion loudly, which is the right
    # outcome: the numbers are then wrong for that emulator and must be re-pinned.
    "probe_rgb": [[120, 220, 180], [255, 135, 0], [13, 115, 204]],
    "scenario": sys.argv[7],
    "phases": phases,
}, open(sys.argv[1], "w"), indent=2)
' "${geometry_json}" "${phase_records}" "${BORDER_RGB}" "${STATUS_RGB}" \
    "${CONTENT_RGB}" "${IMAGE_RGB}" "${scenario}"

echo "▶ verdict"
verdict_status=0
"${verdict_py}" --geometry "${geometry_json}" || verdict_status=$?
# Exit 2 is the comparator saying it could not read the file, which is a broken
# input and not a rendering defect. Collapsing the two here would throw away the
# whole point of the distinction: the reader would be told shux painted outside
# the box because a key was missing from a JSON file this script wrote.
if [ "${verdict_status}" -eq 2 ]; then
    echo "✗ gui_terminal_check: ${geometry_json} is not readable by the comparator." >&2
    echo "  Nothing was judged; this is a rig defect, not a shux one." >&2
    exit 2
fi
if [ "${verdict_status}" -ne 0 ]; then
    fail "gui_terminal_check: the emulator's frames failed the assertions (exit ${verdict_status})"
fi
echo "✓ gui_terminal_check: ${scenario} — what kitty drew matches what shux says it drew"
