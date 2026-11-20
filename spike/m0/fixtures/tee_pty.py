#!/usr/bin/env python3
"""Run a program under an inner pty, pass bytes both ways, and append its output to TRACE.

usage: tee_pty.py TRACE prog [args...]
The trace is the oracle's input: a reference emulator fed it sees the whole stream from spawn.
SIGWINCH and the size are passed through to the inner pty.
"""
import fcntl, os, pty, select, signal, struct, sys, termios, tty

trace = open(sys.argv[1], "ab", buffering=0)
argv = sys.argv[2:]


def size():
    return fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8)


pid, fd = pty.fork()
if pid == 0:
    os.execvp(argv[0], argv)
fcntl.ioctl(fd, termios.TIOCSWINSZ, size())


def winch(*_):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, size())
    trace.write(b"\x1b]m0;winch;" + ("%dx%d" % struct.unpack("HHHH", size())[1::-1]).encode() + b"\x07")


signal.signal(signal.SIGWINCH, winch)
tty.setraw(0)
while True:
    try:
        r, _, _ = select.select([0, fd], [], [])
    except InterruptedError:
        continue
    if fd in r:
        try:
            b = os.read(fd, 65536)
        except OSError:
            break
        if not b:
            break
        os.write(1, b)
        trace.write(b)
    if 0 in r:
        b = os.read(0, 65536)
        if not b:
            break
        os.write(fd, b)
os.waitpid(pid, 0)
