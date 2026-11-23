"""The program the herdr-core lane runs in each terminal.

It echoes what is typed (but not a terminal's replies to its queries), appends all of it to
`ctl/typed`, and prints whatever the test asks for through
the `ctl/emit` FIFO (a 4-byte big-endian length and that many bytes), answering each request with
one byte on `ctl/ack` once the bytes are written to the terminal. `ctl/ready` appears when it is
listening. It exits when its terminal closes or its control directory is removed, and it sends no
signals.
"""
import errno
import os
import re
import select
import sys
import tty

# What a terminal answers to the program's queries (CSI and OSC sequences): typed text is echoed,
# but a reply is not, so a client's answer to a query never becomes output of its own.
REPLY = re.compile(rb"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))")


def write_all(fd, data):
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        view = view[written:]


def read_exact(fd, count):
    chunks = []
    while count:
        chunk = os.read(fd, count)
        if not chunk:
            raise EOFError
        chunks.append(chunk)
        count -= len(chunk)
    return b"".join(chunks)


def main():
    ctl = os.environ["COMBINE_HC_CTL"]
    tty.setraw(0)
    emit_path = os.path.join(ctl, "emit")
    ack_path = os.path.join(ctl, "ack")
    for path in (emit_path, ack_path):
        os.mkfifo(path, 0o600)
    emit_fd = os.open(emit_path, os.O_RDWR)
    ack_fd = os.open(ack_path, os.O_RDWR)
    typed_fd = os.open(os.path.join(ctl, "typed"), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    staged = os.path.join(ctl, "ready.tmp")
    with open(staged, "w") as handle:
        handle.write("ready\n")
    os.rename(staged, os.path.join(ctl, "ready"))
    while True:
        if not os.path.isdir(ctl):
            return
        readable, _, _ = select.select([0, emit_fd], [], [], 1.0)
        if 0 in readable:
            try:
                data = os.read(0, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    return
                raise
            if not data:
                return
            write_all(typed_fd, data)
            write_all(1, REPLY.sub(b"", data))
        if emit_fd in readable:
            length = int.from_bytes(read_exact(emit_fd, 4), "big")
            write_all(1, read_exact(emit_fd, length))
            write_all(ack_fd, b"\x01")


try:
    main()
except (BrokenPipeError, EOFError, FileNotFoundError):
    sys.exit(0)
