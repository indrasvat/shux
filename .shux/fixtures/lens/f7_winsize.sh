#!/bin/sh
# f7_winsize.sh — lens fixture F7 (§11 TEST-1).
#
# Prints the current terminal size (rows cols) via `stty size` as a cyan line
# `SIZE=<rows> <cols>` ($COLUMNS/$LINES are NOT set in non-interactive sh, so
# stty is the only truth), then blocks. A SIGWINCH trap reprints the new size
# on the next line so a live resize is observable.
#
# Blocking loop (p0-council-r2 major 1). Signal-survival is load-bearing for R5:
# a SIGWINCH must not end the fixture, while a genuine EOF must end it — and NOT
# by spinning.
#
# The fixture does not block in `read`: whether a trapped signal interrupts a
# `read` differs between shells and between runs (macOS's bash restarts it at
# times, leaving no trace in the status), so no flag or status can tell a resize
# from an EOF. A background `cat` reads the terminal instead, and the shell
# blocks in `wait`, which POSIX requires a trapped signal to interrupt. The
# fixture ends only when `cat` has, on EOF. Used by: R3, R5.

printf '\033[2J\033[H'
# House-rule colour content (truecolor / 256 / basic).
printf '\033[38;2;120;220;160mtc\033[0m \033[38;5;208m256\033[0m \033[36mbasic\033[0m\n'

report() { printf '\033[36mSIZE=%s\033[0m\n' "$(stty size)"; }
report

# Reprint on resize. Newline first so each report is its own capture line.
trap 'printf "\n"; report' WINCH

# An asynchronous list's stdin is /dev/null before its own redirections, so the
# terminal is handed over on a saved descriptor. SIGWINCH is ignored by `cat`.
exec 3<&0
cat <&3 >/dev/null &
reader=$!
while ! wait "$reader"; do
	kill -0 "$reader" 2>/dev/null || break
done
