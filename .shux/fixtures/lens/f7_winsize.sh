#!/bin/sh
# f7_winsize.sh — lens fixture F7 (§11 TEST-1).
#
# Prints the current terminal size (rows cols) via `stty size` as a cyan line
# `SIZE=<rows> <cols>` ($COLUMNS/$LINES are NOT set in non-interactive sh, so
# stty is the only truth), then blocks. A SIGWINCH trap reprints the new size
# on the next line so a live resize is observable.
#
# Blocking loop (p0-council-r2 major 1). Signal-survival is load-bearing for R5:
# a SIGWINCH must interrupt the blocking read WITHOUT ending the loop, while a
# genuine EOF must end it — and NOT by spinning, because `|| :` inside `while :`
# busy-waits at 100% CPU once stdin closes.
#
# This used to key on the exit status: `while read -r _ || [ $? -gt 128 ]`, on
# the grounds that an interrupted read returns 128+signo. bash does that; dash
# does NOT — it returns 1, exactly like EOF. Every Debian/Ubuntu box has dash as
# /bin/sh, so there the resize killed the fixture instead of being reported by
# it, and `resize_step_drives_a_winsize_aware_child` failed with `child_error:
# exit 0` on a runner whose only sin was not being macOS.
#
# The status cannot distinguish the two cases portably, so it is not asked to.
# The trap records that it fired; the loop reads that flag. Traps run before the
# interrupted builtin's status is tested in both shells, so the flag is always
# set by the time it is read. Used by: R3, R5.

printf '\033[2J\033[H'
# House-rule colour content (truecolor / 256 / basic).
printf '\033[38;2;120;220;160mtc\033[0m \033[38;5;208m256\033[0m \033[36mbasic\033[0m\n'

report() { printf '\033[36mSIZE=%s\033[0m\n' "$(stty size)"; }
report

# Reprint on resize. Newline first so each report is its own capture line.
winched=0
trap 'winched=1; printf "\n"; report' WINCH

while :; do
	read -r _ && continue
	# read failed: either the trap fired (keep going) or stdin closed (stop).
	[ "$winched" = 1 ] || break
	winched=0
done
