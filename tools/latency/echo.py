#!/usr/bin/env python3
"""Raw-mode echo: every input byte is written straight back (keystroke-latency fixture).

Run it as a terminal's program in an isolated herdr-core (see README.md). It puts its
terminal in raw mode, prints one ready line, then writes back each read at once, unchanged,
until its input ends. Standard library only; run it with `python3 -I`.
"""
import os
import sys
import termios
import tty


def main():
    saved = termios.tcgetattr(0)
    tty.setraw(0)
    try:
        os.write(1, b"echo ready\r\n")
        while True:
            chunk = os.read(0, 4096)
            if not chunk:
                break
            os.write(1, chunk)
    finally:
        termios.tcsetattr(0, termios.TCSAFLUSH, saved)
    return 0


if __name__ == "__main__":
    sys.exit(main())
