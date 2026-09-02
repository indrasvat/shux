#!/usr/bin/env bash
# The pane workload the GUI-terminal rig photographs.
#
#   gui_terminal_workload.sh <block-rgb> <block-cols> <block-rows> <marker> [payload]
#
# Prints, in this order and nothing else:
#
#   1. A solid block of `~` on a `block-rgb` BACKGROUND. A background run paints
#      every pixel of every cell it covers, so its pixel bounding box in a
#      photograph is exactly its cell rect — which is what makes the cross-path
#      assertion possible to the cell. `~` rather than a space so the same block
#      is locatable in `pane capture` text.
#   2. The truecolor, indexed and basic colour probes CLAUDE.md requires of any
#      daemon-backed test that captures pane output. The comparator asserts a
#      pixel count for each, so a run that has lost a colour class cannot pass by
#      drawing the right shapes in grey.
#   3. One marker, printed only once everything above is on the screen — by this
#      script rather than typed into a shell, so nothing can echo it back ahead of
#      the content it gates (#167, #174).
#
# Then it sleeps, because the pane has to still be there when the photograph is
# taken. It never exits on its own.

set -euo pipefail

rgb="${1:?block rgb as R;G;B}"
cols="${2:?block width in cells}"
rows="${3:?block height in cells}"
marker="${4:?completion marker}"
# Optional: raw bytes to emit as PANE OUTPUT after the marker. The pane has to
# PRINT an image for shux's VT to see it; bytes sent to the pane's stdin are a
# command line to the shell, not output.
payload="${5:-}"

fill=""
for ((i = 0; i < cols; i++)); do
    fill+="~"
done

for ((r = 0; r < rows; r++)); do
    printf '\033[48;2;%sm\033[38;2;0;0;0m%s\033[0m\n' "${rgb}" "${fill}"
done

printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'
printf '%s\n' "${marker}"

# The rig cannot build the payload until it knows the pane's size, which it
# cannot know until this pane exists. So wait for its receipt rather than for
# the file: a half-written payload would be parsed as a truncated escape.
if [ -n "${payload}" ]; then
    for _ in $(seq 1 960); do
        [ -e "${payload}.ready" ] && break
        sleep 0.25
    done
    if [ -e "${payload}.ready" ]; then
        cat "${payload}"
        # The rig's receipt that the bytes are OUT. Without it its only
        # synchronisation was `wait-settled`, which measures quiet since the
        # last mutation -- and the pane's last mutation was the marker, tens of
        # seconds earlier, so it returned settled before this ran.
        : >"${payload}.emitted"
    else
        # NOT to stderr: this script's stderr is pane output, so the message
        # would land in the photographed grid and shift the block the
        # comparator measures, reporting a rig fault as a rendering one.
        printf 'workload: %s.ready never appeared\n' "${payload}" \
            >"${payload}.workload-error"
    fi
fi

while true; do
    sleep 3600
done
