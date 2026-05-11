#!/usr/bin/env bash
# raster_drive_vivecaka.sh — repeatable scripted-interaction + PNG
# snapshot regression test driven 100% through shux RPC. The whole loop —
# spawn TUI, resize, send keys, capture pixels — runs without iTerm2,
# without a display server, and without any GUI runner.
#
# Usage:
#   bash .claude/automations/raster_drive_vivecaka.sh            # default
#   SHUX_BIN=~/.local/bin/shux bash .claude/automations/raster_drive_vivecaka.sh
#   REPO=indrasvat/shux bash .claude/automations/raster_drive_vivecaka.sh
#   OUT_DIR=/tmp/raster bash .claude/automations/raster_drive_vivecaka.sh
#
# Outputs PNGs to ${OUT_DIR:-.claude/screenshots/scenario_vivecaka}/
# Idempotent — re-run safely; same labels overwrite the same files.

set -euo pipefail

SHUX="${SHUX_BIN:-target/release/shux}"
REPO="${REPO:-indrasvat/kartaa}"
SESSION="${SHUX_SESSION:-raster_demo}"
COLS="${COLS:-200}"
ROWS="${ROWS:-60}"
OUT_DIR="${OUT_DIR:-.claude/screenshots/scenario_vivecaka}"
TEMPLATE="${TEMPLATE:-.claude/templates/vivecaka.toml}"

# Pre-encoded control byte sequences for `pane.send_keys --data`.
B64_ESC=$(printf '\033' | base64)
B64_ENTER=$(printf '\r' | base64)
B64_TAB=$(printf '\t' | base64)
# shellcheck disable=SC2034  # kept for downstream scenarios
B64_BACKSPACE=$(printf '\177' | base64)

mkdir -p "$OUT_DIR"

log () { printf '\033[1;36m[scenario]\033[0m %s\n' "$*"; }
die () { printf '\033[1;31m[scenario]\033[0m %s\n' "$*" >&2; exit 1; }

# Tear down any prior run, then apply the template fresh.
"$SHUX" kill -s "$SESSION" >/dev/null 2>&1 || true
log "applying template $TEMPLATE (repo=$REPO)"

# Apply the template (uses state.apply atomically). The template's
# default repo argument can be overridden by editing the TOML or by
# constructing the session inline via session.create. We do the inline
# path here when REPO != default so that a single script works across
# any repo without disk edits.
# Use the declarative template path when REPO matches the template's
# default; otherwise drop down to session.create to override the repo
# arg without editing the TOML on disk.
DEFAULT_REPO="indrasvat/kartaa"
if [[ "$REPO" == "$DEFAULT_REPO" ]]; then
    "$SHUX" apply "$TEMPLATE" >/dev/null \
        || die "shux apply $TEMPLATE failed"
    PID=$("$SHUX" api session.list '{}' | python3 -c "
import json, sys
sess = next(s for s in json.load(sys.stdin)['result']['sessions'] if s['name'] == '$SESSION')
print(sess['pane_id'])
")
else
    log "overriding repo to $REPO (inline session.create)"
    RESP=$("$SHUX" api session.create \
        "{\"name\":\"$SESSION\",\"command\":[\"/Users/indrasvat/.local/bin/vivecaka\",\"--repo\",\"$REPO\"]}")
    PID=$(printf '%s' "$RESP" \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["pane_id"])')
fi
[[ -n "$PID" ]] || die "could not resolve pane id"
log "session=$SESSION pane=$PID"

log "resizing pane to ${COLS}x${ROWS}"
"$SHUX" api pane.set_size "{\"pane_id\":\"$PID\",\"cols\":$COLS,\"rows\":$ROWS}" >/dev/null

snap () {
    local label="$1"
    local out="$OUT_DIR/${label}.png"
    "$SHUX" api pane.snapshot "{\"pane_id\":\"$PID\"}" | python3 -c "
import json,sys,base64,pathlib
r = json.load(sys.stdin)['result']
pathlib.Path('$out').write_bytes(base64.b64decode(r['png_base64']))
print(f'  → $out ({r[\"width\"]}x{r[\"height\"]} px, grid {r[\"cols\"]}x{r[\"rows\"]})')
"
}

send_text () { "$SHUX" api pane.send_keys "{\"pane_id\":\"$PID\",\"text\":$1}" >/dev/null; }
send_b64  () { "$SHUX" api pane.send_keys "{\"pane_id\":\"$PID\",\"data\":\"$1\"}" >/dev/null; }
wait_ms   () { python3 -c "import time;time.sleep($1/1000)"; }

# -----------------------------------------------------------------------
# The scenario — each row is one (action, value, sleep_ms, label) tuple.
# Adding/removing/reordering steps here is the entire authoring surface
# for a new visual-regression test. Try copying this file and editing the
# array to script a different app.
#
#   wait   :  pure sleep, no input — used to let the TUI settle.
#   text   :  send a literal string (JSON-string-quoted in the call).
#   esc    :  send the Escape control byte.
#   enter  :  send the Enter control byte.
#   tab    :  send the Tab control byte.
#   snap   :  rasterize the current pane to OUT_DIR/<label>.png.
# -----------------------------------------------------------------------
SCENARIO=(
  "wait :          : 6000 : -"
  "snap :          :    0 : 01_splash"
  "wait :          : 7000 : -"
  "snap :          :    0 : 02_pr_list_loaded"
  "text : \"j\"    :  600 : -"
  "text : \"j\"    :  600 : -"
  "snap :          :    0 : 03_after_jj_nav"
  "text : \"/\"    : 1000 : -"
  "snap :          :    0 : 04_search_prompt"
  "text : \"actions\" : 1500 : -"
  "snap :          :    0 : 05_search_filtered"
  "esc  :          : 1000 : -"
  "snap :          :    0 : 06_search_cleared"
  "text : \"?\"    : 1500 : -"
  "snap :          :    0 : 07_help_overlay"
  "esc  :          : 1000 : -"
  "enter :         : 3000 : -"
  "snap :          :    0 : 08_pr_detail"
  "tab  :          : 1500 : -"
  "snap :          :    0 : 09_tab_checks"
  "tab  :          : 1500 : -"
  "snap :          :    0 : 10_tab_files"
  "esc  :          : 1000 : -"
  "snap :          :    0 : 11_back_to_list"
)

for row in "${SCENARIO[@]}"; do
    # Strip surrounding whitespace from fields, but keep quoted strings intact.
    action="$(printf '%s' "$row" | awk -F: '{gsub(/^ +| +$/, "", $1); print $1}')"
    value="$( printf '%s' "$row" | awk -F: '{for(i=2;i<NF-1;i++) printf "%s%s",$i,(i<NF-2?":":""); }' | sed 's/^ *//; s/ *$//')"
    sleepms="$(printf '%s' "$row" | awk -F: '{gsub(/^ +| +$/, "", $(NF-1)); print $(NF-1)}')"
    label="$(printf '%s' "$row" | awk -F: '{gsub(/^ +| +$/, "", $NF); print $NF}')"

    case "$action" in
        wait)  : ;; # Just sleep below.
        text)  send_text "$value" ;;
        esc)   send_b64 "$B64_ESC" ;;
        enter) send_b64 "$B64_ENTER" ;;
        tab)   send_b64 "$B64_TAB" ;;
        snap)  snap "$label" ;;
        *)     die "unknown action: $action" ;;
    esac

    [[ "$sleepms" -gt 0 ]] && wait_ms "$sleepms"
done

log "tearing down session $SESSION"
"$SHUX" kill -s "$SESSION" >/dev/null 2>&1 || true

log "done — snapshots in $OUT_DIR/"
ls -la "$OUT_DIR" | tail -n +2
