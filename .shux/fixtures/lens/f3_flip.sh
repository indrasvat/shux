#!/bin/sh
# f3_flip.sh — lens fixture F3 (§11 TEST-1).
#
# Token-paced (one stdin line == one flip; NO sleeps). Alternates two FULL
# 80x24 frames:
#   Frame A: every cell 'A', red background, white foreground.
#   Frame B: every cell 'B', blue background, yellow foreground.
# Column 79 (0-based, the last column) of every row carries a per-row checksum
# digit that differs between the frames (A: r%10, B: (r+5)%10), so the text
# alone identifies the frame. The pane is NEVER still while tokens flow.
#
# Each frame spreads its background across truecolor + 256-color + basic ANSI
# (by row mod 3) so the red/blue identity survives a monochrome regression and
# the house colour rule holds. Used by: G1, S3.

printf '\033[2J\033[3J\033[H'
# Echo OFF is load-bearing: an echoed token newline at the parked cursor
# (bottom row) would scroll the frame mid-flip and fail G1's clean-frame check.
stty -echo 2>/dev/null || :

# Build the 79-char fill strings once (cols 0..78; col 79 is the checksum).
A79=''
B79=''
i=0
while [ "$i" -lt 79 ]; do
	A79="${A79}A"
	B79="${B79}B"
	i=$((i + 1))
done

# $1 = 'A' or 'B'
draw_frame() {
	r=0
	while [ "$r" -lt 24 ]; do
		if [ "$1" = 'A' ]; then
			fg='38;2;250;250;250'
			case "$((r % 3))" in
			0) bg='48;2;190;30;40' ;;
			1) bg='48;5;196' ;;
			2) bg='41' ;;
			esac
			fill="$A79"
			ck=$((r % 10))
		else
			fg='38;2;240;220;40'
			case "$((r % 3))" in
			0) bg='48;2;30;60;200' ;;
			1) bg='48;5;21' ;;
			2) bg='44' ;;
			esac
			fill="$B79"
			ck=$(((r + 5) % 10))
		fi
		printf '\033[%d;1H\033[%s;%sm%s%d\033[0m' "$((r + 1))" "$fg" "$bg" "$fill" "$ck"
		r=$((r + 1))
	done
	# Park the cursor out of the way so the frame reads clean.
	printf '\033[24;80H'
}

# Initial frame so the pane is non-blank before the first token.
draw_frame A
frame='A'

while read -r _; do
	if [ "$frame" = 'A' ]; then
		draw_frame B
		frame='B'
	else
		draw_frame A
		frame='A'
	fi
done
