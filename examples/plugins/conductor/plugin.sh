#!/usr/bin/env bash
# shux-conductor v0.1 — VT-poll watchdog for coding-agent panes.
#
# Subscribes to pane.created / pane.exited. When a pane's command
# looks like a known agent (claude / codex / opencode / gemini),
# polls `pane.capture` every POLL_MS, classifies the visible state
# from a small regex set, and updates the pane's title via
# `pane.set_title` to surface live state in the border:
#
#   ✱ ready       agent prompt visible, waiting for input
#   ⚡ thinking   agent is generating a response
#   ✓ idle       no grid change for SETTLE_MS
#   ! stuck       a trust-prompt is visible — auto-dismissed via Enter
#
# Setup (default-deny permission model in v0.20+):
#   shux plugin install ./examples/plugins/conductor/plugin.sh
#   shux plugin grant conductor pane.capture
#   shux plugin grant conductor pane.set_title
#   shux plugin grant conductor pane.send_keys
#
# Conductor doesn't own the user's agent panes, so each method needs
# an explicit grant. The plugin emits a one-shot stderr line listing
# the missing grants on first denied call so the user sees what to do.
#
# Single bash file, jq + grep deps. No background subshells — one
# main loop multiplexes stdin (events + RPC responses) and a poll
# tick. RPC responses are correlated by `id`; events without `id`
# are dispatched to the event handler.

set -u

POLL_MS="${SHUX_CONDUCTOR_POLL_MS:-2000}"
SETTLE_MS="${SHUX_CONDUCTOR_SETTLE_MS:-5000}"
AUTO_DISMISS="${SHUX_CONDUCTOR_AUTO_DISMISS:-1}"
CAPTURE_LINES="${SHUX_CONDUCTOR_CAPTURE_LINES:-40}"

# --- agent detection -----------------------------------------------
# Map command-basename → display agent name. A pane's `command[0]`
# (basename) must match one of these to start being tracked.
agent_for_command() {
    local cmd="$1"
    case "$(basename "$cmd")" in
        claude)   echo "claude"   ;;
        codex)    echo "codex"    ;;
        opencode) echo "opencode" ;;
        gemini)   echo "gemini"   ;;
        *)        return 1        ;;
    esac
}

# --- state classifier ----------------------------------------------
# Reads captured pane text on stdin, prints one of:
#   stuck | thinking | ready | empty
# (idle is computed by the main loop via SETTLE_MS, not from text.)
#
# Patterns are intentionally loose so they survive minor agent
# version drift. Tight per-agent patterns will live in pattern files
# starting in v0.2 — for now we ship one set that covers all four
# agents reasonably well.
classify_grid() {
    local text="$1"
    if [ -z "$text" ]; then
        echo "empty"
        return
    fi
    # Trust / permission / yes-no prompts → stuck (auto-dismiss target).
    if printf '%s' "$text" | grep -qiE \
        '(do you trust|allow this|press enter to (continue|approve)|\[y/n\]|y\)es.*\bn\)o|continue\? )'; then
        echo "stuck"
        return
    fi
    # Active generation: spinner glyphs, "Thinking", "Working",
    # "Generating", or claude/codex/gemini's specific progress lines.
    if printf '%s' "$text" | grep -qiE \
        '(thinking|working|generating|esc to interrupt|⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏)'; then
        echo "thinking"
        return
    fi
    echo "ready"
}

# Pick a single-char marker for the title. The shux rasterizer
# embeds JetBrains Mono Regular, which has solid coverage of the
# geometric-shapes / dingbats blocks but no emoji and no
# `✱`/`⚡`. The four glyphs below are all present in JBM and
# render cleanly in `pane.snapshot` PNGs.
emoji_for_state() {
    case "$1" in
        idle)     echo "✓" ;;   # U+2713 CHECK MARK
        thinking) echo "●" ;;   # U+25CF BLACK CIRCLE (in-flight)
        stuck)    echo "!" ;;   # ASCII bang for attention
        ready)    echo "○" ;;   # U+25CB WHITE CIRCLE (waiting)
        empty)    echo "·" ;;   # U+00B7 MIDDLE DOT
        *)        echo "?" ;;
    esac
}

# --- jq-on-line helper ---------------------------------------------
jqr() { jq -r "$@" 2>/dev/null || true; }

# --- RPC plumbing --------------------------------------------------
# Generate a unique request id per call. We can't increment a counter
# in $(next_id) because $() spawns a subshell — the parent's RPC_ID
# would never advance. Use PID + microsecond clock instead: PID is
# stable across the plugin's lifetime, EPOCHREALTIME ticks every
# call. Guaranteed unique within one process.
next_id() {
    local us="${EPOCHREALTIME//.}"
    echo "$$${us:0:13}"
}

# Send an RPC frame to the daemon (stdout = daemon stdin).
send_rpc() {
    local method="$1" params="$2" id="$3"
    printf '{"jsonrpc":"2.0","method":"%s","params":%s,"id":%s}\n' \
        "$method" "$params" "$id"
}

# Send a fire-and-forget RPC (no response needed). Still uses an id
# so a future audit-log entry has a request-id; we just drop the
# response when it arrives.
fire_rpc() {
    local method="$1" params="$2"
    send_rpc "$method" "$params" "$(next_id)"
}

# Wait synchronously for a response with the given `id`. Any other
# inbound frames (events, unrelated responses) are dispatched in
# the meantime. Returns the response line on stdout. A 5s timeout
# guards against the daemon hanging — on timeout, returns empty.
await_response() {
    local want="$1"
    local deadline_ms="$(($(epoch_ms) + 5000))"
    local line
    while [ "$(epoch_ms)" -lt "$deadline_ms" ]; do
        if ! IFS= read -r -t 0.5 line; then
            continue
        fi
        case "$line" in
            *"\"id\":$want,"*|*"\"id\":$want}"*)
                printf '%s' "$line"
                return 0
                ;;
            *)
                dispatch_async "$line"
                ;;
        esac
    done
    return 1
}

# --- async dispatcher ----------------------------------------------
# Anything off stdin that isn't the response we're waiting for goes
# through here. v0.1 cares about pane.created and pane.exited events;
# everything else is logged at debug level (stderr) and dropped.
dispatch_async() {
    local line="$1"
    local method
    method=$(printf '%s' "$line" | jqr '.method // empty')
    case "$method" in
        event)
            local type
            type=$(printf '%s' "$line" | jqr '.params.type // empty')
            case "$type" in
                pane.created) handle_pane_created "$line" ;;
                pane.exited)  handle_pane_exited "$line"  ;;
            esac
            ;;
        plugin.shutdown)
            exit 0
            ;;
    esac
}

# Pull a numeric epoch in milliseconds. Falls back to seconds × 1000
# on systems without GNU date (BSD/macOS).
epoch_ms() {
    # bash 5+: EPOCHREALTIME is "<seconds>.<micros>", no subprocess.
    # Drop the dot, keep the first 13 chars → milliseconds. Fast path
    # on every supported platform (macOS users run brew bash; Linux
    # distros ship bash 5+).
    if [ -n "${EPOCHREALTIME:-}" ]; then
        local us="${EPOCHREALTIME//.}"
        echo "${us:0:13}"
    elif date +%s%3N 2>/dev/null | grep -qE '^[0-9]+$'; then
        # GNU date — `%3N` only works here, NOT on BSD date which
        # returns literal `17787068493N` (digit-prefixed, defeats
        # naive `^[0-9]` checks).
        date +%s%3N
    else
        python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null \
            || echo "$(($(date +%s) * 1000))"
    fi
}

# --- per-pane state ------------------------------------------------
# Bash 4+ associative arrays. macOS ships bash 3 by default — the
# plugin shebang is /usr/bin/env bash but users on stock macOS will
# need to install a newer bash via homebrew. Documented in README.
declare -A TRACKED          # pane_id -> agent name
declare -A LAST_HASH        # pane_id -> sha256 of last grid capture
declare -A LAST_CHANGE_AT   # pane_id -> epoch ms when grid last changed
declare -A LAST_STATE       # pane_id -> last classified state
declare -A LAST_TITLE_SET   # pane_id -> last title we wrote (dedup)

# Issue a permissions warning to stderr the first time a method fails
# with -32004. Subsequent denials suppressed so we don't spam the log.
PERMISSION_HINT_SENT=0
maybe_log_permission_hint() {
    if [ "$PERMISSION_HINT_SENT" -eq 1 ]; then return; fi
    PERMISSION_HINT_SENT=1
    {
        echo "conductor: permission denied by daemon. Conductor needs:"
        echo "  shux plugin grant conductor pane.capture"
        echo "  shux plugin grant conductor pane.set_title"
        echo "  shux plugin grant conductor pane.send_keys"
    } >&2
}

# Track a newly-created pane if its command matches a known agent.
handle_pane_created() {
    local line="$1"
    local pane_id command first_cmd agent
    pane_id=$(printf '%s' "$line" | jqr '.params.data.data.pane_id // empty')
    command=$(printf '%s' "$line" | jqr '.params.data.data.command // []')
    first_cmd=$(printf '%s' "$command" | jqr '.[0] // empty')
    [ -n "$first_cmd" ] || return 0
    agent=$(agent_for_command "$first_cmd") || return 0
    TRACKED[$pane_id]="$agent"
    LAST_HASH[$pane_id]=""
    LAST_CHANGE_AT[$pane_id]=$(epoch_ms)
    LAST_STATE[$pane_id]="ready"
    echo "conductor: tracking $agent in pane $pane_id" >&2
    # Eagerly set the title to ready so the user sees we're alive.
    set_pane_title "$pane_id" "$agent · $(emoji_for_state ready)"
}

handle_pane_exited() {
    local line="$1"
    local pane_id
    pane_id=$(printf '%s' "$line" | jqr '.params.data.data.pane_id // empty')
    [ -n "$pane_id" ] || return 0
    if [ -n "${TRACKED[$pane_id]:-}" ]; then
        echo "conductor: stopped tracking pane $pane_id (${TRACKED[$pane_id]})" >&2
        unset 'TRACKED[$pane_id]'
        unset 'LAST_HASH[$pane_id]'
        unset 'LAST_CHANGE_AT[$pane_id]'
        unset 'LAST_STATE[$pane_id]'
        unset 'LAST_TITLE_SET[$pane_id]'
    fi
}

# --- RPC ergonomics ------------------------------------------------
# pane.capture returns the visible text under .result.text. The
# captured text is left in the global LAST_CAPTURE_TEXT — we must
# NOT print to stdout from inside this function because callers
# would have to wrap the call in $() to capture it, and $() spawns
# a subshell that swallows send_rpc's frame instead of forwarding
# it to the daemon. Mutating a global is the cheapest fix.
LAST_CAPTURE_TEXT=""
capture_pane() {
    local pane_id="$1"
    LAST_CAPTURE_TEXT=""
    local params
    params=$(printf '{"pane_id":%s,"lines":%s}' \
        "$(jq -cn --arg p "$pane_id" '$p')" "$CAPTURE_LINES")
    local id resp err_code
    id=$(next_id)
    send_rpc "pane.capture" "$params" "$id"
    if ! resp=$(await_response "$id"); then
        return 1
    fi
    err_code=$(printf '%s' "$resp" | jqr '.error.code // empty')
    if [ -n "$err_code" ]; then
        if [ "$err_code" = "-32004" ]; then
            maybe_log_permission_hint
        fi
        return 1
    fi
    LAST_CAPTURE_TEXT=$(printf '%s' "$resp" | jqr '.result.text // empty')
}

# Set the pane border title. Deduplicates so we don't churn the bus.
set_pane_title() {
    local pane_id="$1" title="$2"
    if [ "${LAST_TITLE_SET[$pane_id]:-}" = "$title" ]; then return; fi
    LAST_TITLE_SET[$pane_id]="$title"
    local params
    params=$(jq -cn --arg p "$pane_id" --arg t "$title" \
        '{pane_id: $p, title: $t}')
    fire_rpc "pane.set_title" "$params"
}

# Send a literal Enter keypress (base64 "DQ==" = "\r") to dismiss a
# trust prompt. Fire-and-forget; if denied, the next poll classifies
# the pane as still stuck and we don't loop frantically (one Enter
# per poll tick maximum).
send_enter() {
    local pane_id="$1"
    local params
    params=$(jq -cn --arg p "$pane_id" '{pane_id: $p, data: "DQ=="}')
    fire_rpc "pane.send_keys" "$params"
}

# --- poll tick -----------------------------------------------------
# Iterate every tracked pane, capture text, classify, update title.
# Cheap hash on the captured text decides if anything changed; if
# the grid is unchanged for >= SETTLE_MS we promote ready → idle.
poll_tick() {
    local pane_id agent text hash now state new_state
    now=$(epoch_ms)
    for pane_id in "${!TRACKED[@]}"; do
        agent="${TRACKED[$pane_id]}"
        capture_pane "$pane_id" || continue
        text="$LAST_CAPTURE_TEXT"
        hash=$(printf '%s' "$text" | shasum -a 256 | cut -d' ' -f1)
        if [ "$hash" != "${LAST_HASH[$pane_id]:-}" ]; then
            LAST_HASH[$pane_id]="$hash"
            LAST_CHANGE_AT[$pane_id]=$now
        fi
        new_state=$(classify_grid "$text")
        state="${LAST_STATE[$pane_id]:-ready}"
        local since=$((now - LAST_CHANGE_AT[$pane_id]))
        # Promote ready → idle if the grid has been still for SETTLE_MS.
        if [ "$new_state" = "ready" ] && [ "$since" -ge "$SETTLE_MS" ]; then
            new_state="idle"
        fi
        # Auto-dismiss stuck prompts.
        if [ "$new_state" = "stuck" ] && [ "$AUTO_DISMISS" = "1" ]; then
            send_enter "$pane_id"
        fi
        if [ "$new_state" != "$state" ]; then
            LAST_STATE[$pane_id]="$new_state"
        fi
        set_pane_title "$pane_id" "$agent · $(emoji_for_state "$new_state")"
    done
}

# --- main loop -----------------------------------------------------
# Handshake: read the daemon's plugin.init, reply with manifest.
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"conductor","version":"0.1.0","subscribes":["pane.created","pane.exited"],"provides":[],"capabilities":[]}}'

last_poll=$(epoch_ms)
echo "conductor: v0.1 watchdog up (poll_ms=$POLL_MS, settle_ms=$SETTLE_MS, auto_dismiss=$AUTO_DISMISS)" >&2

while true; do
    # Wait briefly for inbound; if nothing arrives, fall through to
    # the poll-tick check. 0.5s gives 4 read attempts per default
    # 2s poll cadence — fast enough to feel responsive on event
    # delivery, slow enough not to spin the CPU.
    if IFS= read -r -t 0.5 line; then
        dispatch_async "$line"
    fi
    now=$(epoch_ms)
    if [ "$((now - last_poll))" -ge "$POLL_MS" ]; then
        poll_tick
        last_poll=$now
    fi
done
