#!/usr/bin/env bash
# Issue #106 end-to-end reproduction: can ONE pane's alternate-screen toggling
# stall the whole daemon for every OTHER pane?
#
# The alt-screen swap runs inside the daemon-wide `PaneIoState` mutex
# (crates/shux/src/main.rs, PTY read loop), so allocator work bought by pane A
# is latency paid by pane B. This drives the REAL binary: two sessions, one
# hostile pane, one victim pane whose `pane capture` latency is sampled while
# quiet and again under attack.
#
# Emits a JSON verdict on stdout. Exit 0 always — the caller reads the numbers.
# Wrap in .shux/scripts/no_leak_guard.sh.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
samples="${SHUX_DOS_SAMPLES:-15}"
attack_seconds="${SHUX_DOS_ATTACK_SECONDS:-6}"
cols="${SHUX_DOS_COLS:-240}"
rows="${SHUX_DOS_ROWS:-64}"
out_json="${SHUX_DOS_OUT:-}"

if [ ! -x "${shux_bin}" ]; then
  echo "missing binary: ${shux_bin} (run: make release)" >&2
  exit 2
fi

runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-alt-dos.XXXXXX")"
victim="alt-dos-victim-${RANDOM}-$$"
attacker="alt-dos-attacker-${RANDOM}-$$"
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

attack_script="${runtime}/alt_toggle.sh"
cat >"${attack_script}" <<'ATTACK'
#!/usr/bin/env bash
# Nothing but the alternate screen, as fast as a pane can ask for it.
burst="$(printf '\033[?1049h\033[?1049l%.0s' $(seq 1 2048))"
while :; do printf '%s' "${burst}"; done
ATTACK
chmod +x "${attack_script}"

# The attacker starts IDLE so the quiet baseline is measured on a daemon that
# already owns both panes — the only variable between the two samples is the
# escape sequence.
attacker_json="$(sx --format json session create "${attacker}" -d -- \
  env TERM=xterm-256color COLORTERM=truecolor bash --noprofile --norc)"
attacker_pane="$(jq -r '.pane_id' <<<"${attacker_json}")"

# Both panes get the same, realistic, large geometry.
sx pane set-size -s "${victim}" -p "${victim_pane}" --cols "${cols}" --rows "${rows}" >/dev/null
sx pane set-size -s "${attacker}" -p "${attacker_pane}" --cols "${cols}" --rows "${rows}" >/dev/null

# Colour-probed content on the victim, so `capture` reads a real screen and the
# same pane can be screenshotted as visual evidence.
sx pane send-keys -s "${victim}" -p "${victim_pane}" --text \
  $'clear; printf "\\033[1;38;2;120;220;180mVICTIM PANE\\033[0m  \\033[38;5;196mINDEXED\\033[0m  \\033[34mBASIC\\033[0m\\n"\n' >/dev/null
sleep 0.8

sample_latency() {
  local label="$1"
  python3 - "${shux_bin}" "${runtime}" "${victim}" "${victim_pane}" "${samples}" "${label}" <<'PY'
import json, os, subprocess, sys, time
shux, runtime, session, pane, n, label = sys.argv[1:7]
env = dict(os.environ, XDG_RUNTIME_DIR=runtime)
env.pop("SHUX_SOCKET", None)
lat = []
for _ in range(int(n)):
    t0 = time.monotonic()
    subprocess.run([shux, "pane", "capture", "-s", session, "-p", pane],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   env=env, check=False)
    lat.append((time.monotonic() - t0) * 1000.0)
print(json.dumps({"label": label, "samples_ms": lat}))
PY
}

quiet="$(sample_latency quiet)"

# Kick the attack off, let it saturate, then re-sample.
sx pane send-keys -s "${attacker}" -p "${attacker_pane}" --text "exec ${attack_script}"$'\n' >/dev/null
sleep "${attack_seconds}"

under_attack="$(sample_latency under_attack)"

verdict="$(python3 - "${quiet}" "${under_attack}" "${cols}" "${rows}" <<'PY'
import json, statistics, sys
quiet = json.loads(sys.argv[1])["samples_ms"]
attack = json.loads(sys.argv[2])["samples_ms"]
cols, rows = int(sys.argv[3]), int(sys.argv[4])

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
    "geometry": f"{cols}x{rows}",
    "victim_capture_latency": {"quiet": q, "under_attack": a},
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
