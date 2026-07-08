#!/bin/sh
# f10_altscreen.sh — lens fixture F10 (§11 TEST-1).
#
# Token-paced (NO sleeps). Alternate-screen semantics:
#   E : enter alternate screen (CSI ?1049h) and draw a magenta frame.
#   L : leave the alternate screen (CSI ?1049l) — the normal screen is restored.
#   X : draw a marker on whichever screen is currently active.
# Used by: A1 (alt-screen glance/diff invalidation).
#
# Coordinates 0-based grid; ANSI = (row+1,col+1). Both screens carry
# truecolor/256/basic colour (house rule).

printf '\033[2J\033[3J\033[H'
# Echo OFF is load-bearing: echoed token newlines would scroll whichever
# screen is active and break A1's normal-screen restore golden.
stty -echo 2>/dev/null || :

# --- Normal screen (wait_for sentinel: LENS-F10-ALT) ----------------------
printf '\033[1;1H\033[1;38;2;250;250;250mLENS-F10-ALT\033[0m'
printf '\033[3;3H\033[38;2;120;220;160mNORMAL-SCREEN\033[0m'
printf '\033[5;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[6;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'
printf '\033[24;80H'

draw_alt() {
	printf '\033[2J\033[H'
	printf '\033[2;2H\033[1;38;2;220;60;220mALT-SCREEN\033[0m'
	# Magenta frame corners (truecolor / 256 / basic magenta).
	printf '\033[4;2H\033[48;2;120;20;120m   \033[0m'
	printf '\033[4;6H\033[48;5;90m   \033[0m'
	printf '\033[4;10H\033[45m   \033[0m'
	printf '\033[24;80H'
}

active='normal'
while read -r tok; do
	case "$tok" in
	E)
		printf '\033[?1049h'
		draw_alt
		active='alt'
		;;
	L)
		printf '\033[?1049l'
		active='normal'
		;;
	X)
		if [ "$active" = 'alt' ]; then
			printf '\033[8;2H\033[38;2;240;240;60mX-MARK-ALT\033[0m'
		else
			printf '\033[8;3H\033[38;2;240;240;60mX-MARK-NORMAL\033[0m'
		fi
		printf '\033[24;80H'
		;;
	esac
done
