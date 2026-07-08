#!/bin/sh
# f8_repaint.sh — lens fixture F8 (§11 TEST-1).
#
# Token-paced (one stdin line == one repaint; NO sleeps). Each token rewrites
# row 10 (0-based) with the SAME layout but the next glyph from 0-9A-Z as
# `FRAME:<glyph>` in truecolor — a PURE content repaint with zero structural
# change (no split/rename/resize). This is the graph-version trap fixture:
# content_revision must bump per repaint while the SessionGraph structural
# version stays put. Used by: G3, G4.
#
# Coordinates 0-based grid; ANSI = (row+1,col+1). Colours: truecolor repaint +
# a static 256/basic legend (house rule).

printf '\033[2J\033[3J\033[H'
# Echo OFF is load-bearing: echoed token newlines would scroll the frame and
# add unintended Class-A mutations beyond the pure repaint under test.
stty -echo 2>/dev/null || :

# Title / wait_for sentinel (drawn once).
printf '\033[1;1H\033[1;38;2;250;250;250mLENS-F8-REPAINT\033[0m'
# Static colour legend (256 + basic).
printf '\033[21;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[22;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'

GLYPHS='0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ'

nth_glyph() {
	# Echo the 0-based $1-th character of GLYPHS using only builtins.
	s="$GLYPHS"
	j=0
	while [ "$j" -lt "$1" ]; do
		s=${s#?}
		j=$((j + 1))
	done
	printf '%s' "${s%"${s#?}"}"
}

count=0
while read -r _; do
	g=$(nth_glyph "$((count % 36))")
	# Erase row 10 then repaint it (truecolor). Pure content change.
	printf '\033[11;1H\033[2K\033[38;2;80;220;120mFRAME:%s\033[0m' "$g"
	printf '\033[24;80H'
	count=$((count + 1))
done
