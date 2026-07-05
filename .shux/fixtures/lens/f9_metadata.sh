#!/bin/sh
# f9_metadata.sh — lens fixture F9 (§11 TEST-1).
#
# Token-paced (NO sleeps). Every non-`V` token emits ONLY Class-B noise that
# must NOT bump ContentRevision or reset a settle window:
#   OSC title change  +  BEL  +  DECSCUSR cursor-shape cycle.
# The single `V` token draws ONE visible green ▮ at grid (4,4) — a real
# Class-A cell change. Used by: S5 (Class-B immunity).
#
# Coordinates 0-based grid; ANSI = (row+1,col+1). Colours: truecolor/256/basic
# legend drawn once at startup (house rule).

printf '\033[2J\033[3J\033[H'

printf '\033[1;1H\033[1;38;2;250;250;250mLENS-F9-META\033[0m'
printf '\033[21;3H'
c=0
while [ "$c" -lt 72 ]; do printf '\033[48;2;%d;90;%dm ' "$((40 + c * 2))" "$((220 - c))"; c=$((c + 1)); done
printf '\033[0m'
printf '\033[22;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[23;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'
printf '\033[24;80H'

count=1
while read -r tok; do
	case "$tok" in
	V)
		printf '\033[5;5H\033[38;2;60;220;90m▮\033[0m'
		printf '\033[24;80H'
		;;
	*)
		# Class-B only: OSC title, BEL, DECSCUSR shape cycle. No cell change.
		printf '\033]0;lens-f9-%d\007' "$count"
		printf '\007'
		printf '\033[%d q' "$(((count % 6) + 1))"
		count=$((count + 1))
		;;
	esac
done
