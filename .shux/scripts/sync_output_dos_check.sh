#!/usr/bin/env bash
# Issue #115 end-to-end reproduction: can panes emitting nothing but DEC 2026
# synchronized-output windows stall the whole daemon for every OTHER pane?
#
# The freeze runs inside the daemon-wide `PaneIoState` mutex (crates/shux/src/
# main.rs, PTY read loop), so work bought by pane A is latency paid by pane B.
# This drives the REAL binary: one victim session whose `pane capture` latency
# is sampled while quiet and again under attack, and N attacker panes in a
# second session.
#
# Two attack shapes, because the fix has two halves:
#   toggle  ESC[?2026h ESC[?2026l          -- a window that draws nothing
#   write   ESC[?2026h a ESC[?2026l        -- a window that draws one cell,
#                                             which legitimately takes a copy
# Pick with SHUX_DOS_ATTACK=toggle|write (default: toggle).
#
# Every attacker pane first fills its OWN scrollback, because the defect scaled
# with retained history: an attack from an empty pane understates it.
#
# Emits a JSON verdict on stdout. Exit 0 always -- the caller reads the numbers.
# Wrap in .shux/scripts/no_leak_guard.sh.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
samples="${SHUX_DOS_SAMPLES:-15}"
attack_seconds="${SHUX_DOS_ATTACK_SECONDS:-6}"
cols="${SHUX_DOS_COLS:-240}"
rows="${SHUX_DOS_ROWS:-64}"
attackers="${SHUX_DOS_ATTACKERS:-6}"
attack_shape="${SHUX_DOS_ATTACK:-toggle}"
scrollback_lines="${SHUX_DOS_SCROLLBACK:-5000}"
label="${SHUX_DOS_LABEL:-unlabelled}"
out_json="${SHUX_DOS_OUT:-}"

if [ ! -x "${shux_bin}" ]; then
  echo "missing binary: ${shux_bin} (run: make release)" >&2
  exit 2
fi

case "${attack_shape}" in
  toggle) attack_payload='\033[?2026h\033[?2026l' ;;
  write)  attack_payload='\033[?2026ha\033[?2026l' ;;
  *) echo "unknown SHUX_DOS_ATTACK: ${attack_shape} (toggle|write)" >&2; exit 2 ;;
esac

runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-sync-dos.XXXXXX")"
victim="sync-dos-victim-${RANDOM}-$$"
attacker="sync-dos-attacker-${RANDOM}-$$"
created=0

cleanup() {
  if [ "${created}" = "1" ]; then
    shux_harness_kill_session "${runtime}" "${shux_bin}" "${attacker}"
    shux_harness_cleanup_runtime "${runtime}" "${shux_bin}" "${victim}"
  else
    shux_harness_stop_daemon "${runtime}"
    rm -rf "${runtime}"
  fi
}
trap cleanup EXIT

sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" "$@"; }

victim_json="$(sx --format json session create "${victim}" -d -- \
  env TERM=xterm-256color COLORTERM=truecolor bash --noprofile --norc)"
created=1
victim_pane="$(jq -r '.pane_id' <<<"${victim_json}")"
sx pane set-size -s "${victim}" -p "${victim_pane}" --cols "${cols}" --rows "${rows}" >/dev/null

# Colour-probed content on the victim, so `capture` reads a real screen and the
# same pane can be screenshotted as visual evidence. Truecolor + indexed +
# basic, so a monochrome regression cannot pass this harness either.
sx pane send-keys -s "${victim}" -p "${victim_pane}" --text \
  $'clear; printf "\\033[1;38;2;120;220;180mVICTIM PANE\\033[0m  \\033[38;5;196mINDEXED\\033[0m  \\033[34mBASIC\\033[0m\\n"\n' >/dev/null
sleep 0.8

attack_script="${runtime}/sync_toggle.sh"
cat >"${attack_script}" <<ATTACK
#!/usr/bin/env bash
# Fill this pane's OWN scrollback first: the freeze copied retained history,
# so the cost of one window scales with how much of it there is.
for i in \$(seq 1 ${scrollback_lines}); do
  printf 'scrollback line %s \033[38;5;33mCOLOUR\033[0m\n' "\$i"
done
burst="\$(printf '${attack_payload}%.0s' \$(seq 1 2048))"
while :; do printf '%s' "\${burst}"; done
ATTACK
chmod +x "${attack_script}"

# The attackers start IDLE so the quiet baseline is measured on a daemon that
# already owns every pane -- the only variable between the two samples is the
# escape sequence.
attacker_json="$(sx --format json session create "${attacker}" -d -- \
  env TERM=xterm-256color COLORTERM=truecolor bash --noprofile --norc)"
first_attacker_pane="$(jq -r '.pane_id' <<<"${attacker_json}")"
attacker_panes=("${first_attacker_pane}")
for _ in $(seq 2 "${attackers}"); do
  pane_json="$(sx --format json pane split -s "${attacker}" -p "${first_attacker_pane}")"
  attacker_panes+=("$(jq -r '.pane.id' <<<"${pane_json}")")
done
for pane in "${attacker_panes[@]}"; do
  sx pane set-size -s "${attacker}" -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
done

sample_latency() {
  local sample_label="$1"
  python3 - "${shux_bin}" "${runtime}" "${victim}" "${victim_pane}" "${samples}" "${sample_label}" <<'PY'
import json, os, subprocess, sys, time
shux, runtime, session, pane, n, label = sys.argv[1:7]
env = dict(os.environ, XDG_RUNTIME_DIR=runtime)
env.pop("SHUX_SOCKET", None)
# A daemon that never answers is a RESULT, not a reason to hang: the sample is
# recorded at the ceiling and counted, so "wedged" is reported rather than
# waited out.
CEILING_S = 8.0
lat, timeouts = [], 0
for _ in range(int(n)):
    t0 = time.monotonic()
    try:
        subprocess.run([shux, "pane", "capture", "-s", session, "-p", pane],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       env=env, check=False, timeout=CEILING_S)
    except subprocess.TimeoutExpired:
        timeouts += 1
    lat.append((time.monotonic() - t0) * 1000.0)
print(json.dumps({"label": label, "samples_ms": lat, "timed_out": timeouts,
                  "ceiling_ms": CEILING_S * 1000}))
PY
}

quiet="$(sample_latency quiet)"

for pane in "${attacker_panes[@]}"; do
  shux_harness_timeout 20s env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${shux_bin}" \
    pane send-keys -s "${attacker}" -p "${pane}" --text "exec ${attack_script}"$'\n' >/dev/null || true
done
# Give the attackers time to lay down their scrollback and reach steady state.
sleep "$(( attack_seconds > 8 ? attack_seconds : 8 ))"

under_attack="$(sample_latency under_attack)"

verdict="$(python3 - "${quiet}" "${under_attack}" "${cols}" "${rows}" \
                    "${attackers}" "${attack_shape}" "${label}" "${scrollback_lines}" <<'PY'
import json, statistics, sys
quiet_raw = json.loads(sys.argv[1])
attack_raw = json.loads(sys.argv[2])
quiet, attack = quiet_raw["samples_ms"], attack_raw["samples_ms"]
cols, rows, attackers = int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
shape, label, scrollback = sys.argv[6], sys.argv[7], int(sys.argv[8])

def stats(v):
    s = sorted(v)
    return {
        "n": len(s),
        "median_ms": round(statistics.median(s), 2),
        "p95_ms": round(s[min(len(s) - 1, int(0.95 * len(s)))], 2),
        "max_ms": round(max(s), 2),
    }

q, a = stats(quiet), stats(attack)
print(json.dumps({
    "label": label,
    "attack": shape,
    "attacker_panes": attackers,
    "scrollback_lines": scrollback,
    "geometry": f"{cols}x{rows}",
    "victim_capture_latency": {"quiet": q, "under_attack": a},
    "captures_that_never_returned": {
        "quiet": quiet_raw.get("timed_out", 0),
        "under_attack": attack_raw.get("timed_out", 0),
        "ceiling_ms": attack_raw.get("ceiling_ms"),
    },
    "median_slowdown_x": round(a["median_ms"] / max(q["median_ms"], 1e-9), 1),
    "max_slowdown_x": round(a["max_ms"] / max(q["max_ms"], 1e-9), 1),
}, indent=2))
PY
)"

printf '%s\n' "${verdict}"
if [ -n "${out_json}" ]; then
  mkdir -p "$(dirname "${out_json}")"
  printf '%s\n' "${verdict}" >"${out_json}"
fi
