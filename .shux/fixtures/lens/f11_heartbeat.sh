#!/bin/sh
# f11_heartbeat.sh — lens fixture F11 (task 083 settle hardening).
#
# Token-paced (one stdin line == one repaint; NO sleeps). Each token REDRAWS THE
# SAME row 10 with byte-identical content — a pure IDENTICAL repaint. This is the
# fast-identical-repainter case: content_revision bumps on every token (§4.2 is
# value-independent — an identical repaint still bumps), but the PRESENTED FRAME
# HASH is constant. Quiet-mode `wait-settled` times out (never quiet while tokens
# flow); `--stable-frames`/`--hold-ms` settle because the frame content holds.
# Contrast with f8_repaint (changes the glyph each token) and f3_flip (alternates
# two frames) — both genuine animation → `settle_never_stable`.
#
# Coordinates 0-based grid; ANSI = (row+1,col+1). Colours: truecolor heartbeat +
# a static 256/basic legend (house colour rule) so a monochrome regression fails.

printf '\033[2J\033[3J\033[H'
# Echo OFF is load-bearing for every token-paced fixture: the PTY line discipline
# would otherwise echo each token newline at the cursor, scrolling the frame.
stty -echo 2>/dev/null || :

# Title / wait_for sentinel (drawn once).
printf '\033[1;1H\033[1;38;2;250;250;250mLENS-F11-HEARTBEAT\033[0m'
# Static colour legend (256 + basic).
printf '\033[21;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[22;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'

# One repaint of the IDENTICAL heartbeat line (erase + redraw, byte-identical).
beat() {
	printf '\033[11;1H\033[2K\033[38;2;80;220;120mHEARTBEAT\033[0m'
	printf '\033[24;80H'
}

# Initial frame so the pane is non-blank before the first token.
beat

while read -r _; do
	beat
done
