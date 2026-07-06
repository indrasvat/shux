#!/bin/sh
# f4_keys.sh — lens fixture F4 (§11 TEST-1).
#
# Raw single-byte input (stty raw -echo; echo OFF is load-bearing — an echoed
# keystroke would add cells and break D2's exact count). Reads one byte per
# loop with `dd bs=1 count=1`.
#
#   'a'  : draw red block █ at grid (2,2) AND green-bold "A-PRESSED" at grid
#          (5,10)..(5,18) — EXACTLY 10 cells change (1 + 9).
#   's'  : recolor those SAME 10 cells (new fg/bg, identical glyphs). Pressing
#          's' BEFORE any 'a' is a documented NO-OP (the cells must exist to
#          recolor — D4 always sends 'a' first).
#   Tab  : move focus marker ▶ among grid cells (8,5)/(8,25)/(8,45), clearing
#          the old cell and drawing the new one — EXACTLY 2 cells change.
# The cursor is parked at grid (23,0) after every redraw.
#
# Coordinates are 0-based grid; ANSI = (row+1,col+1). Used by: D2, D4, K1, E1.

printf '\033[2J\033[3J\033[H'
stty raw -echo

# Static wait_for sentinel + colour legend (truecolor/256/basic; house rule).
printf '\033[1;1H\033[1;38;2;250;250;250mLENS-F4-KEYS\033[0m'
printf '\033[21;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[22;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'
printf '\033[23;3H\033[38;2;120;220;160mtruecolor-legend\033[0m'

park() { printf '\033[24;1H'; }

# Focus marker at ANSI positions for marker indices 0/1/2.
marker_ansi() {
	case "$1" in
	0) printf '9;6H' ;;
	1) printf '9;26H' ;;
	2) printf '9;46H' ;;
	esac
}

# Initial marker at grid (8,5) (index 0).
printf '\033[9;6H\033[38;2;80;220;220m▶\033[0m'
park

pressed_a=0
marker_idx=0
tab=$(printf '\t')

while :; do
	c=$(dd bs=1 count=1 2>/dev/null) || :
	# EOF: dd succeeds with EMPTY output — break instead of busy-spinning
	# (p0-council-r2 major 1). Tests only ever send a/s/Tab to F4; NUL and
	# bare newline (the two byte values that also read back empty through
	# command substitution) are never sent.
	[ -n "$c" ] || break
	if [ "$c" = a ]; then
		printf '\033[3;3H\033[38;2;220;40;40m█\033[0m'
		printf '\033[6;11H\033[1;38;2;40;200;80mA-PRESSED\033[0m'
		pressed_a=1
		park
	elif [ "$c" = s ]; then
		if [ "$pressed_a" -eq 1 ]; then
			printf '\033[3;3H\033[38;2;40;210;210m█\033[0m'
			printf '\033[6;11H\033[1;38;2;240;220;60;48;2;30;40;120mA-PRESSED\033[0m'
			park
		fi
	elif [ "$c" = "$tab" ]; then
		old=$(marker_ansi "$marker_idx")
		marker_idx=$(((marker_idx + 1) % 3))
		new=$(marker_ansi "$marker_idx")
		# Clear the old marker cell (default space), then draw the new marker:
		# exactly 2 cells change.
		printf '\033[%s\033[0m \033[%s\033[38;2;80;220;220m▶\033[0m' "$old" "$new"
		park
	fi
done
