#!/usr/bin/env bash
# Visual evidence for issue #106, captured from real panes through shux's own
# rasterizer. Runs against whichever binary `SHUX_BIN` points at, so the same
# scenes can be shot before and after the fix and compared.
#
#   SHUX_BIN=<path> LABEL=before .shux/scripts/issue_106_evidence.sh
#
# Scenes:
#   ris-copy-mode   a pane that was reset while a full-screen app owned the
#                   screen, viewed through copy mode. Before the fix the pane's
#                   scrollback is gone; after it is intact.
#   dos-victim      a pane in one session while ANOTHER session's pane does
#                   nothing but toggle the alternate screen.
#   richtui-vim     vim, entered and exited five times in the same pane, to
#                   show the recycled buffer still renders a real full-screen
#                   application correctly.
#
# Output: .shux/out/issue-106/<label>/*.png (+ .txt). Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
label="${LABEL:-after}"
out_dir="${repo_root}/.shux/out/issue-106/${label}"
runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-106-${label}.XXXXXX")"

# Deliberately narrow panes. The rasterizer draws a fixed cell size, so fewer
# columns means the text fills more of the frame — the point of the shot is to
# be readable, not to survey a desktop.
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

# A colour probe on every scene, so a monochrome regression cannot pass as a
# clean shot: truecolor, 256-indexed, and basic ANSI all present.
probe='printf "\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m"'

shoot() { # shoot <session> <pane> <name>
  sx pane snapshot -s "$1" -p "$2" -o "${out_dir}/$3.png" >/dev/null
  sx pane capture -s "$1" -p "$2" >"${out_dir}/$3.txt"
  local size
  size=$(wc -c <"${out_dir}/$3.png")
  # A valid PNG of the right dimensions can still be blank; the .txt beside it
  # is what gets asserted on.
  printf '    %-28s %8s bytes png, %3s lines text\n' "$3" "${size}" "$(wc -l <"${out_dir}/$3.txt")"
}

# ---------------------------------------------------------------------------
# Scene 1: RIS on the alternate screen, seen through copy mode
# ---------------------------------------------------------------------------
# The inner pane plays out what happens when a full-screen application owns the
# screen and the pane is then reset — `reset(1)`, or a crashed TUI. It then
# prints a numbered history far longer than the viewport. Whether that history
# survives is the whole question, and copy mode is where a human would look.

inner="ev106-inner-${RANDOM}-$$"
outer="ev106-outer-${RANDOM}-$$"

history_script="${runtime}/history.sh"
cat >"${history_script}" <<'INNER'
#!/usr/bin/env bash
# A full-screen application takes the screen...
printf '\033[?1049h'
printf '\033[2J\033[3;6HFULL-SCREEN APP RUNNING\033[6;6H(vim, less, btop -- anything that uses the alternate screen)'
sleep 1
# ...and the pane is reset while it still owns it.
printf '\033c'
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'
printf 'pane was reset while the alternate screen was live\n'
for i in $(seq -w 1 120); do
  printf '\033[38;5;245mhistory-line-%s\033[0m  this line scrolled off the top\n' "$i"
done
sleep 9000
INNER
chmod +x "${history_script}"

sx session create "${inner}" -d --title "reset-while-fullscreen" -- \
  env TERM=xterm-256color COLORTERM=truecolor bash "${history_script}" >/dev/null
sessions+=("${inner}")
inner_pane="$(sx --format json pane list -s "${inner}" | jq -r '.[0].id')"
sx pane set-size -s "${inner}" -p "${inner_pane}" --cols "${cols}" --rows "${rows}" >/dev/null
sx pane wait-for -s "${inner}" -p "${inner_pane}" -t "history-line-120" --timeout-ms 15000 >/dev/null

# Attach to it from a pane of ANOTHER session, so the attach client's own copy
# mode can be rasterized. Nested shux: the outer pane is just a terminal.
sx session create "${outer}" -d --title "copy mode" -- \
  env TERM=xterm-256color COLORTERM=truecolor XDG_RUNTIME_DIR="${runtime}" \
  "${shux_bin}" session attach "${inner}" >/dev/null
sessions+=("${outer}")
outer_pane="$(sx --format json pane list -s "${outer}" | jq -r '.[0].id')"
sx pane set-size -s "${outer}" -p "${outer_pane}" --cols "${cols}" --rows "${rows}" >/dev/null
sleep 1.5

# The default prefix is ctrl-space (0x00); prefix + "[" enters copy mode and
# "gg" jumps to the oldest retained line. If the pane has no scrollback there
# is nothing above the viewport to jump to, which is exactly the symptom.
sx pane send-keys -s "${outer}" -p "${outer_pane}" --data "AA==" >/dev/null
sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "[" >/dev/null
sleep 1.0
sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "gg" >/dev/null
sleep 0.6
# Step the copy-mode cursor off the top-left cell so it does not sit on top of
# the first character of the oldest retained line, which is the evidence.
sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "j" >/dev/null
sleep 0.8
shoot "${outer}" "${outer_pane}" "ris-copy-mode-top"

sx pane send-keys -s "${outer}" -p "${outer_pane}" --text "q" >/dev/null
sleep 0.5
# Tear the attach client down BEFORE anything else: a live attach client that
# loses its daemon will auto-start a replacement, which reads as a leak.
sx session kill "${outer}" >/dev/null 2>&1 || true
sessions=("${inner}")
sleep 1.0

# ---------------------------------------------------------------------------
# Scene 2: one pane's toggling, another session's pane
# ---------------------------------------------------------------------------
victim="ev106-victim-${RANDOM}-$$"
attacker="ev106-attacker-${RANDOM}-$$"

attack_script="${runtime}/attack.sh"
cat >"${attack_script}" <<'ATTACK'
#!/usr/bin/env bash
burst="$(printf '\033[?1049h\033[?1049l%.0s' $(seq 1 2048))"
while :; do printf '%s' "${burst}"; done
ATTACK
chmod +x "${attack_script}"

# The victim runs a clock, so a frozen pane is visible as a stale one.
clock_script="${runtime}/clock.sh"
cat >"${clock_script}" <<CLOCK
#!/usr/bin/env bash
printf '\033[2J\033[H'
${probe}
printf '\n\n'
i=0
while :; do
  i=\$((i + 1))
  printf '\r\033[1;38;2;255;170;60mVICTIM PANE\033[0m  tick \033[1m%06d\033[0m  (a different session entirely)' "\$i"
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

# Spawned IDLE so its geometry can be set before the attack starts: the cost of
# the defect scales with rows x cols, and a small pane does not show it.
sx session create "${attacker}" -d --title "attacker" -- \
  env TERM=xterm-256color COLORTERM=truecolor bash --noprofile --norc >/dev/null
sessions+=("${attacker}")
attacker_pane="$(sx --format json pane list -s "${attacker}" | jq -r '.[0].id')"
sx pane set-size -s "${attacker}" -p "${attacker_pane}" \
  --cols "${ATTACK_COLS:-240}" --rows "${ATTACK_ROWS:-64}" >/dev/null
sx pane send-keys -s "${attacker}" -p "${attacker_pane}" \
  --text "exec ${attack_script}"$'\n' >/dev/null
sleep 3
# An ordinary, unrelated RPC, issued while the attack runs. Whether it returns
# at all is the headline number, so it is recorded rather than asserted: on the
# unfixed build the daemon does not answer inside its own ack timeout.
set +e
resize_start=$(python3 -c 'import time; print(time.monotonic())')
sx pane set-size -s "${victim}" -p "${victim_pane}" --cols "${cols}" --rows 9 >/dev/null 2>&1
resize_rc=$?
set -e
python3 - "${resize_start}" "${resize_rc}" >"${out_dir}/dos-rpc-outcome.json" <<'PY2'
import json, sys, time
start, rc = float(sys.argv[1]), int(sys.argv[2])
print(json.dumps({
    "rpc": "pane.set_size",
    "issued_while": "another session's pane toggles the alternate screen",
    "exit_code": rc,
    "outcome": "ok" if rc == 0 else "FAILED (daemon did not answer)",
    "elapsed_ms": round((time.monotonic() - start) * 1000, 1),
}, indent=2))
PY2
sed 's/^/    /' "${out_dir}/dos-rpc-outcome.json"
sleep 4

# Latency of an ordinary RPC against the victim pane, while the attacker runs.
python3 - "${shux_bin}" "${runtime}" "${victim}" "${victim_pane}" \
  >"${out_dir}/dos-latency.json" <<'PY'
import json, os, subprocess, sys, time
shux, runtime, session, pane = sys.argv[1:5]
env = dict(os.environ, XDG_RUNTIME_DIR=runtime)
env.pop("SHUX_SOCKET", None)
lat = []
for _ in range(15):
    t0 = time.monotonic()
    subprocess.run([shux, "pane", "capture", "-s", session, "-p", pane],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   env=env, check=False)
    lat.append(round((time.monotonic() - t0) * 1000.0, 2))
lat.sort()
print(json.dumps({"median_ms": lat[len(lat) // 2], "max_ms": lat[-1], "samples_ms": lat}))
PY
shoot "${victim}" "${victim_pane}" "dos-victim-under-attack"
sx session kill "${attacker}" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# Scene 3: a real full-screen application, entered and exited repeatedly
# ---------------------------------------------------------------------------
if command -v vim >/dev/null 2>&1; then
  tui="ev106-tui-${RANDOM}-$$"
  sx session create "${tui}" -d --title "vim x5" -- \
    env TERM=xterm-256color COLORTERM=truecolor bash --noprofile --norc >/dev/null
  sessions+=("${tui}")
  tui_pane="$(sx --format json pane list -s "${tui}" | jq -r '.[0].id')"
  sx pane set-size -s "${tui}" -p "${tui_pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  sleep 0.8

  vim_file="${runtime}/recycled.txt"
  cat >"${vim_file}" <<'VIMDOC'
alternate screen entry #N
this buffer is drawn into a RECYCLED alternate-screen grid
if a previous vim session's pixels survived, they would be visible here
VIMDOC

  # Five full enter/exit cycles through the alternate screen in ONE pane. Each
  # one retires a buffer the next one picks back up.
  for round in 1 2 3 4 5; do
    sx pane send-keys -s "${tui}" -p "${tui_pane}" \
      --text "vim -u NONE -c 'syntax off' ${vim_file}"$'\n' >/dev/null
    sleep 1.2
    if [ "${round}" -lt 5 ]; then
      sx pane send-keys -s "${tui}" -p "${tui_pane}" --text ":q!"$'\n' >/dev/null
      sleep 0.8
    fi
  done
  sx pane send-keys -s "${tui}" -p "${tui_pane}" --text "GoENTRY 5 typed into the recycled buffer" >/dev/null
  sleep 1.0
  shoot "${tui}" "${tui_pane}" "richtui-vim-5th-entry"
  sx pane send-keys -s "${tui}" -p "${tui_pane}" --data "Gw==" >/dev/null
  sx pane send-keys -s "${tui}" -p "${tui_pane}" --text ":q!"$'\n' >/dev/null
  sleep 0.8
  shoot "${tui}" "${tui_pane}" "richtui-vim-after-exit"
fi

echo "==> ${label} evidence in ${out_dir}"
