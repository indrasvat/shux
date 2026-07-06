#!/bin/sh
# f7_winsize.sh — lens fixture F7 (§11 TEST-1).
#
# Prints the current terminal size (rows cols) via `stty size` as a cyan line
# `SIZE=<rows> <cols>` ($COLUMNS/$LINES are NOT set in non-interactive sh, so
# stty is the only truth), then blocks. A SIGWINCH trap reprints the new size
# on the next line so a live resize is observable.
#
# Blocking loop (p0-council-r2 major 1): `while read -r _ || [ $? -gt 128 ];
# do :; done` — a SIGWINCH-interrupted `read` returns >128 (POSIX) and the loop
# CONTINUES (signal-survival is load-bearing for R5); EOF returns 1 and the
# loop EXITS cleanly. NEVER `|| :` inside `while :` — that busy-spins at 100%
# CPU once stdin hits EOF. Used by: R3, R5.

printf '\033[2J\033[H'
# House-rule colour content (truecolor / 256 / basic).
printf '\033[38;2;120;220;160mtc\033[0m \033[38;5;208m256\033[0m \033[36mbasic\033[0m\n'

report() { printf '\033[36mSIZE=%s\033[0m\n' "$(stty size)"; }
report

# Reprint on resize. Newline first so each report is its own capture line.
trap 'printf "\n"; report' WINCH

while read -r _ || [ $? -gt 128 ]; do :; done
