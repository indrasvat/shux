#!/usr/bin/env bash
# Two real GUI terminals at different sizes on one split session (issue #190).
#
#   SHUX_BIN=<binary> .shux/scripts/gui_terminal_two_clients.sh [--out DIR]
#
# A large kitty attaches first and drives; a small one attaches second and only
# views, so every pane keeps the large client's size and is cropped in the small
# one. Photographed, the small window must show each pane's top-left with its
# `⤡ W×H` label on the border and nothing of a pane past its border; the large
# window must not change when the small one attaches. The rest of the rig, and
# what it cannot see: docs/agents/visual-testing.md § "The GUI-terminal rig".
#
# Exit: 0 pass · 1 an assertion failed · 2 usage · 3 a tool is missing.
# Output: .shux/out/gui-terminal/two-clients/. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
# shellcheck source=lib/shux_harness.sh disable=SC1091  # path is runtime-resolved
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
cd "${repo_root}"

verdict_py="${repo_root}/.shux/scripts/lib/kitty_two_clients_verdict.py"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
out_dir="${repo_root}/.shux/out/gui-terminal/two-clients"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --out)
            out_dir="${2:?--out needs a value}"
            shift 2
            ;;
        -h | --help)
            sed -n '2,14p' "$0"
            exit 0
            ;;
        *)
            echo "gui_terminal_two_clients: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

BORDER_RGB="255,0,0"
LEFT_RGB="255,0,212"
RIGHT_RGB="0,255,135"
BLOCK_COLS=20
BLOCK_ROWS=4
TITLE="shux-gui-rig"
SCREEN_W=1900
SCREEN_H=1000
LARGE="1100x700"
SMALL="640x400"

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
probe_tool verdict "${verdict_py}" --help
[ -x "${shux_bin}" ] || missing+=("missing: ${shux_bin} — build it with 'make release'")
if [ "${#missing[@]}" -gt 0 ]; then
    echo "✗ gui_terminal_two_clients: cannot run — nothing was checked." >&2
    printf '    %s\n' "${missing[@]}" >&2
    exit 3
fi

runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-guiterm2.XXXXXX")"
config_home="${runtime}/config"
state_home="${runtime}/state"
mkdir -p "${config_home}/shux" "${state_home}/shux" "${out_dir}"
rm -f "${out_dir}"/*.png "${out_dir}"/*.json "${out_dir}"/*.log 2>/dev/null || true

cat >"${config_home}/shux/config.toml" <<TOML
[appearance]
border_style = "rounded"
nerd_fonts = false

[theme]
border_focused = "#ff0000"
border_unfocused = "#ff0000"
status_bg = "#0000ff"
status_fg = "#ffffff"
status_accent = "#ffffff"
status_muted = "#ffffff"
status_branch = "#ffffff"
status_attention = "#ffffff"
status_shared = "#ffffff"
TOML
printf '{"prefix_discovered":true,"welcome_toast_seen":true}\n' \
    >"${state_home}/shux/onboarding.json"

session="guiterm-two-$$"
xvfb_pid=""
large_pid=""
small_pid=""

# TERM, wait, then KILL, by the pid recorded at launch.
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

# Teardown, so every step tolerates a process already gone.
cleanup() {
    local status=$?
    reap "${small_pid}" || true
    reap "${large_pid}" || true
    shux_harness_kill_session "${runtime}" "${shux_bin}" "${session}" || true
    shux_harness_stop_daemon "${runtime}" || true
    reap "${xvfb_pid}" || true
    local leaked="" pid
    for pid in "${small_pid}" "${large_pid}" "${xvfb_pid}"; do
        if [ -n "${pid}" ] && kill -0 "${pid}" 2>/dev/null; then
            leaked="${leaked} ${pid}"
        fi
    done
    rm -rf "${runtime}" || true
    if [ -n "${leaked}" ]; then
        echo "✗ gui_terminal_two_clients: left processes running:${leaked}" >&2
        exit 1
    fi
    exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

fail() {
    echo "✗ $*" >&2
    exit 1
}

sx() {
    env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${config_home}" \
        XDG_STATE_HOME="${state_home}" "${shux_bin}" "$@"
}

echo "▶ GUI-terminal rig — two clients on one split session"
echo "    shux:  $(${shux_bin} version 2>/dev/null | head -1)"
echo "    out:   ${out_dir}"

Xvfb -displayfd 3 -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp -noreset \
    >"${out_dir}/xvfb.log" 2>&1 3>"${runtime}/display" &
xvfb_pid=$!
for _ in $(seq 1 100); do
    [ -s "${runtime}/display" ] && break
    kill -0 "${xvfb_pid}" 2>/dev/null || fail "Xvfb exited during startup: $(cat "${out_dir}/xvfb.log")"
    sleep 0.1
done
[ -s "${runtime}/display" ] || fail "Xvfb never reported a display number"
DISPLAY=":$(tr -cd '0-9' <"${runtime}/display")"
export DISPLAY

# Each pane paints a solid block at its top-left, prints a marker below it, and
# parks its cursor at home: the crop a smaller client shows follows the cursor,
# so it must show the block whole.
block() {
    local rgb="$1" marker="$2" script="${runtime}/block-$1.sh"
    cat >"${script}" <<BLOCK
draw() {
    printf '\\033[2J\\033[H'
    for _ in \$(seq 1 ${BLOCK_ROWS}); do
        printf '\\033[48;2;${rgb//,/;}m%*s\\033[0m\\r\\n' ${BLOCK_COLS} ''
    done
    printf '%s' '${marker}'
    printf '\\033[H'
}
# Drawn again at each size: a resize does not keep a blank cell's colour.
trap draw WINCH
draw
while :; do sleep 100000 & wait \$!; done
BLOCK
    printf '%s\n' "${script}"
}
left_marker="LEFT-READY-${RANDOM}"
right_marker="RIGHT-READY-${RANDOM}"
sx session create "${session}" -d --title "${TITLE}" -- \
    env TERM=xterm-256color COLORTERM=truecolor bash "$(block "${LEFT_RGB}" "${left_marker}")" >/dev/null
left_pane="$(sx --format json pane list -s "${session}" | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,list) else d["panes"])[0]["id"])')"
sx pane split -s "${session}" -p "${left_pane}" -d vertical -- \
    env TERM=xterm-256color COLORTERM=truecolor bash "$(block "${RIGHT_RGB}" "${right_marker}")" >/dev/null
sx pane wait-for -s "${session}" -p "${left_pane}" -t "${left_marker}" --timeout-ms 30000 >/dev/null ||
    fail "the left pane never printed its block"

launch_kitty() {
    local size="$1" log="$2"
    LIBGL_ALWAYS_SOFTWARE=1 env -u SHUX_SOCKET HOME="${runtime}" XDG_RUNTIME_DIR="${runtime}" \
        XDG_CONFIG_HOME="${config_home}" XDG_STATE_HOME="${state_home}" \
        XDG_CACHE_HOME="${runtime}/cache" XDG_DATA_HOME="${runtime}/data" \
        kitty --config NONE -o font_family="DejaVu Sans Mono" -o font_size=12 \
        -o remember_window_size=no -o initial_window_width="${size%x*}" \
        -o initial_window_height="${size#*x}" -o cursor_blink_interval=0 \
        -e "${shux_bin}" session attach "${session}" >"${log}" 2>&1 &
    echo $!
}

# The largest visible window kitty `pid` maps, once it answers a geometry query.
window_of() {
    local pid="$1" wid w h best="" area=0
    for _ in $(seq 1 100); do
        while read -r wid; do
            [ -n "${wid}" ] || continue
            eval "$(xdotool getwindowgeometry --shell "${wid}" 2>/dev/null | grep -E '^(WIDTH|HEIGHT)=')" || continue
            w="${WIDTH:-0}"
            h="${HEIGHT:-0}"
            if [ "$((w * h))" -gt "${area}" ] && [ "${w}" -ge 200 ]; then
                best="${wid}"
                area=$((w * h))
            fi
        done < <(xdotool search --onlyvisible --pid "${pid}" 2>/dev/null)
        if [ -n "${best}" ]; then
            printf '%s\n' "${best}"
            return 0
        fi
        sleep 0.2
    done
    fail "kitty pid ${pid} mapped no usable window"
}

shot() {
    shux_harness_timeout 20s import -window root "$1" 2>>"${out_dir}/import.log" ||
        fail "import failed capturing $1"
    [ -s "$1" ] || fail "import wrote an empty file: $1"
}

# Two captures agreeing on what the window region draws: kitty has finished a
# frame, not just started one.
settle() {
    local name="$1" a="${runtime}/settle-a.png" b="${runtime}/settle-b.png"
    shot "${a}"
    for _ in $(seq 1 60); do
        sleep 0.4
        shot "${b}"
        if "${verdict_py}" --same "${a}" "${b}" >/dev/null 2>&1; then
            cp "${b}" "${out_dir}/${name}.png"
            return 0
        fi
        cp "${b}" "${a}"
    done
    fail "the screen never stopped changing before ${name}"
}

client_count() {
    sx --format json client list 2>/dev/null |
        python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d if isinstance(d,list) else d.get("clients", [])))'
}

large_pid="$(launch_kitty "${LARGE}" "${out_dir}/kitty-large.log")"
large_win="$(window_of "${large_pid}")"
xdotool windowmove "${large_win}" 0 0
for _ in $(seq 1 100); do [ "$(client_count)" = "1" ] && break; sleep 0.2; done
[ "$(client_count)" = "1" ] || fail "the large client never attached"
sx pane wait-settled "${left_pane}" --quiet 300 --timeout 8000 >/dev/null 2>&1 || true
settle "large-alone"

# The grids the large client gave the panes: what the small one's labels name.
left_grid="$(sx --format json pane capture -s "${session}" -p "${left_pane}" --lines 1 |
    python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["cols"], d["rows"])')"

small_pid="$(launch_kitty "${SMALL}" "${out_dir}/kitty-small.log")"
small_win="$(window_of "${small_pid}")"
xdotool windowmove "${small_win}" 1200 0
for _ in $(seq 1 100); do [ "$(client_count)" = "2" ] && break; sleep 0.2; done
[ "$(client_count)" = "2" ] || fail "the small client never attached"
# kitty draws an unfocused window's cursor hollow: give the large one focus back
# so the comparison sees shux's frame, not kitty's focus.
xdotool windowfocus --sync "${large_win}"
settle "both"

geom() {
    xdotool getwindowgeometry --shell "$1" |
        awk -F= '/^X=/ {x=$2} /^Y=/ {y=$2} /^WIDTH=/ {w=$2} /^HEIGHT=/ {h=$2} END {print x+0, y+0, w+0, h+0}'
}
now_grid="$(sx --format json pane capture -s "${session}" -p "${left_pane}" --lines 1 |
    python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["cols"], d["rows"])')"
[ "${now_grid}" = "${left_grid}" ] || fail "the viewer attaching resized the panes: ${left_grid} -> ${now_grid}"

read -r lx ly lw lh <<<"$(geom "${large_win}")"
read -r sx_ sy sw shh <<<"$(geom "${small_win}")"
read -r gcols grows <<<"${left_grid}"
python3 -c '
import json, sys
a = sys.argv
json.dump({
    "border_rgb": [int(c) for c in a[2].split(",")],
    "left_rgb": [int(c) for c in a[3].split(",")],
    "right_rgb": [int(c) for c in a[4].split(",")],
    "block_cols": int(a[5]),
    "block_rows": int(a[6]),
    "title": a[7],
    "left_grid": {"cols": int(a[8]), "rows": int(a[9])},
    "large": {"x": int(a[10]), "y": int(a[11]), "w": int(a[12]), "h": int(a[13])},
    "small": {"x": int(a[14]), "y": int(a[15]), "w": int(a[16]), "h": int(a[17])},
    "before": a[18],
    "after": a[19],
}, open(a[1], "w"), indent=2)
' "${out_dir}/run.json" "${BORDER_RGB}" "${LEFT_RGB}" "${RIGHT_RGB}" "${BLOCK_COLS}" \
    "${BLOCK_ROWS}" "${TITLE}" "${gcols}" "${grows}" "${lx}" "${ly}" "${lw}" "${lh}" \
    "${sx_}" "${sy}" "${sw}" "${shh}" "${out_dir}/large-alone.png" "${out_dir}/both.png"

echo "▶ verdict"
verdict_status=0
"${verdict_py}" --geometry "${out_dir}/run.json" || verdict_status=$?
[ "${verdict_status}" -eq 0 ] ||
    fail "gui_terminal_two_clients: the two windows failed the assertions (exit ${verdict_status})"
echo "✓ gui_terminal_two_clients: the viewer crops each pane to its top-left, labelled, inside its borders; the driver's window is unchanged"
