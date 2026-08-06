#!/usr/bin/env bash
# Visual evidence for issue #115, captured from real panes through shux's own
# rasterizer. Runs against whichever binary `SHUX_BIN` points at, so the same
# scenes can be shot before and after the fix and compared.
#
#   SHUX_BIN=<path> LABEL=before .shux/scripts/issue_115_evidence.sh
#
# Scenes:
#   dos-victim      a pane in one session while panes in ANOTHER session do
#                   nothing but open and close synchronized-output windows.
#   sync-atomicity  a pane redrawing inside `ESC[?2026h` ... `ESC[?2026l`,
#                   photographed repeatedly WHILE it redraws. Every frame that
#                   comes out must be whole. A regression guard, not a
#                   before/after: this is what the fix had to preserve.
#   sync-scrollback the same pane's history, read through copy mode. Rows are
#                   now shared copy-on-write between the live grid and any
#                   frozen frame, so history is where an aliasing mistake would
#                   surface.
#   stuck-window    a pane that opens a window and never closes it, as a
#                   crashed application does.
#   richtui-*       vim, btop and htop, each redrawing inside real
#                   synchronized-output windows.
#
# Output: .shux/out/issue-115/<label>/*.png (+ .txt). Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
label="${LABEL:-after}"
# Which scenes to run, space separated. The dos scene wedges an UNFIXED daemon
# so hard that the scenes after it cannot be captured at all — which is itself
# the finding, measured by .shux/scripts/sync_output_dos_check.sh — so the
# before/after screenshot pass runs the other scenes on their own.
scenes="${SCENES:-dos sync stuck richtui}"
has_scene() { case " ${scenes} " in *" $1 "*) return 0;; *) return 1;; esac; }
out_dir="${repo_root}/.shux/out/issue-115/${label}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-115-${label}.XXXXXX")"

# Deliberately narrow panes. The rasterizer draws a fixed cell size, so fewer
# columns means the text fills more of the frame — the point of the shot is to
# be readable when zoomed, not to survey a desktop.
cols="${EVID_COLS:-84}"
rows="${EVID_ROWS:-22}"

sessions=()
cleanup() {
  for s in "${sessions[@]:-}"; do
    [ -n "${s}" ] && shux_harness_kill_session "${runtime}" "${shux_bin}" "${s}"
  done
  shux_harness_stop_daemon "${runtime}"
  shux_harness_assert_no_daemon "${runtime}" || shux_harness_stop_daemon "${runtime}"
  sleep 0.5
  rm -rf "${runtime}"
}
trap cleanup EXIT

sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" "$@"; }

mkdir -p "${out_dir}"
echo "==> ${label}: $(${shux_bin} version 2>/dev/null | head -1)"

shoot() { # shoot <session> <pane> <name>
  # Bounded: a wedged daemon must leave a missing artifact and a visible
  # complaint, not a harness that never returns.
  shux_harness_timeout 25s env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" \
    pane snapshot -s "$1" -p "$2" -o "${out_dir}/$3.png" >/dev/null \
    || echo "    !! ${3}: pane snapshot did not return within 25s"
  shux_harness_timeout 25s env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" \
    pane capture -s "$1" -p "$2" >"${out_dir}/$3.txt" \
    || echo "    !! ${3}: pane capture did not return within 25s"
  [ -f "${out_dir}/$3.png" ] || return 0
  local size
  size=$(wc -c <"${out_dir}/$3.png")
  # A valid PNG of the right dimensions can still be blank; the .txt beside it
  # is what gets asserted on.
  printf '    %-30s %8s bytes png, %3s lines text\n' "$3" "${size}" "$(wc -l <"${out_dir}/$3.txt")"
}

# ---------------------------------------------------------------------------
# Scene 1: one session's panes toggling, another session's pane
# ---------------------------------------------------------------------------
victim="ev115-victim-${RANDOM}-$$"
attacker="ev115-attacker-${RANDOM}-$$"
attackers="${EVID_ATTACKERS:-6}"
if has_scene dos; then

attack_script="${runtime}/attack.sh"
cat >"${attack_script}" <<'ATTACK'
#!/usr/bin/env bash
# Fill this pane's own scrollback first: the freeze copied retained history, so
# the cost of one window scaled with how much of it there is.
for i in $(seq 1 5000); do
  printf 'scrollback line %s \033[38;5;33mCOLOUR\033[0m\n' "$i"
done
burst="$(printf '\033[?2026h\033[?2026l%.0s' $(seq 1 2048))"
while :; do printf '%s' "${burst}"; done
ATTACK
chmod +x "${attack_script}"

# The victim runs a clock, so a stalled pane is visible as a stale one.
clock_script="${runtime}/clock.sh"
cat >"${clock_script}" <<'CLOCK'
#!/usr/bin/env bash
printf '\033[2J\033[H'
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n\n'
i=0
while :; do
  i=$((i + 1))
  printf '\r\033[1;38;2;255;170;60mVICTIM PANE\033[0m  tick \033[1m%06d\033[0m  (a different session entirely)' "$i"
  sleep 0.02
done
CLOCK
chmod +x "${clock_script}"

sx session create "${victim}" -d --title "victim" -- \
  env TERM=xterm-256color COLORTERM=truecolor bash "${clock_script}" >/dev/null
sessions+=("${victim}")
victim_pane="$(sx --format json pane list -s "${victim}" | jq -r '.[0].id')"
sx pane set-size -s "${victim}" -p "${victim_pane}" --cols "${cols}" --rows 8 >/dev/null
sleep 1
shoot "${victim}" "${victim_pane}" "dos-victim-quiet"

# Spawned IDLE so geometry is set before the attack starts: the cost of the
# defect scales with rows x cols, and a small pane does not show it.
sx session create "${attacker}" -d --title "attacker" -- \
  env TERM=xterm-256color COLORTERM=truecolor bash --noprofile --norc >/dev/null
sessions+=("${attacker}")
first_attacker="$(sx --format json pane list -s "${attacker}" | jq -r '.[0].id')"
attacker_panes=("${first_attacker}")
for _ in $(seq 2 "${attackers}"); do
  attacker_panes+=("$(sx --format json pane split -s "${attacker}" -p "${first_attacker}" | jq -r '.pane.id')")
done
for pane in "${attacker_panes[@]}"; do
  sx pane set-size -s "${attacker}" -p "${pane}" \
    --cols "${ATTACK_COLS:-240}" --rows "${ATTACK_ROWS:-64}" >/dev/null
  shux_harness_timeout 20s env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" \
    pane send-keys -s "${attacker}" -p "${pane}" --text "exec ${attack_script}"$'\n' >/dev/null || true
done
sleep 10

# Latency of an ordinary RPC against the victim pane, while the attack runs.
python3 - "${shux_bin}" "${runtime}" "${victim}" "${victim_pane}" \
  >"${out_dir}/dos-latency.json" <<'PY'
import json, os, subprocess, sys, time
shux, runtime, session, pane = sys.argv[1:5]
env = dict(os.environ, XDG_RUNTIME_DIR=runtime)
env.pop("SHUX_SOCKET", None)
# A daemon that never answers is a RESULT, not a reason to hang: the sample is
# recorded at the ceiling and counted, so "wedged" is reported rather than
# waited out. Silently succeeding after a long wait would be the masked
# failure; this is the opposite.
CEILING_S = 8.0
lat, timeouts = [], 0
for _ in range(15):
    t0 = time.monotonic()
    try:
        subprocess.run([shux, "pane", "capture", "-s", session, "-p", pane],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       env=env, check=False, timeout=CEILING_S)
    except subprocess.TimeoutExpired:
        timeouts += 1
    lat.append(round((time.monotonic() - t0) * 1000.0, 2))
lat.sort()
print(json.dumps({
    "median_ms": lat[len(lat) // 2],
    "max_ms": lat[-1],
    "timed_out": timeouts,
    "ceiling_ms": CEILING_S * 1000,
    "note": ("%d of %d captures never returned within %.0f s" % (timeouts, len(lat), CEILING_S))
            if timeouts else "every capture returned",
    "samples_ms": lat,
}, indent=2))
PY
sed 's/^/    /' "${out_dir}/dos-latency.json"
shoot "${victim}" "${victim_pane}" "dos-victim-under-attack"
sx session kill "${attacker}" >/dev/null 2>&1 || true
sessions=("${victim}")
sleep 1
fi

# ---------------------------------------------------------------------------
# Scene 2: a synchronized redraw, photographed while it redraws
# ---------------------------------------------------------------------------
# This is the scene that proves the fix did not break what mode 2026 is FOR.
# The pane redraws a boxed frame inside one synchronized-output window, with a
# pause part-way through the drawing, for ever. Snapshots are taken faster than
# the frames change. Every one of them must show a COMPLETE frame — the old one
# or the new one, never a box with its bottom missing.
#
# Real applications hold these windows for single-digit milliseconds (measured:
# btop, 0-6.3 ms), so the pause here is 60 ms — long enough to photograph
# inside, short enough to be an ordinary redraw.

if has_scene sync; then
app="ev115-app-${RANDOM}-$$"
app_script="${runtime}/syncapp.sh"
cat >"${app_script}" <<'APP'
#!/usr/bin/env bash
printf '\033[2J\033[H'
for i in $(seq -w 1 120); do
  printf '\033[38;5;245mhistory-line-%s\033[0m  this line scrolled off the top\n' "$i"
done
printf '\033[2J\033[H'
printf 'ready\n'
sleep 1
n=0
while :; do
  n=$((n + 1))
  if [ $((n % 2)) -eq 0 ]; then colour='255;170;60'; else colour='80;170;255'; fi
  printf '\033[?2026h'
  printf '\033[2J\033[H'
  printf '\033[1;38;2;%smFRAME %04d\033[0m\n' "${colour}" "${n}"
  printf '\033[38;2;120;220;180m+--------------------------------------+\033[0m\n'
  printf '\033[38;2;120;220;180m|\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m \033[38;2;%sm TRUECOLOR\033[0m       \033[38;2;120;220;180m|\033[0m\n' "${colour}"
  # Half-drawn: the box has no bottom yet. A terminal that ignored the mode
  # would show exactly this.
  sleep 0.06
  printf '\033[38;2;120;220;180m|\033[0m this frame was drawn as ONE unit     \033[38;2;120;220;180m|\033[0m\n'
  printf '\033[38;2;120;220;180m+--------------------------------------+\033[0m\n'
  printf 'FRAME-COMPLETE\n'
  printf '\033[?2026l'
  sleep 0.14
done
APP
chmod +x "${app_script}"

sx session create "${app}" -d --title "sync redraw" -- \
  env TERM=xterm-256color COLORTERM=truecolor bash "${app_script}" >/dev/null
sessions+=("${app}")
app_pane="$(sx --format json pane list -s "${app}" | jq -r '.[0].id')"
sx pane set-size -s "${app}" -p "${app_pane}" --cols "${cols}" --rows 9 >/dev/null
sx pane wait-for -s "${app}" -p "${app_pane}" -t "FRAME-COMPLETE" --timeout-ms 30000 >/dev/null

# Photograph the pane repeatedly, straight through the redraws.
torn=0
frames=0
for shot in $(seq 1 20); do
  sx pane capture -s "${app}" -p "${app_pane}" >"${runtime}/frame-${shot}.txt"
  frames=$((frames + 1))
  if ! grep -q 'FRAME-COMPLETE' "${runtime}/frame-${shot}.txt"; then
    torn=$((torn + 1))
    cp "${runtime}/frame-${shot}.txt" "${out_dir}/sync-torn-frame-${shot}.txt"
  fi
  sleep 0.037
done
python3 - "${frames}" "${torn}" >"${out_dir}/sync-atomicity.json" <<'PY3'
import json, sys
frames, torn = int(sys.argv[1]), int(sys.argv[2])
print(json.dumps({
    "captures": frames,
    "torn_frames": torn,
    "verdict": "every capture was a complete frame" if torn == 0
               else "TORN: a half-drawn frame escaped the synchronized window",
}, indent=2))
PY3
sed 's/^/    /' "${out_dir}/sync-atomicity.json"
shoot "${app}" "${app_pane}" "sync-redraw-frame"

# ---------------------------------------------------------------------------
# Scene 3: that pane's history, through copy mode
# ---------------------------------------------------------------------------
# `pane capture` is viewport-only, so scrollback is read the way a human reads
# it: attach to the session from a pane of ANOTHER session and drive copy mode.
# Rows are now shared copy-on-write between the live grid and any frozen frame,
# so history is where an aliasing mistake would show up as corrupted or missing
# lines.
outer="ev115-outer-${RANDOM}-$$"
sx session create "${outer}" -d --title "copy mode" -- \
  env TERM=xterm-256color COLORTERM=truecolor XDG_RUNTIME_DIR="${runtime}" \
  "${shux_bin}" session attach "${app}" >/dev/null
sessions+=("${outer}")
outer_pane="$(sx --format json pane list -s "${outer}" | jq -r '.[0].id')"
sx pane set-size -s "${outer}" -p "${outer_pane}" --cols "${cols}" --rows "${rows}" >/dev/null
sleep 1.5
# Default prefix is ctrl-space (0x00); prefix + "[" enters copy mode, "gg"
# jumps to the oldest retained line, "j" steps the cursor off it so it does not
# sit on top of the evidence.
sx pane send-keys -s "${outer}" -p "${outer_pane}" --data "AA==" >/dev/null
sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "[" >/dev/null
sleep 1.0
sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "gg" >/dev/null
sleep 0.6
sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "j" >/dev/null
sleep 0.8
shoot "${outer}" "${outer_pane}" "sync-scrollback-copy-mode"
sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "q" >/dev/null
sleep 0.5
# Tear the attach client down BEFORE anything else: a live attach client that
# loses its daemon auto-starts a replacement, which reads as a leak.
sx session kill "${outer}" >/dev/null 2>&1 || true
sleep 1.0
sx session kill "${app}" >/dev/null 2>&1 || true
sessions=()
fi

# ---------------------------------------------------------------------------
# Scene 4: a window nobody ever closes
# ---------------------------------------------------------------------------
if has_scene stuck; then
stuck="ev115-stuck-${RANDOM}-$$"
stuck_script="${runtime}/stuck.sh"
cat >"${stuck_script}" <<'STUCK'
#!/usr/bin/env bash
printf '\033[2J\033[H'
printf '\033[1;38;2;255;170;60mBEFORE THE WINDOW OPENS\033[0m\n'
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'
printf 'ready\n'
sleep 2
# An application killed mid-redraw: it opens the window, draws, and never
# sends the close.
printf '\033[?2026h'
printf '\033[2J\033[H'
printf '\033[1;38;2;80;170;255mAFTER  -- written inside a window that is never closed\033[0m\n'
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'
printf 'this text is only reachable if the window has a deadline\n'
sleep 9000
STUCK
chmod +x "${stuck_script}"

sx session create "${stuck}" -d --title "stuck window" -- \
  env TERM=xterm-256color COLORTERM=truecolor bash "${stuck_script}" >/dev/null
sessions+=("${stuck}")
stuck_pane="$(sx --format json pane list -s "${stuck}" | jq -r '.[0].id')"
sx pane set-size -s "${stuck}" -p "${stuck_pane}" --cols "${cols}" --rows 10 >/dev/null
sx pane wait-for -s "${stuck}" -p "${stuck_pane}" -t "ready" --timeout-ms 20000 >/dev/null
sleep 4
shoot "${stuck}" "${stuck_pane}" "stuck-window-4s-later"
sx session kill "${stuck}" >/dev/null 2>&1 || true
sessions=()
fi

# ---------------------------------------------------------------------------
# Scene 5: real full-screen applications
# ---------------------------------------------------------------------------
richtui() { # richtui <name> <command> <settle seconds> [rows]
  local name="$1" cmd="$2" settle="$3" tui_rows="${4:-${rows}}"
  command -v "${cmd%% *}" >/dev/null 2>&1 || { echo "    skip ${name}: not installed"; return; }
  local sess="ev115-${name}-${RANDOM}-$$"
  sx session create "${sess}" -d --title "${name}" -- \
    env TERM=xterm-256color COLORTERM=truecolor bash --noprofile --norc >/dev/null
  sessions+=("${sess}")
  local pane
  pane="$(sx --format json pane list -s "${sess}" | jq -r '.[0].id')"
  sx pane set-size -s "${sess}" -p "${pane}" --cols "${cols}" --rows "${tui_rows}" >/dev/null
  sleep 0.8
  sx pane send-keys -s "${sess}" -p "${pane}" --text "${cmd}"$'\n' >/dev/null
  sleep "${settle}"
  shoot "${sess}" "${pane}" "richtui-${name}"
  sx session kill "${sess}" >/dev/null 2>&1 || true
  sessions=()
}
if ! has_scene richtui; then richtui() { :; }; fi

vim_file="${runtime}/sync.txt"
cat >"${vim_file}" <<'VIMDOC'
vim redraws through DEC 2026 synchronized output.
Every character on this screen arrived inside one of those windows.
If the freeze were taken from the wrong frame, this text would tear.
VIMDOC
richtui "vim" "vim -u NONE -c 'syntax on' ${vim_file}" 2.5
# btop is the one installed application that actually drives DEC 2026: it opens
# and closes a synchronized-output window per redraw, ~19 of them in 12 s,
# holding each for 0-6.3 ms. It also needs at least 24 rows to draw at all.
richtui "btop" "btop --utf-force" 6 30
richtui "htop" "htop" 4 30

echo "==> ${label} evidence in ${out_dir}"
