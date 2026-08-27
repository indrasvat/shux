import sys, os, termios, tty
sys.stdout.write("\x1b[?1000h\x1b[?1002h\x1b[?1006h")
sys.stdout.write("mouse-echo: asking shux for mouse reports; every one I receive is printed below.\r\n")
sys.stdout.flush()
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
buf = b""; n = 0
BAN = {
 b'A': "\x1b[1;36m\r\n== PHASE 1: PLAIN CLICK (no modifier) -- the app should receive these ==\x1b[0m\r\n",
 b'B': "\x1b[1;33m\r\n== PHASE 2: SHIFT+CLICK -- shux should keep these; the app should see NOTHING ==\x1b[0m\r\n",
 b'C': "\x1b[1;32m\r\n== RESULT: 4 reports from phase 1, 0 from phase 2 -- Shift took the mouse back ==\x1b[0m\r\n",
}
try:
    while True:
        c = os.read(fd, 1)
        if not c: break
        if c == b'q': break
        if c in BAN:
            sys.stdout.write(BAN[c]); sys.stdout.flush(); buf = b""; continue
        buf += c
        if buf.startswith(b"\x1b[<") and c in (b'M', b'm'):
            n += 1
            kind = "press  " if c == b'M' else "release"
            sys.stdout.write("  app got report #%d  %s  at %s\r\n" % (n, kind, buf[3:-1].decode('ascii','replace')))
            sys.stdout.flush(); buf = b""
        elif len(buf) > 32:
            buf = b""
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
