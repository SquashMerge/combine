#!/usr/bin/env python3
"""Output fixture: on each 'g' typed, write MB megabytes of numbered lines, then idle.

Each line is exactly 64 bytes: "L<seq:010d> " + filler + "\r\n" so a consumer can check
order, loss and duplication from the bytes alone. The line counter continues across
bursts. '1'..'9' write that many single lines (small, for interactive tests). 'x' exits.
usage: flood.py [MB]
"""
import os, sys, termios, tty

mb = float(sys.argv[1]) if len(sys.argv) > 1 else 50
fd = sys.stdin.fileno()
tty.setraw(fd)
seq = 0
FILL = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"


def line():
    global seq
    s = "L%010d " % seq
    seq += 1
    return (s + FILL[: 62 - len(s)] + "\r\n").encode()


while True:
    b = os.read(fd, 16)
    if not b:
        break
    for c in b.decode(errors="replace"):
        if c == "g":
            n = int(mb * 1024 * 1024 / 64)
            chunk = []
            for _ in range(n):
                chunk.append(line())
                if len(chunk) == 1024:
                    os.write(1, b"".join(chunk))
                    chunk = []
            if chunk:
                os.write(1, b"".join(chunk))
            os.write(1, b"DONE %d\r\n" % seq)
        elif c.isdigit() and c != "0":
            os.write(1, b"".join(line() for _ in range(int(c))))
        elif c == "x":
            sys.exit(0)
