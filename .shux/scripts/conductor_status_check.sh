#!/usr/bin/env bash
# shellcheck disable=SC2016  # wait_until evals its quoted condition later
# Conductor reads program status (OSC 7501) where a pane reports it, and
# scrapes only where it does not.
#   - A reporting agent that shows trust-prompt text is never auto-dismissed:
#     its reported `blocked` notifies instead, and its title carries no
#     conductor marker (shux draws the badge).
#   - An agent that never reports is still classified from its screen.
#   - Without the pane.status grant, conductor falls back to scraping.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
SHUX="${SHUX_BIN:-${REPO_ROOT}/target/debug/shux}"
PLUGIN_SRC="${REPO_ROOT}/examples/plugins/conductor/plugin.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/shux-cond.XXXXXX")"
export XDG_RUNTIME_DIR="${WORK}/run" XDG_CONFIG_HOME="${WORK}/cfg"
mkdir -p "${XDG_RUNTIME_DIR}" "${XDG_CONFIG_HOME}/shux" "${WORK}/bin"
LOG="${WORK}/conductor.log"
fail=0

cleanup() {
  "$SHUX" plugin kill conductor >/dev/null 2>&1 || true
  "$SHUX" daemon stop >/dev/null 2>&1 || true
  find "${WORK}" -delete 2>/dev/null || true
}
trap cleanup EXIT

check() { # name, condition exit status
  if [ "$2" -eq 0 ]; then
    printf '  \033[32m✓\033[0m %s\n' "$1"
  else
    printf '  \033[31m✗\033[0m %s\n' "$1"
    fail=1
  fi
}

# Run $1 until it succeeds, for up to $2 seconds.
wait_until() {
  local deadline=$((SECONDS + $2))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if eval "$1"; then return 0; fi
    sleep 0.2
  done
  return 1
}

title_of() { # session
  "$SHUX" --format json pane list --session "$1" | jq -r '(if type == "array" then . else .panes end)[0].title // empty'
}

# A reporting agent: trust-prompt text on screen, OSC 7501 working then
# blocked, and every byte it receives logged to $1.
cat > "${WORK}/bin/claude" <<'AGENT'
#!/usr/bin/env bash
printf '\033[38;2;120;220;180mtrue\033[0m \033[38;5;208midx\033[0m \033[31mbasic\033[0m\n'
printf 'Do you trust this folder? [y/n]\n'
printf '\033]7501;state=working:id=a\033\\'
sleep 1
printf '\033]7501;state=blocked:id=a:kind=permission\033\\'
stty raw -echo 2>/dev/null
exec cat > "$1"
AGENT
# An agent that never reports.
cat > "${WORK}/bin/codex" <<'AGENT'
#!/usr/bin/env bash
printf 'Thinking... (esc to interrupt)\n'
exec sleep 120
AGENT
chmod +x "${WORK}/bin/claude" "${WORK}/bin/codex"

cat > "${WORK}/conductor.sh" <<WRAP
#!/usr/bin/env bash
exec env SHUX_CONDUCTOR_POLL_MS=300 SHUX_CONDUCTOR_SETTLE_MS=1500 SHUX_CONDUCTOR_SNAPSHOTS=0 \\
  SHUX_CONDUCTOR_NOTIFY=stdout bash "${PLUGIN_SRC}" 2>>"${LOG}"
WRAP
chmod +x "${WORK}/conductor.sh"
"$SHUX" session create base -d -- sleep 300 >/dev/null
"$SHUX" plugin install "${WORK}/conductor.sh" --no-watch >/dev/null
for m in pane.capture pane.set_title pane.send_keys pane.status; do
  "$SHUX" plugin grant conductor "$m" >/dev/null
done

echo "reporting agent, pane.status granted:"
"$SHUX" session create rep -d -- "${WORK}/bin/claude" "${WORK}/claude.stdin" >/dev/null
"$SHUX" session create scr -d -- "${WORK}/bin/codex" >/dev/null
if wait_until 'grep -q "is waiting for you" "$LOG"' 20; then ok=0; else ok=1; fi
check "a reported block notifies" "$ok"
sleep 1.5
if [ -e "${WORK}/claude.stdin" ] && [ ! -s "${WORK}/claude.stdin" ]; then ok=0; else ok=1; fi
check "the trust-prompt text was not auto-dismissed" "$ok"
if [ "$(title_of rep)" = claude ]; then ok=0; else ok=1; fi
check "the reporting pane's title carries no conductor marker ($(title_of rep))" "$ok"
if wait_until '[[ "$(title_of scr)" == "codex · ●" ]]' 10; then ok=0; else ok=1; fi
check "a silent agent is still classified from its screen ($(title_of scr))" "$ok"

echo "program status switched off (hot reload):"
printf '[program_status]\nenabled = false\n' > "${XDG_CONFIG_HOME}/shux/config.toml"
sleep 3
if [ -e "${WORK}/claude.stdin" ] && [ ! -s "${WORK}/claude.stdin" ]; then ok=0; else ok=1; fi
check "a pane that reported is not auto-dismissed once status is off" "$ok"
: > "${XDG_CONFIG_HOME}/shux/config.toml"
sleep 1

echo "reporting agent, pane.status revoked:"
"$SHUX" plugin revoke conductor pane.status >/dev/null
"$SHUX" session create rep2 -d -- "${WORK}/bin/claude" "${WORK}/claude2.stdin" >/dev/null
if wait_until '[ -s "${WORK}/claude2.stdin" ]' 10; then ok=0; else ok=1; fi
check "without the grant it falls back to scraping" "$ok"
if [ -e "${WORK}/claude.stdin" ] && [ ! -s "${WORK}/claude.stdin" ]; then ok=0; else ok=1; fi
check "a pane that once reported is still never auto-dismissed" "$ok"

if [ "$fail" -ne 0 ]; then
  echo "--- conductor log ---"
  tail -20 "$LOG"
  exit 1
fi
echo "CONDUCTOR-STATUS-OK"
