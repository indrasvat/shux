import sys
import time

sys.stdout.write("old-frame")
sys.stdout.flush()
sys.stdout.write("\x1b[?2026h\x1b[1;1Hpending-frame")
sys.stdout.flush()
time.sleep(1.0)
sys.stdout.write("\x1b[?2026l\x1b[2;1Hsync-output committed")
sys.stdout.flush()
time.sleep(60)
