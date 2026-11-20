"""Minimal herdr-core framed-protocol client (protocol generation 1), as observed in M0.

Wire format after `hello {framed:true}` (the hello request and its reply are plain
newline-terminated JSON lines):

    u32 LE channel | u8 kind | u32 LE payload length | payload

channel 0 carries JSON-RPC (kind 0). Attach channels (from AttachResult.channel):
    0 DATA     bytes (core->viewer: paint + live output; viewer->core: input)
    1 CREDIT   u32 LE bytes (viewer->core)
    2 CLOSE    empty
    3 RESIZE   u16 LE cols, u16 LE rows (viewer->core; core->viewer only with resize_notifications)
    4 FOCUS    u8: 1 in, 0 out
    5 FACTS    JSON Display {fg,bg,cursor,cell}
    6 AUTOMATIC_INPUT  bytes (input not typed by a person)
"""
import json, os, socket, struct, time

DATA, CREDIT, CLOSE, RESIZE, FOCUS, FACTS, AUTO = range(7)
HDR = struct.Struct("<IBI")
DEFAULT_SOCK_MARK = "/herdr-core/default/"


def isolated_socket():
    p = os.environ.get("M0_SOCK", "/tmp/cm0/run/herdr-core/m0.sock")
    if DEFAULT_SOCK_MARK in p:
        raise SystemExit("refusing the default herdr-core socket")
    return p


class Conn:
    def __init__(self, path=None, framed=True):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(path or isolated_socket())
        self.buf = b""
        self.next_id = 1
        self.pending = []  # frames read while waiting for something else
        self.s.sendall(json.dumps({"jsonrpc": "2.0", "id": 0, "method": "hello",
                                   "params": {"framed": framed, "generation": 1}}).encode() + b"\n")
        while b"\n" not in self.buf:
            self.buf += self.s.recv(65536)
        line, self.buf = self.buf.split(b"\n", 1)
        self.hello = json.loads(line)["result"]

    def send(self, ch, kind, payload=b""):
        self.s.sendall(HDR.pack(ch, kind, len(payload)) + payload)

    def read_frame(self, timeout=None):
        self.s.settimeout(timeout)
        try:
            while len(self.buf) < 9 or len(self.buf) < 9 + HDR.unpack_from(self.buf)[2]:
                b = self.s.recv(1 << 20)
                if not b:
                    return None
                self.buf += b
        except socket.timeout:
            return "timeout"
        ch, kind, n = HDR.unpack_from(self.buf)
        p = self.buf[9:9 + n]
        self.buf = self.buf[9 + n:]
        return ch, kind, p

    def frame(self, timeout=None):
        if self.pending:
            return self.pending.pop(0)
        return self.read_frame(timeout)

    def call(self, method, params=None, timeout=30):
        i = self.next_id
        self.next_id += 1
        self.send(0, DATA, json.dumps({"jsonrpc": "2.0", "id": i, "method": method,
                                       "params": params or {}}).encode())
        end = time.time() + timeout
        while True:
            f = self.read_frame(max(0.01, end - time.time()))
            if f == "timeout" or f is None:
                raise RuntimeError("no reply to %s: %r" % (method, f))
            ch, kind, p = f
            if ch == 0:
                m = json.loads(p)
                if m.get("id") == i:
                    if "error" in m:
                        raise RuntimeError(json.dumps(m["error"]))
                    return m["result"]
            self.pending.append(f)

    def attach(self, terminal, cols, rows, **kw):
        r = self.call("terminal.attach", dict(terminal=terminal, cols=cols, rows=rows, **kw))
        return r

    def credit(self, ch, n):
        self.send(ch, CREDIT, struct.pack("<I", n))

    def resize(self, ch, cols, rows):
        self.send(ch, RESIZE, struct.pack("<HH", cols, rows))
