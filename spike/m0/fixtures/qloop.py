#!/usr/bin/env python3
"""On 'q': ask OSC 11 (background) and CSI 18 t (text area size); print the replies as text.
On any other byte: print "<got XX>" in hex. Used to see which viewer's facts answer queries."""
import os, select, sys, tty

fd = sys.stdin.fileno()
tty.setraw(fd)


def ask(q):
    os.write(1, q)
    buf = b""
    while True:
        r, _, _ = select.select([fd], [], [], 0.5 if not buf else 0.1)
        if not r:
            return buf
        buf += os.read(fd, 256)


os.write(1, b"qloop ready\r\n")
while True:
    b = os.read(fd, 64)
    if not b:
        break
    for c in b:
        if c == ord("q"):
            a = ask(b"\x1b]11;?\x1b\\")
            s = ask(b"\x1b[18t")
            os.write(1, ("REPLY bg=%r size=%r\r\n" % (a, s)).encode())
        elif c == ord("x"):
            sys.exit(0)
        else:
            os.write(1, b"<got %02x>" % c)
