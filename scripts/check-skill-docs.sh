#!/usr/bin/env bash
# Assert agent-facing skill docs keep high-friction CLI/RPC shape gotchas explicit.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKILL="$ROOT/skills/shux/SKILL.md"

fail=0
err() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=1; }
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }

[[ -f "$SKILL" ]] || { err "missing required file: ${SKILL#"$ROOT"/}"; exit 1; }

printf '\033[34m▶ skill doc drift\033[0m\n'

skill_text="$(tr '\n' ' ' < "$SKILL")"
session_create_text="$(
  awk '
    /session create --format json/ { in_block = 1 }
    in_block { print }
    in_block && /session list --format json/ { exit }
  ' "$SKILL" | tr '\n' ' '
)"

if [[ "$skill_text" =~ session[[:space:]]+list[^.]*\.sessions.*pane[[:space:]]+list.*window[[:space:]]+list.*bare[[:space:]]+JSON[[:space:]]+array ]]; then
  ok "SKILL.md documents session/window/pane list JSON shape differences"
else
  err "SKILL.md must document that \`session list --format json\` wraps \`.sessions\` while \`pane list\` / \`window list\` return bare arrays"
fi

if [[ "$session_create_text" == *"session create --format json"* ]] \
  && [[ "$session_create_text" == *".id"* ]] \
  && [[ "$session_create_text" == *".window_id"* ]] \
  && [[ "$session_create_text" == *".pane_id"* ]] \
  && [[ "$session_create_text" == *"no \`.session_id\` field"* ]]; then
  ok "SKILL.md documents session create id/window_id/pane_id fields"
else
  err "SKILL.md must document that \`session create --format json\` returns \`.id\`, \`.window_id\`, and \`.pane_id\`, not \`.session_id\`"
fi

if [[ "$skill_text" =~ pane[[:space:]]+wait-for ]] \
  && [[ "$skill_text" =~ --text[[:space:]]+\'ready\' ]] \
  && [[ "$skill_text" =~ not[[:space:]]+a[[:space:]]+positional[[:space:]]+argument ]]; then
  ok "SKILL.md documents pane wait-for needle syntax"
else
  err "SKILL.md must document that \`pane wait-for\` uses \`--text\` or \`--regex\`, not a positional needle"
fi

if [[ "$skill_text" == *"pane.snapshot"* ]] \
  && [[ "$skill_text" == *"-o frame.png"* ]] \
  && [[ "$skill_text" == *"omit \`--format json\`"* ]] \
  && [[ "$skill_text" == *"png_base64"* ]]; then
  ok "SKILL.md documents pane snapshot -o JSON stdout behavior"
else
  err "SKILL.md must document that \`pane snapshot -o\` plus JSON still prints the full result including \`png_base64\`"
fi

echo
if [[ $fail -eq 0 ]]; then
  printf '\033[32m✓ skill docs match shipped CLI gotchas\033[0m\n'
else
  printf '\033[31m✗ skill doc drift detected\033[0m\n'
fi
exit $fail
