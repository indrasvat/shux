#!/bin/sh
# f7_winsize.sh — lens fixture F7 (§11 TEST-1).
#
# Prints the current terminal size (rows cols) via `stty size` as a cyan line
# `SIZE=<rows> <cols>` ($COLUMNS/$LINES are NOT set in non-interactive sh, so
# stty is the only truth), then blocks. A SIGWINCH trap reprints the new size
# on the next line so a live resize is observable.
#
# The blocking loop MUST be `while :; do read -r _ || :; done`: a bare `read`
# can be interrupted by SIGWINCH and exit the script, which would kill the pane
# out from under R5. The loop is load-bearing. Used by: R3, R5.

printf '\033[2J\033[H'
# House-rule colour content (truecolor / 256 / basic).
printf '\033[38;2;120;220;160mtc\033[0m \033[38;5;208m256\033[0m \033[36mbasic\033[0m\n'

report() { printf '\033[36mSIZE=%s\033[0m\n' "$(stty size)"; }
report

# Reprint on resize. Newline first so each report is its own capture line.
trap 'printf "\n"; report' WINCH

while :; do read -r _ || :; done
