#!/usr/bin/env bash
# The command kitty runs: attach to a shux session, and — when the rig's
# self-test asks for it — inject raw terminal bytes alongside the attach client.
#
#   kitty_attach_launch.sh <go-file> <payload-file|-> <log> <shux-bin> <session>
#
# A sidecar rather than a pane, because shux's VT parser has no APC handling:
# kitty graphics written INTO a pane are swallowed and never reach the outer
# terminal. Sharing the emulator's controlling terminal is the only way to put
# them on the screen the attach client is drawing on.
#
# `<go>` is the rig's starting gun — the payload's cursor position depends on a
# pane geometry that is not known until the client has attached and resized.
# `<go>.done` is the injector's receipt: the rig waits for it rather than
# sleeping, so a payload that never got written fails the run instead of quietly
# producing a frame with no image in it.

set -euo pipefail

go_file="${1:?go-file}"
payload="${2:?payload file, or - for no injection}"
log="${3:?log file}"
shux_bin="${4:?shux binary}"
session="${5:?session name}"

if [ "${payload}" != "-" ]; then
    (
        set -euo pipefail
        deadline=$((SECONDS + 120))
        while [ ! -e "${go_file}" ]; do
            if [ "${SECONDS}" -ge "${deadline}" ]; then
                echo "inject: go-file ${go_file} never appeared" >&2
                exit 1
            fi
            sleep 0.1
        done
        # /dev/tty is the emulator's own terminal, shared with the attach client
        # in the foreground. No `|| true`: a write that fails must leave the
        # receipt unwritten so the rig notices.
        cat "${payload}" >/dev/tty
        : >"${go_file}.done"
    ) >>"${log}" 2>&1 &
fi

exec "${shux_bin}" session attach "${session}"
