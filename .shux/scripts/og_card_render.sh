#!/usr/bin/env bash
#
# og_card_render.sh — headless-screenshot pages/og-card.html into
# pages/og-card.png at 1200×630 (the OG / Twitter summary_large_image
# canonical aspect). Run after editing the HTML / CSS so the social
# preview tracks the source.
#
# Requires: a working local HTTP server bound to 127.0.0.1:8765
# serving the pages/ tree (so the og-card.html can resolve its
# /assets/logo.svg + /screenshots/multi-agent.png references). The
# script starts one in the background if nothing's listening.

set -euo pipefail

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
[ -x "$CHROME" ] || { echo "no Chrome at $CHROME"; exit 1; }

PORT="${OG_CARD_PORT:-8765}"
URL="http://localhost:${PORT}/og-card.html"
OUT="pages/og-card.png"

cleanup_server=0
if ! curl -s -o /dev/null -w "%{http_code}" "$URL" 2>/dev/null | grep -q '^200$'; then
    echo "starting local server on :$PORT"
    ( cd pages && python3 -m http.server "$PORT" --bind 127.0.0.1 \
        >/tmp/og-card-render.log 2>&1 ) &
    cleanup_server=$!
    # Wait until the server starts answering.
    for _ in {1..40}; do
        if curl -s -o /dev/null -w "%{http_code}" "$URL" 2>/dev/null | grep -q '^200$'; then
            break
        fi
        sleep 0.1
    done
fi

# Render at 2× device pixel ratio so the PNG ships sharp on retina
# clients, then stays crisp when Slack / Twitter / iMessage scale it
# down. Hide the scrollbar via a media-query trick in the HTML.
"$CHROME" \
    --headless=new \
    --hide-scrollbars \
    --disable-gpu \
    --window-size=1200,630 \
    --force-device-scale-factor=2 \
    --screenshot="$OUT" \
    "$URL" >/dev/null 2>&1

if [ "$cleanup_server" -ne 0 ]; then
    kill "$cleanup_server" 2>/dev/null || true
fi

echo "→ $OUT"
file "$OUT"
ls -lh "$OUT" | awk '{print "   size:", $5}'
