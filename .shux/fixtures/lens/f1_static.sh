#!/bin/sh
# f1_static.sh — lens fixture F1 (§11 TEST-1).
#
# Draws ONE deterministic 80x24 frame, parks a HIDDEN cursor at grid (23,79),
# then blocks forever. Pure static content: no token pacing, no sleeps.
#
# Coordinate convention (all lens fixtures): the PRD's (row,col) are 0-based
# grid coordinates; ANSI cursor addressing is 1-based, so ANSI = (row+1,col+1).
#
# House rule: every fixture carries truecolor AND 256-color AND basic-color
# content so a monochrome / NO_COLOR regression cannot pass unnoticed.
#   row 2: truecolor gradient bar   row 3: 256-color strip   row 4: 16-color blocks
#   row 6: Devanagari   row 7: CJK   row 8: emoji
# Used by: G2, S4, D1, R2, R4.

# Clear screen + scrollback + home so no shell echo / prompt survives.
printf '\033[2J\033[3J\033[H'

# --- Outer border: rounded corners, light top/bottom, heavy sides ---------
# top (rounded left/right + light horizontal)
printf '\033[1;1H\033[38;2;120;200;255m╭'
i=2
while [ "$i" -le 79 ]; do printf '─'; i=$((i + 1)); done
printf '╮\033[0m'
# bottom
printf '\033[24;1H\033[38;2;120;200;255m╰'
i=2
while [ "$i" -le 79 ]; do printf '─'; i=$((i + 1)); done
printf '╯\033[0m'
# heavy sides
r=2
while [ "$r" -le 23 ]; do
	printf '\033[%d;1H\033[38;2;120;200;255m┃\033[%d;80H┃\033[0m' "$r" "$r"
	r=$((r + 1))
done

# Title (row 0, inside the border) — stable wait_for sentinel is दृश्यते (row 6).
printf '\033[1;30H\033[1;38;2;250;250;250mLENS-F1-STATIC\033[0m'

# --- row 2 (ANSI 3): truecolor gradient bar -------------------------------
printf '\033[3;3H'
c=0
while [ "$c" -lt 72 ]; do
	rr=$((40 + c * 2))
	gg=$((120))
	bb=$((240 - c * 2))
	printf '\033[48;2;%d;%d;%dm ' "$rr" "$gg" "$bb"
	c=$((c + 1))
done
printf '\033[0m'

# --- row 3 (ANSI 4): 256-color strip --------------------------------------
printf '\033[4;3H'
n=16
while [ "$n" -lt 88 ]; do
	printf '\033[48;5;%dm ' "$n"
	n=$((n + 1))
done
printf '\033[0m'

# --- row 4 (ANSI 5): 16-color (basic) blocks ------------------------------
printf '\033[5;3H'
b=0
while [ "$b" -le 7 ]; do
	printf '\033[4%dm  \033[10%dm  ' "$b" "$b"
	b=$((b + 1))
done
printf '\033[0m'

# --- row 6 (ANSI 7): Devanagari (truecolor) -------------------------------
printf '\033[7;3H\033[38;2;255;210;120mविचय · विवेचक · निधि — दृश्यते सत्यम्\033[0m'

# --- row 7 (ANSI 8): CJK fullwidth (256-color) ----------------------------
printf '\033[8;3H\033[38;5;213m終端 真実 テスト\033[0m'

# --- row 8 (ANSI 9): emoji (basic color) ----------------------------------
printf '\033[9;3H\033[32m✓ \033[31m✗ \033[33m⚠\033[0m'

# Park a HIDDEN cursor at grid (23,79) == ANSI (24,80).
printf '\033[24;80H\033[?25l'

# Block forever. SIGWINCH-safe loop (a bare read can be interrupted and exit).
while :; do read -r _ || :; done
