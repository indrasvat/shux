#!/bin/sh
# spinner.sh — gate fixture: self-animating GENUINE animation (task 083 settle_never_stable).
#
# Cycles a spinner glyph at row 10 every ~25ms for a bounded number of frames. Every frame
# differs, so the presented FRAME HASH never repeats: `--stable-frames K` never accumulates K
# contiguous identical frames and `--hold-ms N` never holds N ms without a change → within the
# settle budget this is `settle_never_stable` (a FAILURE, never infra — the frozen 078/082
# contract). Self-animating + bounded (see repaint.sh header); killed on scratch teardown.

printf '\033[2J\033[3J\033[H'
stty -echo 2>/dev/null || :

printf '\033[1;1H\033[1;38;2;250;250;250mGATE-SPINNER\033[0m'
printf '\033[21;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[22;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'

i=0
while [ "$i" -lt 240 ]; do
	case "$((i % 4))" in
	0) g='⠋' ;;
	1) g='⠙' ;;
	2) g='⠹' ;;
	3) g='⠸' ;;
	esac
	printf '\033[11;1H\033[2K\033[38;2;255;191;0mSPIN %s\033[0m\033[24;80H' "$g"
	sleep 0.025
	i=$((i + 1))
done
