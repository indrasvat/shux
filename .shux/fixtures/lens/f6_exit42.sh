#!/bin/sh
# f6_exit42.sh — lens fixture F6 (§11 TEST-1).
#
# Prints a colour line + "BYE" (red bold), then exits 42. No tokens, no sleeps.
# Used by R1 (scratch lifecycle: `lens run --wait` must surface exit code 42).

printf '\033[2J\033[H'
# House-rule colour content: truecolor / 256 / basic on one line.
printf '\033[38;2;120;220;160mtc\033[0m \033[38;5;208m256\033[0m \033[36mbasic\033[0m\n'
printf '\033[1;38;2;220;40;40mBYE\033[0m\n'
exit 42
