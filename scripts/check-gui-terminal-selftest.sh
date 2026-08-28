#!/usr/bin/env bash
# scripts/check-gui-terminal-selftest.sh — prove the GUI-terminal rig can FAIL.
#
# A green run from a rig that cannot fail looks exactly like a green run from a
# rig that works, and the spike this rig came out of shipped a false pass on a
# run where the browser had never started and no image existed at all. So every
# case drives the REAL rig or the REAL comparator, never a copy of its logic.
#
# Issue #175 requires three:
#
#   1. A reintroduced overflow — the same payload with and without the
#      destination box in cells (`c=`/`r=`) that was the fix. Without it the rig
#      must go red naming CONTAINMENT:IMAGE, not merely non-zero, which a crashed
#      kitty would also produce. With it the rig must go green, which is what
#      proves the red came from the missing box and not from the injection.
#   2. Empty input — no frames, a missing frame, a black screen, a truncated
#      capture, a frame that lost a colour class, and a phase whose promised
#      image never arrived.
#   3. A missing tool — no kitty, no ImageMagick, no shux binary, no working
#      comparator: say so and exit non-zero, never report success for work not
#      done.
#
# And one the issue does not name: the default `plain` scenario must PASS at all
# four window sizes. Nothing else exercises `xdotool windowsize` or the wait for
# shux to resize the pane, so without it a rig whose resize is a silent no-op is
# green.
#
# Modelled on scripts/check-vt-qa-selftest.sh. Wall clock is dominated by three
# real emulator bring-ups: 62 s measured.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
rig="${repo_root}/.shux/scripts/gui_terminal_check.sh"
verdict="${repo_root}/.shux/scripts/lib/kitty_frame_verdict.py"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"

for required in "${rig}" "${verdict}"; do
    if [ ! -f "${required}" ]; then
        echo "✗ cannot find ${required}" >&2
        exit 1
    fi
done

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-guiterm-selftest.XXXXXX")"
work="$(cd "${work}" && pwd -P)"
trap 'rm -rf "${work}"' EXIT

failures=0
passes=0

# Run a command, require an exact exit code, and require a needle in its output.
# `status=$?` is captured on the FIRST line of the failure path, before any
# diagnostic command can overwrite it.
expect() {
    local want="$1" label="$2" needle="$3"
    shift 3
    local slug log status=0 ok=1
    slug="$(printf '%s' "${label}" | tr -c 'a-zA-Z0-9' '_')"
    log="${work}/${slug}.log"
    "$@" >"${log}" 2>&1 || status=$?
    if [ "${status}" != "${want}" ]; then
        ok=0
    fi
    if [ -n "${needle}" ] && ! grep -qF -- "${needle}" "${log}"; then
        ok=0
    fi
    if [ "${ok}" -eq 1 ]; then
        printf '  \033[32m✓\033[0m %s (exit %s)\n' "${label}" "${status}"
        passes=$((passes + 1))
    else
        printf '  \033[31m✗\033[0m %s: expected exit %s with %s, got exit %s\n' \
            "${label}" "${want}" "'${needle}'" "${status}"
        sed 's/^/      /' "${log}" | tail -20
        failures=$((failures + 1))
    fi
}

echo "▶ GUI-terminal rig self-test"

# ── 3. A missing tool must be loud ──────────────────────────────────────────
#
# The shim is EXECUTABLE and fails. A non-executable file of the right name
# shadows nothing: `command -v` skips it and keeps searching, so a preflight
# written that way runs the whole rig and the arm reports a red it did not earn.
# The rig's preflight now RUNS each tool, which is the only probe that can tell
# "absent" and "installed and broken" apart — and both are the same failure to
# anyone waiting on the result.
shim_dir="${work}/shims"
mkdir -p "${shim_dir}"

hide_tool() {
    local tool="$1"
    rm -f "${shim_dir}"/*
    printf '#!/bin/sh\nexit 127\n' >"${shim_dir}/${tool}"
    chmod 0755 "${shim_dir}/${tool}"
}

for tool in kitty import xdotool Xvfb; do
    hide_tool "${tool}"
    # The needle names the tool in the rig's own "missing:" line rather than
    # anywhere in the output: the install hint used to mention every tool, so any
    # arm matched any tool's name and all four cases passed on one failure.
    expect 3 "a broken ${tool} is loud" "missing: ${tool}" \
        env PATH="${shim_dir}:${PATH}" bash "${rig}" --scenario plain
done
rm -f "${shim_dir}"/*

expect 3 "a missing shux binary is loud" "missing: ${work}/definitely-not-a-binary" \
    env SHUX_BIN="${work}/definitely-not-a-binary" bash "${rig}" --scenario plain

# The comparator is a `uv run --script` that resolves pillow and numpy. On a
# cache-cold offline machine it cannot start, and without this probe that surfaces
# as "shux's chrome never appeared" — a tooling failure blamed on shux.
expect 3 "a comparator that cannot run is loud" "missing: a working" \
    env UV_OFFLINE=1 UV_CACHE_DIR="${work}/empty-uv-cache" \
    UV_PYTHON_INSTALL_DIR="${work}/empty-uv-python" \
    bash "${rig}" --scenario plain

expect 2 "an unknown scenario is rejected" "unknown scenario" \
    bash "${rig}" --scenario paint-it-black

# ── 1a. The control: the fix in place must PASS ─────────────────────────────
#
# Run before the empty-input cases, because its frames are real captures of a
# real emulator and those cases mutate them. Asserting on synthetic PNGs would
# only prove the comparator rejects synthetic PNGs.
contained_out="${work}/contained"
expect 0 "cell-box placement stays in the pane" "verdict: PASS" \
    env SHUX_BIN="${shux_bin}" \
    bash "${rig}" --scenario image-contained --frames 2 --out "${contained_out}"

good_frame=""
if [ -f "${contained_out}/run.json" ]; then
    good_frame="$(python3 -c '
import json, sys
run = json.load(open(sys.argv[1]))
print(run["phases"][0]["frames"][0])
' "${contained_out}/run.json")"
fi

# ── 2. Empty input is a failure, not an empty pass ──────────────────────────
if [ -z "${good_frame}" ] || [ ! -f "${good_frame}" ]; then
    printf '  \033[31m✗\033[0m no captured frame to mutate — the empty-input cases cannot run\n'
    failures=$((failures + 1))
else
    # A geometry file describing one phase of the real run, with `frames` swapped
    # for whatever the case is about. `--phase` picks which phase to keep, so the
    # injected phase's `require_image` can be exercised too.
    mutate() {
        local out="$1" phase="$2"
        shift 2
        python3 -c '
import json, sys
run = json.load(open(sys.argv[1]))
keep = [p for p in run["phases"] if p["name"] == sys.argv[3]][0]
run["phases"] = [dict(keep, frames=list(sys.argv[4:]))]
json.dump(run, open(sys.argv[2], "w"))
' "${contained_out}/run.json" "${out}" "${phase}" "$@"
    }

    # The positive control for this half: the unmutated frame still passes, so a
    # red below is the mutation talking and not a comparator that rejects
    # everything.
    mutate "${work}/good.json" initial "${good_frame}"
    expect 0 "an untouched frame still passes" "verdict: PASS" \
        "${verdict}" --geometry "${work}/good.json"

    mutate "${work}/none.json" initial
    expect 1 "zero frames is a failure" "noframes" \
        "${verdict}" --geometry "${work}/none.json"

    python3 -c '
import json, sys
run = json.load(open(sys.argv[1]))
run["phases"] = []
json.dump(run, open(sys.argv[2], "w"))
' "${contained_out}/run.json" "${work}/nophases.json"
    expect 1 "no phases at all is a failure" "noframes" \
        "${verdict}" --geometry "${work}/nophases.json"

    mutate "${work}/gone.json" initial "${work}/never-captured.png"
    expect 1 "a frame that was never captured is a failure" "does not exist" \
        "${verdict}" --geometry "${work}/gone.json"

    # A valid PNG of exactly the right size, and black. This is the shape of
    # false pass the spike shipped.
    python3 -c '
import struct, sys, zlib
w, h = 1400, 900
raw = b"".join(b"\x00" + b"\x00" * (w * 3) for _ in range(h))
def chunk(tag, data):
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
open(sys.argv[1], "wb").write(
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw))
    + chunk(b"IEND", b""))
' "${work}/black.png"
    mutate "${work}/black.json" initial "${work}/black.png"
    expect 1 "a black screen of the right size is a failure" "chrome" \
        "${verdict}" --geometry "${work}/black.json"

    # A capture interrupted half-written.
    head -c 2000 "${good_frame}" >"${work}/truncated.png"
    mutate "${work}/truncated.json" initial "${work}/truncated.png"
    expect 1 "a truncated capture is a failure" "unreadable" \
        "${verdict}" --geometry "${work}/truncated.json"

    # A frame that lost a colour class. The mutation is on the PICTURE, not on
    # the geometry file: repointing `probe_rgb` at a colour nothing paints would
    # manufacture a red on any tree, which proves the assertion is wired and
    # nothing else. Repainting the pixels the indexed probe drew is what a shux
    # that had stopped emitting indexed colour would actually produce.
    cat >"${work}/strip_colour.py" <<'STRIPPER'
# /// script
# requires-python = ">=3.14"
# dependencies = ["pillow", "numpy"]
# ///
import json, sys
from pathlib import Path
import numpy as np
from PIL import Image

run = json.loads(Path(sys.argv[1]).read_text())
phase = run["phases"][0]
arr = np.asarray(Image.open(phase["frames"][0]).convert("RGB")).astype(int)
target = np.array(run["probe_rgb"][1])
arr[np.abs(arr - target).max(axis=2) <= 60] = [200, 200, 200]
Image.fromarray(arr.astype(np.uint8)).save(sys.argv[2])
run["phases"] = [dict(phase, frames=[sys.argv[2]])]
Path(sys.argv[3]).write_text(json.dumps(run))
STRIPPER
    uv run --script "${work}/strip_colour.py" "${contained_out}/run.json" \
        "${work}/no_indexed.png" "${work}/no_indexed.json"
    expect 1 "a frame that lost indexed colour is a failure" "probe" \
        "${verdict}" --geometry "${work}/no_indexed.json"

    # An injected phase judged against a frame from BEFORE the injection: the
    # image the phase promises is simply not there. Without this case nothing
    # exercises `require_image`, and an injection that silently never happened
    # would read as a clean containment pass.
    mutate "${work}/noimage.json" inject "${good_frame}"
    expect 1 "an injected phase with no image in it is a failure" "content:image" \
        "${verdict}" --geometry "${work}/noimage.json"
fi

# ── 1b. The reintroduced defect ─────────────────────────────────────────────
#
# The overflow #175 is about. Same payload, same position, no destination box in
# cells — and the rig must go red naming CONTAINMENT:IMAGE.
expect 1 "an unclamped image overflows the pane and the rig sees it" \
    "containment:image" \
    env SHUX_BIN="${shux_bin}" \
    bash "${rig}" --scenario image-overflow --frames 2 --out "${work}/overflow"

# ── The default scenario, all four window sizes ──────────────────────────
#
# The only case that drives `xdotool windowsize` and the wait for shux to resize
# the pane. One frame per phase: this arm is about the phases existing and
# passing, not about frame cadence.
expect 0 "the plain scenario passes at every window size" "4 phases" \
    env SHUX_BIN="${shux_bin}" \
    bash "${rig}" --scenario plain --frames 1 --out "${work}/plain"

if [ "${failures}" -gt 0 ]; then
    echo "✗ GUI-terminal rig self-test: ${failures} case(s) failed" >&2
    exit 1
fi
echo "✓ GUI-terminal rig self-test: ${passes} cases — the rig fails when it should"
