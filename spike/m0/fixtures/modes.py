#!/usr/bin/env python3
"""Deterministic M0 fixture: sets input modes, enters the alt screen, paints, idles.

State it leaves behind (what a fresh attach must restore):
  main screen   : 3 lines "MAIN-1..3" (visible again after 'q')
  alt screen    : active (?1049), with a header, colours, wide chars, emoji
  modes         : mouse ?1000 + ?1002 + SGR ?1006, focus ?1004, bracketed paste ?2004,
                  DECCKM ?1h, DECKPAM (ESC =), cursor hidden ?25l, DECSCUSR 4 (steady underline)
  scroll region : DECSTBM 3..20
  saved cursor  : DECSC at (row 15, col 5)
  pen           : bold magenta (SGR 1;35) left active at the end
  charset       : G0 = ASCII (default; the 'g' command switches to DEC graphics)
  cursor        : row 10, col 20 (1-based)
  title         : OSC 2 "m0-fixture"

Commands read from stdin (raw mode), each answered by output so the verifier can
check that live bytes after the first paint land where the reference expects:
  p  write "PEN" with whatever pen is active (tests SGR pen restore)
  r  DECRC, then write "RC" (tests saved-cursor restore)
  w  write "WRAP" at col 78 of an 80-col line (tests autowrap mode; ?7 untouched)
  s  print 3 lines inside the scroll region at its bottom (tests DECSTBM restore)
  q  leave the alt screen (tests main-screen restore), then print "BACK"
  k  report the last 8 input bytes as hex on row 22 (tests input encoding from a client)
  x  exit
Any other byte is recorded for 'k'.
usage: modes.py [TRACEFILE]
"""
import os, sys, termios, tty

E = "\x1b"
out = sys.stdout
# Oracle trace: every byte written, so a reference emulator can replay the whole stream.
trace = open(sys.argv[1], "ab", buffering=0) if len(sys.argv) > 1 else None


def w(s):
    b = s.encode()
    os.write(1, b)
    if trace:
        trace.write(b)


fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
try:
    # Main screen content first.
    w("MAIN-1 plain\r\nMAIN-2 " + E + "[31mred" + E + "[0m\r\nMAIN-3 end\r\n")
    # Title, alt screen, modes.
    w(E + "]2;m0-fixture\x07")
    w(E + "[?1049h" + E + "[H" + E + "[2J")
    w(E + "[?1000h" + E + "[?1002h" + E + "[?1006h")  # mouse: normal + button-event, SGR encoding
    w(E + "[?1004h")  # focus reporting
    w(E + "[?2004h")  # bracketed paste
    w(E + "[?1h" + E + "=")  # DECCKM + DECKPAM
    w(E + "[4 q")  # steady underline cursor
    # Content.
    w(E + "[1;1H" + E + "[7m M0 FIXTURE (alt screen) " + E + "[0m")
    w(E + "[2;1H" + E + "[1mbold" + E + "[0m " + E + "[3mitalic" + E + "[0m " + E + "[4munder" + E + "[0m "
      + E + "[38;5;208m256c" + E + "[0m " + E + "[38;2;10;200;30;48;2;40;0;80mtruecolor" + E + "[0m")
    w(E + "[3;1Hwide: 漢字テスト emoji: 🎉🚀 combining: é flag: 🇵🇹 end")
    w(E + "[4;1H" + E + "[44m" + E + "[K" + "blue bg to EOL" + E + "[0m")
    for r in range(5, 9):
        w(E + "[%d;1Hrow %d " % (r, r) + "." * (30 + r))
    # Saved cursor, then scroll region (DECSTBM homes the cursor), then final cursor + pen.
    w(E + "[15;5H" + E + "7")
    w(E + "[3;20r")
    w(E + "[10;20H")
    w(E + "[?25l")
    w(E + "[1;35m")
    rec = []
    while True:
        b = os.read(fd, 64)
        if not b:
            break
        for c in b:
            ch = chr(c)
            if ch == "p":
                w("PEN")
            elif ch == "r":
                w(E + "8RC")
            elif ch == "w":
                w(E + "[23;78HWRAP")
            elif ch == "s":
                w(E + "[20;1H" + "\r\nS1\r\nS2\r\nS3")
            elif ch == "q":
                w(E + "[?1049l" + "BACK")
            elif ch == "k":
                w(E + "[22;1H" + E + "[0mIN:" + " ".join("%02x" % x for x in rec[-8:]) + E + "[K")
            elif ch == "x":
                raise SystemExit(0)
            else:
                rec.append(c)
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
