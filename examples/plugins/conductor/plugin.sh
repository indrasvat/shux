#!/usr/bin/env bash
# shux-conductor v0.3 — VT-poll watchdog + settle-snapshot archive
# + multi-pane window aggregation + OS notifications, for
# coding-agent panes.
#
# Subscribes to pane.created / pane.exited. When a pane's command
# looks like a known agent (claude / codex / opencode / gemini),
# polls `pane.capture` every POLL_MS, classifies the visible state
# from a small regex set, and updates the pane's title via
# `pane.set_title` to surface live state in the border:
#
#   ○ ready       agent prompt visible, waiting for input
#   ● thinking   agent is generating a response
#   ✓ idle       no grid change for SETTLE_MS
#   ! stuck       a trust-prompt is visible — auto-dismissed via Enter
#
# v0.2 — settle-snapshot archive ⭐ (shux-unique). On every
# ready→idle transition, conductor calls `pane.snapshot` and saves
# the resulting PNG to `.shux/conductor/snapshots/<agent>-<short>-<ts>.png`,
# appending one row per snapshot to `INDEX.tsv`. No other agent
# watchdog can do this — they don't own a rasterizer.
#
# v0.3 — multi-pane window aggregation. Conductor tracks per-window
# in-flight counts (`thinking`/`stuck`/`ready` panes). The moment a
# window's last in-flight pane settles to `idle`, conductor fires
# ONE OS notification (`osascript display notification` on macOS,
# `notify-send` on Linux). Empty windows or windows with no agent
# panes are silent.
#
# Setup (default-deny permission model in v0.20+):
#   shux plugin install ./examples/plugins/conductor/plugin.sh
#   shux plugin grant conductor pane.capture
#   shux plugin grant conductor pane.set_title
#   shux plugin grant conductor pane.send_keys
#   shux plugin grant conductor pane.snapshot   # v0.2+, for settle archive
#
# Conductor doesn't own the user's agent panes, so each method needs
# an explicit grant. The plugin emits a one-shot stderr line listing
# the missing grants on first denied call so the user sees what to do.
#
# Single bash file, jq + grep + (optionally) `osascript` /
# `notify-send` deps. No background subshells — one main loop
# multiplexes stdin (events + RPC responses) and a poll tick. RPC
# responses are correlated by `id`; events without `id` are
# dispatched to the event handler.

set -u

POLL_MS="${SHUX_CONDUCTOR_POLL_MS:-2000}"
SETTLE_MS="${SHUX_CONDUCTOR_SETTLE_MS:-5000}"
AUTO_DISMISS="${SHUX_CONDUCTOR_AUTO_DISMISS:-1}"
CAPTURE_LINES="${SHUX_CONDUCTOR_CAPTURE_LINES:-40}"
# v0.2 settle-snapshot archive
SNAPSHOTS="${SHUX_CONDUCTOR_SNAPSHOTS:-1}"
SNAPSHOT_DIR="${SHUX_CONDUCTOR_SNAPSHOT_DIR:-.shux/conductor/snapshots}"
SNAPSHOT_COLS="${SHUX_CONDUCTOR_SNAPSHOT_COLS:-100}"
SNAPSHOT_ROWS="${SHUX_CONDUCTOR_SNAPSHOT_ROWS:-32}"
# v0.3 OS notifications
NOTIFY="${SHUX_CONDUCTOR_NOTIFY:-system}"   # system | stdout | off
NOTIFY_TITLE="${SHUX_CONDUCTOR_NOTIFY_TITLE:-shux-conductor}"

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
declare -A WINDOW_OF        # pane_id -> window_id (v0.3 — for window-level aggregation)
declare -A SESSION_OF       # pane_id -> session_id (v0.3 — for notification context)
declare -A WINDOW_INFLIGHT  # window_id -> count of agent panes whose state is NOT idle
declare -A WINDOW_NOTIFIED  # window_id -> 1 if we already fired the "all settled" notification
                            # for this batch (cleared when any pane in the window leaves idle)

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
    local pane_id window_id session_id command first_cmd agent
    pane_id=$(printf '%s' "$line" | jqr '.params.data.data.pane_id // empty')
    window_id=$(printf '%s' "$line" | jqr '.params.data.data.window_id // empty')
    session_id=$(printf '%s' "$line" | jqr '.params.data.data.session_id // empty')
    command=$(printf '%s' "$line" | jqr '.params.data.data.command // []')
    first_cmd=$(printf '%s' "$command" | jqr '.[0] // empty')
    [ -n "$first_cmd" ] || return 0
    agent=$(agent_for_command "$first_cmd") || return 0
    TRACKED[$pane_id]="$agent"
    LAST_HASH[$pane_id]=""
    LAST_CHANGE_AT[$pane_id]=$(epoch_ms)
    LAST_STATE[$pane_id]="ready"
    WINDOW_OF[$pane_id]="$window_id"
    SESSION_OF[$pane_id]="$session_id"
    # New agent pane → bump in-flight count for the containing
    # window. Notification will fire later when this drops to 0.
    if [ -n "$window_id" ]; then
        WINDOW_INFLIGHT[$window_id]=$(( ${WINDOW_INFLIGHT[$window_id]:-0} + 1 ))
        unset 'WINDOW_NOTIFIED[$window_id]'
    fi
    echo "conductor: tracking $agent in pane $pane_id (window=$window_id)" >&2
    # Eagerly set the title to ready so the user sees we're alive.
    set_pane_title "$pane_id" "$agent · $(emoji_for_state ready)"
}

handle_pane_exited() {
    local line="$1"
    local pane_id window_id was_inflight
    pane_id=$(printf '%s' "$line" | jqr '.params.data.data.pane_id // empty')
    [ -n "$pane_id" ] || return 0
    if [ -n "${TRACKED[$pane_id]:-}" ]; then
        window_id="${WINDOW_OF[$pane_id]:-}"
        was_inflight=0
        # Was this pane in-flight (not idle)? If so, decrement the
        # window's in-flight count; otherwise idle exits already
        # decremented when they transitioned to idle.
        case "${LAST_STATE[$pane_id]:-ready}" in
            idle) ;;
            *) was_inflight=1 ;;
        esac
        echo "conductor: stopped tracking pane $pane_id (${TRACKED[$pane_id]})" >&2
        unset 'TRACKED[$pane_id]'
        unset 'LAST_HASH[$pane_id]'
        unset 'LAST_CHANGE_AT[$pane_id]'
        unset 'LAST_STATE[$pane_id]'
        unset 'LAST_TITLE_SET[$pane_id]'
        unset 'WINDOW_OF[$pane_id]'
        unset 'SESSION_OF[$pane_id]'
        if [ -n "$window_id" ] && [ "$was_inflight" = "1" ]; then
            WINDOW_INFLIGHT[$window_id]=$(( ${WINDOW_INFLIGHT[$window_id]:-1} - 1 ))
            maybe_notify_window_settled "$window_id"
        fi
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

# --- v0.2: settle-snapshot archive ---------------------------------
# Save a PNG of the pane the moment it goes idle. Synchronous so the
# index entry only lands after the file write succeeds. If the
# daemon denies pane.snapshot (no grant), log the permission hint
# and skip — failure here must not block the title-update flow.
SNAPSHOT_INDEX_HEADER_WRITTEN=0
snapshot_pane_settled() {
    local pane_id="$1" agent="$2"
    [ "$SNAPSHOTS" = "1" ] || return 0
    mkdir -p "$SNAPSHOT_DIR" 2>/dev/null || {
        echo "conductor: cannot create snapshot dir $SNAPSHOT_DIR" >&2
        return 1
    }
    local short ts iso path index params id resp err_code rel
    short="${pane_id:0:8}"
    ts=$(epoch_ms)
    iso=$(iso_now)
    rel="${agent}-${short}-${iso}.png"
    # Sanitize the colons in ISO 8601 ("14:32:09" → "14-32-09") so
    # the path works on every fs we care about (Windows, mostly,
    # where colons are reserved).
    rel=${rel//:/-}
    path="$SNAPSHOT_DIR/$rel"
    index="$SNAPSHOT_DIR/INDEX.tsv"

    # pane.snapshot returns base64-encoded PNG bytes in
    # `.result.png_base64` — it does NOT write to disk. Decode and
    # write here. cols/rows on the params are ignored by the daemon
    # handler; it always renders the VT's current dimensions. Resize
    # the pane via pane.set_size first if you need a larger canvas.
    params=$(jq -cn --arg p "$pane_id" '{pane_id: $p}')
    id=$(next_id)
    send_rpc "pane.snapshot" "$params" "$id"
    if ! resp=$(await_response "$id"); then
        echo "conductor: pane.snapshot timed out for $pane_id" >&2
        return 1
    fi
    err_code=$(printf '%s' "$resp" | jqr '.error.code // empty')
    if [ -n "$err_code" ]; then
        if [ "$err_code" = "-32004" ]; then maybe_log_permission_hint; fi
        echo "conductor: pane.snapshot rejected ($err_code) for $pane_id" >&2
        return 1
    fi

    local b64
    b64=$(printf '%s' "$resp" | jqr '.result.png_base64 // empty')
    if [ -z "$b64" ]; then
        echo "conductor: pane.snapshot returned no png_base64 for $pane_id" >&2
        return 1
    fi
    if ! printf '%s' "$b64" | base64 -d > "$path" 2>/dev/null; then
        # GNU coreutils prefers --decode; BSD prefers -D. Try both.
        if ! printf '%s' "$b64" | base64 --decode > "$path" 2>/dev/null; then
            echo "conductor: base64 decode failed for $pane_id" >&2
            return 1
        fi
    fi

    # Append to the rolling index — one row per snapshot, tab-separated:
    #   ts_ms<TAB>iso<TAB>agent<TAB>session_id<TAB>window_id<TAB>pane_id<TAB>relpath
    # Rolling, never rotated by conductor; users can `head` / `awk`
    # / `mv` it themselves. Header is written exactly once per
    # process run so re-installs don't duplicate it mid-file.
    if [ "$SNAPSHOT_INDEX_HEADER_WRITTEN" = "0" ] && [ ! -s "$index" ]; then
        printf 'ts_ms\tiso\tagent\tsession_id\twindow_id\tpane_id\trel_path\n' >> "$index"
        SNAPSHOT_INDEX_HEADER_WRITTEN=1
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$ts" "$iso" "$agent" \
        "${SESSION_OF[$pane_id]:-?}" \
        "${WINDOW_OF[$pane_id]:-?}" \
        "$pane_id" "$rel" >> "$index"
    echo "conductor: settle-snapshot $rel" >&2
}

# Conductor uses ISO 8601 in UTC with milliseconds (e.g.
# 2026-05-13T21:30:55.123Z) for snapshot filenames + index rows.
iso_now() {
    if date -u +%Y-%m-%dT%H:%M:%S.%3NZ 2>/dev/null | grep -qE '\.[0-9]{3}Z$'; then
        date -u +%Y-%m-%dT%H:%M:%S.%3NZ
    elif [ -n "${EPOCHREALTIME:-}" ]; then
        # Bash 5: build manually so macOS BSD date doesn't pollute
        # the filename with a literal `%3N`.
        local s us
        s="${EPOCHREALTIME%.*}"
        us="${EPOCHREALTIME#*.}"
        printf '%s.%sZ\n' \
            "$(date -u -r "$s" +%Y-%m-%dT%H:%M:%S 2>/dev/null \
                || date -u +%Y-%m-%dT%H:%M:%S)" \
            "${us:0:3}"
    else
        date -u +%Y-%m-%dT%H:%M:%SZ
    fi
}

# --- v0.3: window-level aggregation + OS notifications -------------
# Decrement the window's in-flight counter when a pane goes idle.
# When the counter hits 0, fire ONE notification (deduped by
# WINDOW_NOTIFIED until any pane in the window leaves idle again).
note_pane_idle() {
    local pane_id="$1"
    local window_id="${WINDOW_OF[$pane_id]:-}"
    [ -n "$window_id" ] || return 0
    local cur="${WINDOW_INFLIGHT[$window_id]:-0}"
    if [ "$cur" -gt 0 ]; then
        WINDOW_INFLIGHT[$window_id]=$(( cur - 1 ))
    fi
    maybe_notify_window_settled "$window_id"
}

# Inverse of note_pane_idle — call when a pane leaves the idle state
# (e.g., user typed something and we re-classified to "thinking").
# Re-bumps the in-flight counter and clears the notified flag so the
# next settle can fire a fresh notification.
note_pane_busy() {
    local pane_id="$1"
    local window_id="${WINDOW_OF[$pane_id]:-}"
    [ -n "$window_id" ] || return 0
    WINDOW_INFLIGHT[$window_id]=$(( ${WINDOW_INFLIGHT[$window_id]:-0} + 1 ))
    unset 'WINDOW_NOTIFIED[$window_id]'
}

maybe_notify_window_settled() {
    local window_id="$1"
    [ -n "$window_id" ] || return 0
    local cur="${WINDOW_INFLIGHT[$window_id]:-0}"
    if [ "$cur" -gt 0 ]; then return 0; fi
    if [ "${WINDOW_NOTIFIED[$window_id]:-0}" = "1" ]; then return 0; fi
    WINDOW_NOTIFIED[$window_id]=1
    fire_notification "$window_id"
}

# Backend-aware notification dispatch. NOTIFY=stdout dumps a
# structured line on conductor's stderr so test harnesses can grep
# for it; NOTIFY=off skips entirely; NOTIFY=system tries
# osascript on macOS, notify-send on Linux, and falls back to
# stdout if neither is available.
fire_notification() {
    local window_id="$1"
    local body
    body="window $window_id: all agent panes idle"
    case "$NOTIFY" in
        off)    return 0 ;;
        stdout) echo "conductor[notify]: $body" >&2; return 0 ;;
        system|*)
            if command -v osascript >/dev/null 2>&1; then
                osascript -e "display notification \"$body\" with title \"$NOTIFY_TITLE\"" >/dev/null 2>&1 \
                    && return 0
            fi
            if command -v notify-send >/dev/null 2>&1; then
                notify-send "$NOTIFY_TITLE" "$body" >/dev/null 2>&1 \
                    && return 0
            fi
            echo "conductor[notify]: $body" >&2
            ;;
    esac
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
            # v0.2: ready/anything → idle is a SETTLE event. Snapshot
            # exactly once per transition. The dedup is implicit — we
            # only enter this branch when the state actually flipped.
            if [ "$state" != "idle" ] && [ "$new_state" = "idle" ]; then
                snapshot_pane_settled "$pane_id" "$agent"
            fi
            # v0.3: window-level in-flight counter bookkeeping. Pane
            # going idle decrements; pane leaving idle re-bumps and
            # clears the per-window notified flag so the next settle
            # can fire a fresh notification.
            if [ "$state" != "idle" ] && [ "$new_state" = "idle" ]; then
                note_pane_idle "$pane_id"
            elif [ "$state" = "idle" ] && [ "$new_state" != "idle" ]; then
                note_pane_busy "$pane_id"
            fi
        fi
        set_pane_title "$pane_id" "$agent · $(emoji_for_state "$new_state")"
    done
}

# --- main loop -----------------------------------------------------
# Handshake: read the daemon's plugin.init, reply with manifest.
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"conductor","version":"0.3.0","subscribes":["pane.created","pane.exited"],"provides":[],"capabilities":[]}}'

last_poll=$(epoch_ms)
echo "conductor: v0.3 up (poll_ms=$POLL_MS, settle_ms=$SETTLE_MS, snapshots=$SNAPSHOTS, notify=$NOTIFY)" >&2

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
