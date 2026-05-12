#!/usr/bin/env bash
# shux end-to-end smoke test. Run from the repo root with a built binary
# on PATH (or `SHUX=target/release/shux .shux/scripts/smoke.sh`).
#
# Exercises every feature an agent would touch in real use:
#   1. version
#   2. shux init scaffolding
#   3. daemon auto-start via system.version RPC
#   4. shux state apply (atomic graph mutation + PTY spawn)
#   5. pane wait-for (text, regex, --absent)
#   6. window snapshot (composed PNG with borders + status bar)
#   7. session kill cleanup
#
# Used by CI on ubuntu-latest + macos-latest to catch platform regressions
# the cargo test suite (in-process routers) doesn't cover.

set -euo pipefail

SHUX="${SHUX:-shux}"
WORKDIR="${WORKDIR:-$(mktemp -d -t shux-smoke.XXXXXX)}"
trap '"$SHUX" session kill shux-smoke-wait 2>/dev/null || true;
      "$SHUX" session kill shux-smoke-snap 2>/dev/null || true' EXIT

echo "==> using shux: $($SHUX --version)"
echo "==> workdir: $WORKDIR"
cd "$WORKDIR"

echo
echo "==> [1/7] shux init"
"$SHUX" init

echo
echo "==> [2/7] daemon auto-start via system.version"
"$SHUX" rpc call system.version | grep -q '"name"' || { echo "system.version failed"; exit 1; }

echo
echo "==> [3/7] state apply 1-pane spec — target for wait-for"
cat > .shux/templates/wait.toml <<'TOML'
[session]
name = "shux-smoke-wait"
[[windows]]
title = "wait"
[[windows.panes]]
command = ["bash", "-c", "echo BOOTING; sleep 0.8; echo READY-MARKER; sleep 9000"]
TOML
"$SHUX" state apply .shux/templates/wait.toml | grep -E 'Applied|spawned'

echo
echo "==> [4/7] pane wait-for: text match (asynchronous output)"
"$SHUX" pane wait-for -s shux-smoke-wait -t READY-MARKER --timeout-ms 5000

echo "==> [4b] pane wait-for: regex match"
"$SHUX" pane wait-for -s shux-smoke-wait --regex 'READY-MARK[A-Z]+' --timeout-ms 500

echo "==> [4c] pane wait-for: --absent (text never appears)"
"$SHUX" pane wait-for -s shux-smoke-wait -t NEVER-PRESENT --absent --timeout-ms 500

"$SHUX" session kill shux-smoke-wait > /dev/null

echo
echo "==> [5/7] state apply 2-pane split spec — target for snapshot"
cat > .shux/templates/snap.toml <<'TOML'
[session]
name = "shux-smoke-snap"
[[windows]]
title = "demo"
[[windows.panes]]
command = ["bash", "-c", "echo HELLO LEFT; sleep 9000"]
[[windows.panes]]
command = ["bash", "-c", "echo HELLO RIGHT; sleep 9000"]
split = "horizontal"
TOML
"$SHUX" state apply .shux/templates/snap.toml | grep -E 'Applied|spawned'
sleep 0.4   # let bash actually print before we capture

echo
echo "==> [6/7] window snapshot (composed PNG, includes status bar)"
"$SHUX" window snapshot -s shux-smoke-snap -o .shux/out/snap.png --cols 100 --rows 24
[ -s .shux/out/snap.png ] || { echo "snapshot file empty"; exit 1; }

# Verify the PNG is at least a valid signature; the cargo tests cover
# pixel-level structure.
head -c 8 .shux/out/snap.png | od -A n -t x1 | tr -d ' \n' | grep -q '89504e470d0a1a0a' \
    || { echo "snapshot is not a valid PNG"; exit 1; }

echo
echo "==> [7/7] cleanup"
"$SHUX" session kill shux-smoke-snap > /dev/null

echo
echo "✓ shux end-to-end smoke passed on $(uname -s)/$(uname -m)"
