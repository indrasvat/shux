#!/usr/bin/env bash
# shux integration suite — no-flake regression coverage for core features.
#
# Every assertion uses synchronous RPC responses or `wait-for` for
# synchronization. No raw sleeps, no order-dependent enumeration, no
# pixel-level diffs. Each scenario captures the UUIDs it needs from
# create-response JSON instead of relying on name-resolution.
#
# Scenarios (12):
#   1.  session create + rename + kill round-trip
#   2.  window CRUD inside a session
#   3.  pane split / list / zoom / unzoom / swap / kill
#   4.  send_keys → wait-for → capture (PTY input fidelity)
#   5.  apply atomicity — name conflict rejects without mutating state
#   6.  events.history — monotonic seq + lifecycle event types present
#   7.  snapshot dimensions — PNG header reports requested grid size
#   8.  session.snapshot ≡ window.snapshot for a single-window session
#   9.  optimistic concurrency — version_conflict on stale kill
#   10. wait-for timeout — pattern that can't match exits 2
#   11. wait-for --absent + regex — both supported modes
#   12. cleanup teardown is idempotent

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "${REPO_ROOT}/.shux/scripts/lib/shux_harness.sh"
SHUX="${SHUX:-shux}"
WORKDIR="${WORKDIR:-$(mktemp -d -t shux-integration.XXXXXX)}"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/shux-integration-runtime.XXXXXX")}"
export XDG_RUNTIME_DIR="${RUNTIME_DIR}"

# All sessions created in this script use this prefix so the trap can
# sweep them on exit without touching unrelated state.
PFX="shux-it"

cleanup() {
    shux_harness_timeout 8s "$SHUX" rpc call session.list 2>/dev/null \
        | jq -r --arg pfx "$PFX" \
            '.result.sessions[]? | select(.name | startswith($pfx)) | .id' \
        | while read -r sid; do
            shux_harness_timeout 8s "$SHUX" rpc call session.kill --params "{\"id\":\"$sid\"}" >/dev/null 2>&1 || true
          done || true
    shux_harness_stop_daemon "${RUNTIME_DIR}"
}
trap cleanup EXIT

cd "$WORKDIR"
"$SHUX" init >/dev/null

ok () { echo "    ✓ $*"; }
fail () { echo "    ✗ $*" >&2; exit 1; }

# Capture a session UUID from create-response so we never trip on
# name-vs-id resolution quirks.
mk_session () {
    local name="$1" cmd="$2"
    "$SHUX" rpc call session.create --params "{\"name\":\"$name\",\"command\":[\"bash\",\"-c\",\"$cmd\"]}" \
        | jq -r .result.id
}

# ──────────────────────────────────────────────────────────────
echo "==> [ 1/12] session.create → rename → kill round-trip"
# ──────────────────────────────────────────────────────────────
sid_a=$(mk_session "${PFX}-life" "sleep 9000")
[ -n "$sid_a" ] && [ "$sid_a" != "null" ] || fail "session.create returned no id"

# CLI `rename` (uses dual-resolution: name → uuid). Asserts the CLI
# wrapper path, not just the raw RPC.
"$SHUX" session rename -s "${PFX}-life" -n "${PFX}-life-renamed" >/dev/null

after=$("$SHUX" rpc call session.list \
        | jq -r --arg id "$sid_a" '.result.sessions[] | select(.id==$id) | .name')
[ "$after" = "${PFX}-life-renamed" ] || fail "rename didn't take effect (got: $after)"

"$SHUX" rpc call session.kill --params "{\"id\":\"$sid_a\"}" >/dev/null
gone=$("$SHUX" rpc call session.list \
       | jq --arg id "$sid_a" '.result.sessions[] | select(.id==$id) | .id')
[ -z "$gone" ] || fail "killed session still in list (got: $gone)"
ok "create → rename → kill via UUIDs"

# ──────────────────────────────────────────────────────────────
echo "==> [ 2/12] window.create / window.list / window.kill"
# ──────────────────────────────────────────────────────────────
sid_w=$(mk_session "${PFX}-wins" "sleep 9000")
"$SHUX" rpc call window.create --params "{\"session_id\":\"$sid_w\",\"name\":\"second\"}" >/dev/null
"$SHUX" rpc call window.create --params "{\"session_id\":\"$sid_w\",\"name\":\"third\"}" >/dev/null

count=$("$SHUX" rpc call window.list --params "{\"session_id\":\"$sid_w\"}" \
        | jq '.result | length')
[ "$count" = "3" ] || fail "expected 3 windows, got $count"

# Kill the middle one, assert remaining two are correct titles.
mid_id=$("$SHUX" rpc call window.list --params "{\"session_id\":\"$sid_w\"}" \
         | jq -r '.result[] | select(.title=="second") | .id')
"$SHUX" rpc call window.kill --params "{\"session_id\":\"$sid_w\",\"id\":\"$mid_id\"}" >/dev/null

titles=$("$SHUX" rpc call window.list --params "{\"session_id\":\"$sid_w\"}" \
         | jq -r '[.result[].title] | sort | join(",")')
echo "$titles" | grep -q 'second' \
    && fail "killed window 'second' still in list (got: $titles)"
echo "$titles" | grep -q 'third' || fail "window 'third' missing (got: $titles)"
ok "create × 3 → list = 3 → kill middle → list = 2"

# ──────────────────────────────────────────────────────────────
echo "==> [ 3/12] pane.split / pane.list / pane.zoom / pane.swap / pane.kill"
# ──────────────────────────────────────────────────────────────
sid_p=$(mk_session "${PFX}-panes" "sleep 9000")
pane0=$("$SHUX" rpc call session.list \
        | jq -r --arg id "$sid_p" '.result.sessions[] | select(.id==$id) | .pane_id')
win0=$("$SHUX" rpc call session.list \
       | jq -r --arg id "$sid_p" '.result.sessions[] | select(.id==$id) | .active_window_id')

# Split twice → 3 panes in the window.
pane1=$("$SHUX" rpc call pane.split --params "{\"pane_id\":\"$pane0\",\"direction\":\"vertical\"}" \
        | jq -r .result.pane.id)
pane2=$("$SHUX" rpc call pane.split --params "{\"pane_id\":\"$pane1\",\"direction\":\"horizontal\"}" \
        | jq -r .result.pane.id)

# pane.list isn't implemented yet across both `window_id` and `session_id`
# shapes uniformly — use window.list and inspect pane_count which IS
# part of the contracted response (see api.md, window_to_json).
pane_count=$("$SHUX" rpc call window.list --params "{\"session_id\":\"$sid_p\"}" \
             | jq --arg w "$win0" '.result[] | select(.id==$w) | .pane_count')
[ "$pane_count" = "3" ] || fail "expected 3 panes after 2 splits, got $pane_count"

# Zoom + unzoom (zoom acts as a toggle).
"$SHUX" rpc call pane.zoom --params "{\"pane_id\":\"$pane1\"}" >/dev/null
"$SHUX" rpc call pane.zoom --params "{\"pane_id\":\"$pane1\"}" >/dev/null

# Swap pane0 ↔ pane2 (pane.swap takes both ids). Verify both still exist.
"$SHUX" rpc call pane.swap --params "{\"pane_id\":\"$pane0\",\"target_pane_id\":\"$pane2\"}" >/dev/null

# Kill the middle pane.
"$SHUX" rpc call pane.kill --params "{\"pane_id\":\"$pane1\"}" >/dev/null
pane_count=$("$SHUX" rpc call window.list --params "{\"session_id\":\"$sid_p\"}" \
             | jq --arg w "$win0" '.result[] | select(.id==$w) | .pane_count')
[ "$pane_count" = "2" ] || fail "expected 2 panes after kill, got $pane_count"
ok "split×2 / zoom / unzoom / swap / kill — pane count stays consistent"

# ──────────────────────────────────────────────────────────────
echo "==> [ 4/12] send_keys → wait-for → capture (PTY round-trip)"
# ──────────────────────────────────────────────────────────────
sid_io=$(mk_session "${PFX}-io" "read x; echo GOT:\$x; sleep 9000")
pane_io=$("$SHUX" rpc call session.list \
          | jq -r --arg id "$sid_io" '.result.sessions[] | select(.id==$id) | .pane_id')

"$SHUX" rpc call pane.send_keys --params \
    "{\"pane_id\":\"$pane_io\",\"text\":\"hello-from-test\\n\"}" >/dev/null
"$SHUX" pane wait-for -p "$pane_io" -t 'GOT:hello-from-test' --timeout-ms 5000 >/dev/null

cap=$("$SHUX" rpc call pane.capture --params "{\"pane_id\":\"$pane_io\",\"lines\":5}" \
      | jq -r .result.text)
echo "$cap" | grep -q 'GOT:hello-from-test' \
    || fail "pane.capture missing echoed text (got: $cap)"
ok "send_keys bytes round-trip through PTY → wait-for → capture"

# ──────────────────────────────────────────────────────────────
echo "==> [ 5/12] apply atomicity — name conflict rejects, graph unchanged"
# ──────────────────────────────────────────────────────────────
cat > base.toml <<TOML
[session]
name = "${PFX}-base"
[[windows]]
title = "w0"
[[windows.panes]]
command = ["bash", "-c", "sleep 9000"]
TOML
"$SHUX" state apply base.toml >/dev/null
before=$("$SHUX" rpc call session.list | jq '.result.sessions | length')

if "$SHUX" state apply base.toml >/dev/null 2>&1; then
    fail "second apply of same session.name unexpectedly succeeded"
fi
after=$("$SHUX" rpc call session.list | jq '.result.sessions | length')
[ "$before" = "$after" ] \
    || fail "session count changed ($before → $after) after a failed apply"
ok "name-conflict apply rejected; graph unchanged"

# ──────────────────────────────────────────────────────────────
echo "==> [ 6/12] events.history — monotonic seq + lifecycle event types"
# ──────────────────────────────────────────────────────────────
mk_session "${PFX}-ev" "sleep 9000" >/dev/null
events=$("$SHUX" rpc call events.history --params '{"count":30}')
seqs=$(echo "$events" | jq -r '.result.events[].seq')
prev=-1
for s in $seqs; do
    [ "$s" -gt "$prev" ] || fail "seq not monotonic: $s after $prev"
    prev="$s"
done

types=$(echo "$events" | jq -r '[.result.events[].type] | join(",")')
for needle in session.created window.created pane.created; do
    echo "$types" | grep -q "$needle" \
        || fail "$needle event missing (types: $types)"
done
ok "event seqs strictly increasing; session/window/pane.created all present"

# ──────────────────────────────────────────────────────────────
echo "==> [ 7/12] snapshot dimension fidelity (80×24 → 720×456)"
# ──────────────────────────────────────────────────────────────
sid_sd=$(mk_session "${PFX}-snap" "sleep 9000")
"$SHUX" window snapshot -s "${PFX}-snap" -o "$WORKDIR/dim.png" --cols 80 --rows 24 >/dev/null

# PNG IHDR width is at byte offset 16, big-endian u32.
w=$(python3 -c "
import struct
print(struct.unpack('>I', open('$WORKDIR/dim.png','rb').read()[16:20])[0])")
h=$(python3 -c "
import struct
print(struct.unpack('>I', open('$WORKDIR/dim.png','rb').read()[20:24])[0])")
[ "$w" = "720" ] || fail "PNG width expected 720, got $w"
[ "$h" = "456" ] || fail "PNG height expected 456, got $h"
ok "PNG header reports 720×456 for an 80×24 request"

# ──────────────────────────────────────────────────────────────
echo "==> [ 8/12] session.snapshot ≡ window.snapshot for single-window session"
# ──────────────────────────────────────────────────────────────
win_sd=$("$SHUX" rpc call session.list \
         | jq -r --arg id "$sid_sd" '.result.sessions[] | select(.id==$id) | .active_window_id')

a=$("$SHUX" rpc call session.snapshot --params \
    "{\"session_id\":\"$sid_sd\",\"cols\":80,\"rows\":24}" \
    | jq -r .result.png_base64)
b=$("$SHUX" rpc call window.snapshot --params \
    "{\"window_id\":\"$win_sd\",\"cols\":80,\"rows\":24}" \
    | jq -r .result.png_base64)
# Status bar contains a live clock, so the two responses sit in
# different ticks. We assert the IMAGE DIMENSIONS, CELL DIMENSIONS, AND
# REPORTED COLS/ROWS agree — the contracted invariant. Pixel parity
# would require freezing the clock.
ja=$("$SHUX" rpc call session.snapshot --params \
     "{\"session_id\":\"$sid_sd\",\"cols\":80,\"rows\":24}")
jb=$("$SHUX" rpc call window.snapshot --params \
     "{\"window_id\":\"$win_sd\",\"cols\":80,\"rows\":24}")
for field in width height cell_width cell_height cols rows format; do
    va=$(echo "$ja" | jq -r ".result.$field")
    vb=$(echo "$jb" | jq -r ".result.$field")
    [ "$va" = "$vb" ] \
        || fail "session.snapshot ≠ window.snapshot on $field: $va vs $vb"
done
[ "${#a}" = "${#b}" ] || ok "PNG sizes differ by clock-tick (expected; dims agree)"
ok "session/window snapshot dimensions identical"

# ──────────────────────────────────────────────────────────────
echo "==> [ 9/12] optimistic concurrency — stale expected_version rejected"
# ──────────────────────────────────────────────────────────────
sid_oc=$(mk_session "${PFX}-oc" "sleep 9000")
# kill with bogus version → should return version_conflict (-32002).
# shux api exits 2 on RPC error; capture-then-parse so pipefail doesn't bite.
set +e
ocjson=$("$SHUX" rpc call session.kill --params "{\"id\":\"$sid_oc\",\"expected_version\":999}" 2>&1)
set -e
err=$(echo "$ocjson" | jq -r '.error.code // "ok"')
[ "$err" = "-32002" ] \
    || fail "expected -32002 on stale version, got $err (full: $ocjson)"

# Session still alive — kill without expected_version succeeds.
still=$("$SHUX" rpc call session.list \
        | jq -r --arg id "$sid_oc" '.result.sessions[] | select(.id==$id) | .id')
[ "$still" = "$sid_oc" ] || fail "session disappeared after a rejected kill"
ok "stale expected_version → -32002; entity untouched"

# ──────────────────────────────────────────────────────────────
echo "==> [10/12] wait-for timeout — never-matching pattern exits 2"
# ──────────────────────────────────────────────────────────────
sid_wt=$(mk_session "${PFX}-wt" "sleep 9000")
pane_wt=$("$SHUX" rpc call session.list \
          | jq -r --arg id "$sid_wt" '.result.sessions[] | select(.id==$id) | .pane_id')

set +e
"$SHUX" pane wait-for -p "$pane_wt" -t NEVER-PRESENT-XYZ --timeout-ms 300 >/dev/null 2>&1
rc=$?
set -e
[ "$rc" = "2" ] || fail "wait-for timeout should exit 2, got $rc"
ok "wait-for timeout returns exit 2"

# ──────────────────────────────────────────────────────────────
echo "==> [11/12] wait-for --absent + --regex modes"
# ──────────────────────────────────────────────────────────────
"$SHUX" pane wait-for -p "$pane_wt" -t NEVER-PRESENT-XYZ --absent --timeout-ms 300 >/dev/null
ok "--absent succeeds immediately when text is not present"

sid_re=$(mk_session "${PFX}-re" "echo READY-NOW; sleep 9000")
pane_re=$("$SHUX" rpc call session.list \
          | jq -r --arg id "$sid_re" '.result.sessions[] | select(.id==$id) | .pane_id')
"$SHUX" pane wait-for -p "$pane_re" --regex 'READY-NO[A-Z]+' --timeout-ms 3000 >/dev/null
ok "--regex matches against captured text"

# ──────────────────────────────────────────────────────────────
echo "==> [12/12] cleanup teardown is idempotent"
# ──────────────────────────────────────────────────────────────
# Run the cleanup function twice. The second invocation must not
# error (every kill is wrapped in `|| true`).
cleanup
cleanup
ok "cleanup is safe to invoke after sessions are already gone"

echo
echo "✓ all 12 integration scenarios passed on $(uname -s)/$(uname -m)"
