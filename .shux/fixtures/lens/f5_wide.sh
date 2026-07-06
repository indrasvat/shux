#!/bin/sh
# f5_wide.sh — lens fixture F5 (§11 TEST-1).
#
# Static Unicode-width torture frame for a 100x30 pane. Blocks forever after
# drawing (no tokens, no sleeps). Used by G2w as a golden fidelity anchor.
#
# Expected display widths (unicode-width; the raster must agree):
#   終端真実  → 4 fullwidth cells = 8 columns
#   ❤️ ⚠️ ✔️   → emoji + VS16: 2 columns each (emoji presentation)
#   क्षत्रिय   → क + ् + ष + त + ् + र + ि + य : combining marks are width 0;
#              renders as 5 base clusters (क्ष त् रि य -> क्षत्रिय), width 5
#   संस्कृति   → स + ं + स + ् + क + ृ + त + ि : width 5
#   ┏┳┓┃┣╋┫┗┻┛ box-drawing joins → 1 column each
#
# Coordinates 0-based grid; ANSI = (row+1,col+1). Colours: truecolor + 256 +
# basic (house rule).

printf '\033[2J\033[3J\033[H'

# Title / wait_for sentinel.
printf '\033[1;2H\033[1;38;2;250;250;250mLENS-F5-WIDE\033[0m'

# Box-drawing joins (heavy) — a small grid of tee/cross glyphs.
printf '\033[3;2H\033[38;2;120;200;255m┏━━━┳━━━┓\033[0m'
printf '\033[4;2H\033[38;2;120;200;255m┣━━━╋━━━┫\033[0m'
printf '\033[5;2H\033[38;2;120;200;255m┗━━━┻━━━┛\033[0m'

# CJK fullwidth (truecolor).
printf '\033[7;2H\033[38;2;255;210;120m終端 真実 テスト 界面\033[0m'

# Emoji with VS16 emoji-presentation selectors (256-color labels).
printf '\033[9;2H\033[38;5;213m❤️ ⚠️ ✔️ 🎯 🧪\033[0m'

# Combining Devanagari clusters (basic color).
printf '\033[11;2H\033[35mक्षत्रिय · संस्कृति · दृश्यते\033[0m'

# --- Colour legend so all three colour classes are present ----------------
# truecolor gradient (row 20)
printf '\033[21;2H'
c=0
while [ "$c" -lt 90 ]; do
	printf '\033[48;2;%d;80;%dm ' "$((40 + c * 2))" "$((220 - c * 2))"
	c=$((c + 1))
done
printf '\033[0m'
# 256-color strip (row 22)
printf '\033[23;2H'
n=16
while [ "$n" -lt 106 ]; do printf '\033[48;5;%dm ' "$n"; n=$((n + 1)); done
printf '\033[0m'
# basic blocks (row 24)
printf '\033[25;2H'
b=0
while [ "$b" -le 7 ]; do printf '\033[4%dm  \033[10%dm  ' "$b" "$b"; b=$((b + 1)); done
printf '\033[0m'

# Hidden cursor parked bottom-right (grid (29,99)).
printf '\033[30;100H\033[?25l'

# Block while stdin is open; exit cleanly on EOF (no EOF busy-spin —
# p0-council-r2 major 1).
cat >/dev/null
