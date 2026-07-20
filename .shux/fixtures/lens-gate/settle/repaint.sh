#!/bin/sh
# repaint.sh — gate fixture: self-animating IDENTICAL repainter (task 083).
#
# `repaint.sh <TEXT>` repaints row 10 with a byte-IDENTICAL `<TEXT>` line every ~25ms
# for a bounded number of frames. The presented FRAME HASH is constant (each repaint is
# identical) while `content_revision` bumps per repaint (§4.2 is value-independent), so
# quiet-mode `wait-settled` never quiets (times out) but `--stable-frames`/`--hold-ms`
# settle. Parameterizing the text lets one fixture mint a golden AND (with a different
# TEXT via `-- argv` override) render a stable-but-WRONG frame for the retry test.
#
# Self-animating (an inline `sleep`, unlike the token-paced §12 lens fixtures) because a
# gate SCENARIO drives steps sequentially and cannot pump input while a settle step waits.
# Bounded (exits after the loop) and killed on scratch teardown → no leak. Colours:
# truecolor line + static 256/basic legend (house rule → monochrome regressions fail).

TEXT=${1:-HEARTBEAT}

printf '\033[2J\033[3J\033[H'
stty -echo 2>/dev/null || :

# Title / wait_for sentinel (static, identical across every TEXT).
printf '\033[1;1H\033[1;38;2;250;250;250mGATE-REPAINT\033[0m'
printf '\033[21;3H'
n=16
while [ "$n" -lt 88 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
printf '\033[22;3H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'

i=0
while [ "$i" -lt 240 ]; do
	printf '\033[11;1H\033[2K\033[38;2;80;220;120mREPAINT:%s\033[0m\033[24;80H' "$TEXT"
	sleep 0.025
	i=$((i + 1))
done
