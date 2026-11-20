#!/usr/bin/env python3
"""Second M0 fixture: main-screen state that modes.py does not cover. Idles after setup.

Leaves: G0 = DEC special graphics, autowrap off (?7l), origin mode (?6h) with DECSTBM 5;15,
insert mode (IRM 4h), reverse wraparound (?45h), any-event mouse ?1003 + SGR ?1006,
alternate scroll ?1007, cursor blink ?12h, kitty keyboard flags pushed (CSI > 1 u),
modifyOtherKeys 2 (CSI > 4;2 m), palette entry 1 changed (OSC 4), default fg changed (OSC 10),
an open OSC 8 hyperlink, and a pen with curly underline + underline colour.
Commands: g = write "lqk" (charset), i = CR then "INS" (IRM), o = CUP 1;1 then "O" (DECOM),
          w = 90 x "W" (autowrap off), x = exit.
usage: modes2.py [TRACEFILE]
"""
import os, sys, tty

E = "\x1b"
ST = E + "\\"
trace = open(sys.argv[1], "ab", buffering=0) if len(sys.argv) > 1 else None


def w(s):
    b = s.encode()
    os.write(1, b)
    if trace:
        trace.write(b)


fd = sys.stdin.fileno()
tty.setraw(fd)
for i in range(1, 9):
    w("line %d: some text here\r\n" % i)
w(E + "]4;1;rgb:12/34/56" + ST)
w(E + "]10;rgb:aa/bb/cc" + ST)
w(E + "[31mpalette-1 red?" + E + "[0m\r\n")
w(E + "]8;;https://example.com/m0" + ST + "link-open ")
w(E + "[?1003h" + E + "[?1006h" + E + "[?1007h" + E + "[?12h" + E + "[?45h")
w(E + "[>1u" + E + "[>4;2m")
w(E + "(0lqqqk" + E + "(B" + " ascii " + E + "(0")  # leave G0 = DEC graphics
w(E + "[5;15r" + E + "[?6h" + E + "[3;4H")  # origin mode: row 3 of the region = screen row 7
w(E + "[?7l" + E + "[4h")
w(E + "[4:3m" + E + "[58;2;255;0;0m")
while True:
    b = os.read(fd, 16)
    if not b:
        break
    for c in b.decode(errors="replace"):
        if c == "g":
            w("lqk")
        elif c == "i":
            w("\rINS")
        elif c == "o":
            w(E + "[1;1HO")
        elif c == "w":
            w("W" * 90)
        elif c == "x":
            sys.exit(0)
