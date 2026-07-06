#!/bin/sh
# f2_spinner.sh — lens fixture F2 (§11 TEST-1).
#
# Token-paced (one stdin line == one frame advance; NO sleeps). Each token
# redraws a braille spinner at grid (1,1) in truecolor amber. The `R` token
# prints READY (green bold) at grid (12,35), parks the cursor, then stays
# still forever (subsequent reads block → pane goes quiet → settles).
#
# Coordinates are 0-based grid; ANSI = (row+1,col+1). Colours: amber spinner
# (truecolor) + a 256-color strip + basic-color blocks (house rule).
# Used by: S1, S2.

printf '\033[2J\033[3J\033[H'
# Echo OFF is load-bearing for every token-paced fixture: the PTY line
# discipline would otherwise echo each token newline at the cursor, scrolling
# and corrupting the frame (breaks S1/S2 golden determinism).
stty -echo 2>/dev/null || :

# Static wait_for sentinel + colour legend (drawn once).
printf '\033[1;1H\033[1;38;2;250;250;250mLENS-F2-SPIN\033[0m'
printf '\033[21;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[22;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'

spin_frame() {
	# $1 = frame index 0..7 → braille glyph, amber truecolor, at ANSI (2,2).
	printf '\033[2;2H\033[38;2;255;191;0m'
	case "$1" in
	0) printf '⠋' ;;
	1) printf '⠙' ;;
	2) printf '⠹' ;;
	3) printf '⠸' ;;
	4) printf '⠼' ;;
	5) printf '⠴' ;;
	6) printf '⠦' ;;
	7) printf '⠧' ;;
	esac
	printf '\033[0m'
}

# Initial spinner frame so the pane is non-blank before the first token.
spin_frame 0

i=1
while read -r tok; do
	case "$tok" in
	R)
		printf '\033[13;36H\033[1;38;2;60;220;90mREADY\033[0m'
		printf '\033[24;1H\033[?25l'
		break
		;;
	*)
		spin_frame "$((i % 8))"
		i=$((i + 1))
		;;
	esac
done

# Post-READY: drain any further stdin SILENTLY and stay still forever
# (p0-council-r1 minor 12). SIGWINCH-safe loop, zero output.
while :; do read -r _ || :; done
