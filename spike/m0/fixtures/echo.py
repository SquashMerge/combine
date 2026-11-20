#!/usr/bin/env python3
"""Raw-mode echo: every input byte is written straight back (keystroke-latency fixture)."""
import os, sys, tty

tty.setraw(0)
os.write(1, b"echo ready\r\n")
while True:
    b = os.read(0, 4096)
    if not b:
        break
    os.write(1, b)
