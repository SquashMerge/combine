#!/usr/bin/env python3
"""Run `hc attach <t>` under a pty against HC_SOCKET (a proxy), script some input, detach.

usage: pty_attach.py TERMINAL OUTFILE [script...]
script steps: "sleep:<s>", "send:<python-escaped bytes>", "size:<cols>x<rows>", "detach"
Everything hc attach writes to its pty is saved to OUTFILE.
The isolation env (HC_SESSION, XDG_*) must already be set by the caller; HC_SOCKET must
not be the default socket.
"""
import fcntl, os, pty, select, struct, sys, termios, time

term, outfile, *script = sys.argv[1:]
sock = os.environ.get("HC_SOCKET", "")
if not sock or "/herdr-core/default/" in sock:
    sys.exit("set HC_SOCKET to the isolated core or proxy")
for k in ("HC_TERMINAL_ID", "HC_TERMINAL_SOCKET", "HC_SCOPE"):
    os.environ.pop(k, None)


def setsize(fd, cols, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


pid, fd = pty.fork()
if pid == 0:
    os.execvp("hc", ["hc", "attach", term])
setsize(fd, 80, 24)
os.kill(pid, 28)  # SIGWINCH
out = open(outfile, "wb")


def drain(t):
    end = time.time() + t
    while True:
        r, _, _ = select.select([fd], [], [], max(0, end - time.time()))
        if not r:
            return
        try:
            b = os.read(fd, 65536)
        except OSError:
            return
        if not b:
            return
        out.write(b)


drain(1.0)
for step in script:
    kind, _, arg = step.partition(":")
    if kind == "sleep":
        drain(float(arg))
    elif kind == "send":
        os.write(fd, arg.encode().decode("unicode_escape").encode("latin-1"))
        drain(0.3)
    elif kind == "size":
        c, r = arg.split("x")
        setsize(fd, int(c), int(r))
        os.kill(pid, 28)
        drain(0.5)
    elif kind == "detach":
        os.write(fd, b"\x1c")
        drain(1.0)
try:
    os.kill(pid, 15)
except ProcessLookupError:
    pass
os.waitpid(pid, 0)
out.close()
