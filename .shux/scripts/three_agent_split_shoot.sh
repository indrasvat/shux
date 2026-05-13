#!/usr/bin/env bash
#
# three_agent_split_shoot.sh — drive a single shux window with 3 real
# agent CLIs side by side (claude · codex · opencode), nudge each one
# into mid-thought, then snapshot the WHOLE window (borders, per-pane
# titles, status bar) into one composed PNG.
#
# Output: pages/screenshots/multi-agent.png    (committed, used by the
#                                              landing page gallery)
#         .shux/out/multi-agent.png            (debug copy)
#
# Re-run anytime; idempotent. Requires `claude`, `codex`, `opencode` on
# PATH and a running shux daemon (auto-starts on first call).

set -euo pipefail

SHUX="${SHUX_BIN:-shux}"
SESSION="three-agents"
TEMPLATE=".shux/templates/three-agents.toml"
OUT=".shux/out/multi-agent.png"
FINAL="pages/screenshots/multi-agent.png"
COLS="${COLS:-220}"
ROWS="${ROWS:-50}"

# Tuned against splash timings — codex is slowest because of its
# first-run trust dialog + keychain init.
SPLASH_WAIT="${SPLASH_WAIT:-4}"
CODEX_TRUST_WAIT="${CODEX_TRUST_WAIT:-4}"
PROMPT_WAIT="${PROMPT_WAIT:-0.6}"

ENTER_B64=$(printf '\r' | base64)

mkdir -p "$(dirname "$OUT")"

# 1. idempotent teardown
"$SHUX" session kill "$SESSION" >/dev/null 2>&1 || true

# 2. atomic 3-pane spawn via the template
"$SHUX" state apply "$TEMPLATE" >/dev/null

# 3. let all three splashes draw
sleep "$SPLASH_WAIT"

# 4. resolve pane ids by their `command` field — `pane.list` doesn't
#    return panes in display order, so positional indexing would
#    randomly mis-route prompts and titles across panes.
PANES_JSON=$("$SHUX" --format json pane list -s "$SESSION")
pane_id_for () {
    # Match against the third argv element of `bash -lc <cmd>`, e.g.
    # `IS_DEMO=1 exec claude`. A simple contains is safe because
    # "claude" / "codex" / "opencode" are mutually unambiguous.
    printf '%s' "$PANES_JSON" \
        | jq -r --arg needle "$1" \
            '.[] | select(any(.command[]; test("\\b" + $needle + "\\b"))) | .id' \
        | head -1
}
P_CLAUDE=$(pane_id_for "claude")
P_CODEX=$(pane_id_for "codex")
P_OPENCODE=$(pane_id_for "opencode")
for v in P_CLAUDE P_CODEX P_OPENCODE; do
    [[ -n "${!v:-}" ]] || { echo "could not resolve pane for $v" >&2; exit 1; }
done

# 5. dismiss codex's first-run trust prompt (CR over the same RPC).
#    Repeat after a short settle in case the keymap wasn't ready on
#    the first press — same belt-and-braces pattern as the original
#    multi_agent_shoot.sh.
"$SHUX" pane send-keys -s "$SESSION" --pane "$P_CODEX" --data "$ENTER_B64" >/dev/null
sleep 0.8
"$SHUX" pane send-keys -s "$SESSION" --pane "$P_CODEX" --data "$ENTER_B64" >/dev/null
sleep "$CODEX_TRUST_WAIT"

# 6. tag each pane so the per-pane title overlays read agent-context.
"$SHUX" pane title -s "$SESSION" --pane "$P_CLAUDE"   --title "claude · refactor" >/dev/null
"$SHUX" pane title -s "$SESSION" --pane "$P_CODEX"    --title "codex · find leak"  >/dev/null
"$SHUX" pane title -s "$SESSION" --pane "$P_OPENCODE" --title "opencode · audit"   >/dev/null

# 7. type a contextual prompt into each agent's input box — no Enter.
#    Leaving prompts unsubmitted is the asciinema-paused frame; it also
#    avoids triggering real LLM API traffic from the demo. Match the
#    prior multi_agent_shoot.sh decision.
send_prompt () {
    local pid="$1" prompt="$2"
    "$SHUX" pane send-keys -s "$SESSION" --pane "$pid" --text "$prompt" >/dev/null
}

send_prompt "$P_CLAUDE"   "Refactor this Rust Fibonacci impl for clarity"
send_prompt "$P_CODEX"    "Find the memory leak in this Node.js worker"
send_prompt "$P_OPENCODE" "Audit this Python for thread-safety risks"

# 8. let each agent re-render after the input-box write
sleep "$PROMPT_WAIT"

# 9. composed window snapshot — borders, per-pane titles, statusbar
"$SHUX" window snapshot -s "$SESSION" --cols "$COLS" --rows "$ROWS" -o "$OUT" >/dev/null
cp "$OUT" "$FINAL"

# 10. teardown (commented out by default — leave the session up so you
#     can `shux session attach three-agents` and inspect manually)
# "$SHUX" session kill "$SESSION" >/dev/null

echo "→ $FINAL"
echo "  $(file "$FINAL" | sed 's/^[^:]*: //')"
echo
echo "Inspect interactively: shux session attach $SESSION"
echo "Tear down:             shux session kill $SESSION"
