#!/usr/bin/env bash
# A picture cropped and restored by a pane's horizontal scroll, in a real GUI
# terminal (issue #190).
#
#   SHUX_BIN=<binary> .shux/scripts/gui_terminal_image_crop.sh [--out DIR]
#
# One kitty window attaches to one pane pinned wider than its rect. The pane
# draws three banded pictures: one a whole number of cells per band, one scaled
# so its band edges fall inside cells, and one upscaled so the slice's edge
# cuts inside one of its source pixels. Its cursor then moves right, so the
# slice starts past every picture's left edge, and back home. Photographed at
# each step, the cropped frame must show each picture's trailing whole source
# pixels where they truly begin, at their widths, and nothing of any picture
# outside the pane; the restored frame must draw the pane as the first one did. The rest of
# the rig: docs/agents/visual-testing.md § "The GUI-terminal rig".
#
# Exit: 0 pass · 1 an assertion failed · 2 usage · 3 a tool is missing.
# Output: .shux/out/gui-terminal/image-crop/. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
# shellcheck source=lib/shux_harness.sh disable=SC1091  # path is runtime-resolved
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"
cd "${repo_root}"

verdict_py="${repo_root}/.shux/scripts/lib/kitty_image_crop_verdict.py"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
out_dir="${repo_root}/.shux/out/gui-terminal/image-crop"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --out)
            out_dir="${2:?--out needs a value}"
            shift 2
            ;;
        -h | --help)
            sed -n '2,17p' "$0"
            exit 0
            ;;
        *)
            echo "gui_terminal_image_crop: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

BORDER_RGB="255,0,0"
STATUS_RGB="0,0,255"
# Picture A: four 9 px bands, one declared cell each, drawn at its own size.
A_BANDS="0,200,0 230,230,0 0,200,200 255,140,0"
# Picture B: four 5 px bands of a 20 px image fitted to 5 cells, 1.25 each.
B_BANDS="160,0,200 255,120,180 120,70,20 90,160,255"
# Picture C: four 1 px bands of a 4x2 image fitted to 5 columns, 1.25 each.
C_BANDS="0,90,0 230,120,40 120,0,120 0,150,150"
A_AT="5 10"
B_AT="10 8"
C_AT="14 9"
B_COLS=5
C_COLS=5
# The scrolled slice starts this many columns into the grid: one cell into A;
# three into B, so B's first visible band is three quarters of a cell wide;
# two into C, inside its second source pixel, so C shows from its third pixel,
# half a cell in, and the cut sliver is blank.
OFFSET=11
GRID_COLS=200
GRID_ROWS=20
TITLE="shux-gui-rig"
SCREEN_W=1400
SCREEN_H=900
WINDOW="1000x880"
# Cells near twice shux's declared 9x19 px, so an offset sent in declared
# pixels lands visibly short.
FONT_SIZE=20

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
    echo "✗ gui_terminal_image_crop: cannot run — nothing was checked." >&2
    printf '    %s\n' "${missing[@]}" >&2
    exit 3
fi

runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-guiterm3.XXXXXX")"
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

session="guiterm-crop-$$"
xvfb_pid=""
kitty_pid=""

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
    reap "${kitty_pid}" || true
    shux_harness_kill_session "${runtime}" "${shux_bin}" "${session}" || true
    shux_harness_stop_daemon "${runtime}" || true
    reap "${xvfb_pid}" || true
    local leaked="" pid
    for pid in "${kitty_pid}" "${xvfb_pid}"; do
        if [ -n "${pid}" ] && kill -0 "${pid}" 2>/dev/null; then
            leaked="${leaked} ${pid}"
        fi
    done
    rm -rf "${runtime}" || true
    if [ -n "${leaked}" ]; then
        echo "✗ gui_terminal_image_crop: left processes running:${leaked}" >&2
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

echo "▶ GUI-terminal rig — a picture cropped and restored by a horizontal scroll"
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

# The pictures, as the pane's application sends them: A transmitted and shown
# at its own size, B transmitted under an id and put fitted to B_COLS columns.
payload="${runtime}/pictures.esc"
python3 - "${payload}" "${A_BANDS}" "${B_BANDS}" "${C_BANDS}" "${A_AT}" "${B_AT}" "${C_AT}" \
    "${B_COLS}" "${C_COLS}" <<'PY'
import base64, sys
out, a_bands, b_bands, c_bands, a_at, b_at, c_at, b_cols, c_cols = sys.argv[1:]
def bands(spec, band_w, h):
    cols = [bytes(int(c) for c in rgb.split(",")) for rgb in spec.split()]
    row = b"".join(c * band_w for c in cols)
    return base64.b64encode(row * h).decode(), band_w * len(cols), h
def cup(at):
    r, c = (int(v) for v in at.split())
    return f"\x1b[{r + 1};{c + 1}H"
a, aw, ah = bands(a_bands, 9, 19)
b, bw, bh = bands(b_bands, 5, 20)
c, cw, chh = bands(c_bands, 1, 2)
with open(out, "w") as f:
    f.write(cup(a_at) + f"\x1b_Ga=T,f=24,s={aw},v={ah},C=1;{a}\x1b\\")
    f.write(f"\x1b_Ga=t,f=24,s={bw},v={bh},i=7;{b}\x1b\\")
    f.write(cup(b_at) + f"\x1b_Ga=p,i=7,c={b_cols},C=1\x1b\\")
    f.write(f"\x1b_Ga=t,f=24,s={cw},v={chh},i=8;{c}\x1b\\")
    f.write(cup(c_at) + f"\x1b_Ga=p,i=8,c={c_cols},C=1\x1b\\")
PY

# Told to go once its grid is pinned, the pane draws the pictures, prints a
# marker and parks its cursor at home; then each line it reads moves its cursor
# to that column of the top row.
marker="CROP-READY-${RANDOM}${RANDOM}"
workload="${runtime}/workload.sh"
cat >"${workload}" <<WORKLOAD
stty -echo
read -r _
printf '\\033[2J'
cat '${payload}'
printf '\\033[18;1H%s\\033[H' '${marker}'
while read -r col; do
    printf '\\033[1;%sH' "\${col}"
done
WORKLOAD
sx session create "${session}" -d --title "${TITLE}" -- \
    env TERM=xterm-256color COLORTERM=truecolor bash "${workload}" >/dev/null
pane="$(sx --format json pane list -s "${session}" |
    python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,list) else d["panes"])[0]["id"])')"
# Pinned wider than any rect the window gives it, so the slice can scroll.
sx pane set-size -s "${session}" -p "${pane}" --cols "${GRID_COLS}" --rows "${GRID_ROWS}" >/dev/null
sx pane send-keys -s "${session}" -p "${pane}" --data DQ== >/dev/null
sx pane wait-for -s "${session}" -p "${pane}" -t "${marker}" --timeout-ms 30000 >/dev/null ||
    fail "the pane never drew its pictures"

LIBGL_ALWAYS_SOFTWARE=1 env -u SHUX_SOCKET HOME="${runtime}" XDG_RUNTIME_DIR="${runtime}" \
    XDG_CONFIG_HOME="${config_home}" XDG_STATE_HOME="${state_home}" \
    XDG_CACHE_HOME="${runtime}/cache" XDG_DATA_HOME="${runtime}/data" \
    kitty --config NONE -o font_family="DejaVu Sans Mono" -o font_size="${FONT_SIZE}" \
    -o remember_window_size=no -o initial_window_width="${WINDOW%x*}" \
    -o initial_window_height="${WINDOW#*x}" -o cursor_blink_interval=0 \
    -e "${shux_bin}" session attach "${session}" >"${out_dir}/kitty.log" 2>&1 &
kitty_pid=$!

win=""
for _ in $(seq 1 100); do
    win="$(xdotool search --onlyvisible --pid "${kitty_pid}" 2>/dev/null | tail -1 || true)"
    [ -n "${win}" ] && break
    sleep 0.2
done
[ -n "${win}" ] || fail "kitty mapped no window"
xdotool windowmove "${win}" 0 0

client_cols() {
    sx --format json client list 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
cs = d if isinstance(d, list) else d.get("clients", [])
print(cs[0]["cols"] if len(cs) == 1 else "")'
}
cols=""
for _ in $(seq 1 100); do
    cols="$(client_cols || true)"
    [ -n "${cols}" ] && break
    sleep 0.2
done
[ -n "${cols}" ] || fail "kitty never attached"
# One pane in a rounded outline: its rect is two columns narrower.
rect_cols=$((cols - 2))
[ "${rect_cols}" -lt "${GRID_COLS}" ] || fail "a ${rect_cols}-column rect shows the whole ${GRID_COLS}-column grid"

shot() {
    shux_harness_timeout 20s import -window root "$1" 2>>"${out_dir}/import.log" ||
        fail "import failed capturing $1"
    [ -s "$1" ] || fail "import wrote an empty file: $1"
}

# Two captures that agree, after one that differs from `prev` when given: kitty
# has drawn the change, and finished drawing it.
settle() {
    local name="$1" prev="${2:-}" a="${runtime}/settle-a.png" b="${runtime}/settle-b.png"
    shot "${a}"
    if [ -n "${prev}" ]; then
        for _ in $(seq 1 60); do
            "${verdict_py}" --same "${a}" "${prev}" >/dev/null 2>&1 || break
            sleep 0.4
            shot "${a}"
        done
        "${verdict_py}" --same "${a}" "${prev}" >/dev/null 2>&1 &&
            fail "the screen never changed before ${name}"
    fi
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

move_cursor() {
    sx pane send-keys -s "${session}" -p "${pane}" --text "$1" >/dev/null
    sx pane send-keys -s "${session}" -p "${pane}" --data DQ== >/dev/null
}

settle "whole"
# The slice's first column is OFFSET once the cursor sits on its last one.
move_cursor "$((OFFSET + rect_cols))"
settle "cropped" "${out_dir}/whole.png"
move_cursor 1
settle "restored" "${out_dir}/cropped.png"

read -r wx wy ww wh <<<"$(xdotool getwindowgeometry --shell "${win}" |
    awk -F= '/^X=/ {x=$2} /^Y=/ {y=$2} /^WIDTH=/ {w=$2} /^HEIGHT=/ {h=$2} END {print x+0, y+0, w+0, h+0}')"
python3 - "${out_dir}/run.json" "${out_dir}" <<PY
import json, sys
def rgbs(spec):
    return [[int(c) for c in rgb.split(",")] for rgb in spec.split()]
out, frames = sys.argv[1:]
json.dump({
    "border_rgb": [int(c) for c in "${BORDER_RGB}".split(",")],
    "status_rgb": [int(c) for c in "${STATUS_RGB}".split(",")],
    "pictures": {
        "A": {"bands": rgbs("${A_BANDS}"), "cells_per_band": 1.0, "cells_per_px": 1 / 9,
              "at": [int(v) for v in "${A_AT}".split()]},
        "B": {"bands": rgbs("${B_BANDS}"), "cells_per_band": ${B_COLS} / 4, "cells_per_px": ${B_COLS} / 20,
              "at": [int(v) for v in "${B_AT}".split()], "edge_px": 1},
        "C": {"bands": rgbs("${C_BANDS}"), "cells_per_band": ${C_COLS} / 4, "cells_per_px": ${C_COLS} / 4,
              "at": [int(v) for v in "${C_AT}".split()], "edge_px": 1},
    },
    "offset": ${OFFSET},
    "window": {"x": ${wx}, "y": ${wy}, "w": ${ww}, "h": ${wh}},
    "whole": f"{frames}/whole.png",
    "cropped": f"{frames}/cropped.png",
    "restored": f"{frames}/restored.png",
}, open(out, "w"), indent=2)
PY

echo "▶ verdict"
verdict_status=0
"${verdict_py}" --geometry "${out_dir}/run.json" || verdict_status=$?
[ "${verdict_status}" -eq 0 ] ||
    fail "gui_terminal_image_crop: the frames failed the assertions (exit ${verdict_status})"
echo "✓ gui_terminal_image_crop: every picture crops at its first whole source pixel, stays inside the pane, and comes back whole"
