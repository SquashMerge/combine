#!/usr/bin/env python3
"""Keystroke-echo latency through a real `combined` daemon (S9, M1 acceptance).

Starts a bridge command (argv after `--`) with piped stdin and stdout, speaks Combine's wire
protocol over those pipes (SPEC.md, "Wire protocol", version 4), attaches to one terminal that
runs a raw-mode echo program (tools/latency/echo.py), and times single-byte input frames until
a data frame carrying each byte's echo arrives. Timing stops when the data frame is read off
the bridge's stdout, before any rendering.

    python3 -I tools/latency/echo_latency.py --label role=echo --samples 2000 \
        --json /tmp/latency.json -- ssh -T -o BatchMode=yes mini /opt/homebrew/bin/combined connect

Standard library only. Every wait has a timeout; a `gap`, `lost`, `closed` or `error` frame,
a bridge that exits, or a malformed frame ends the run with a message and exit code 1.

Exit codes: 0 when the run finished (timed-out samples are counted, not fatal), 1 for a
failure, 2 for a usage error.
"""

import argparse
import datetime
import json
import math
import os
import platform
import select
import socket
import string
import subprocess
import sys
import threading
import time

PROTOCOL_VERSION = 4

# Frame kinds (SPEC.md, "Kinds").
KIND_CONTROL = 1
KIND_DATA = 2
KIND_INPUT = 3
KIND_AUTOMATIC_REPLY = 4

MAX_FRAME_LEN = 1 << 20  # counts the kind byte and the payload
DATA_HEADER = 24  # u64 attachment, u64 epoch, u64 seq
INPUT_HEADER = 8  # u64 attachment
MAX_TEXT = 256  # how much peer text is shown

# Distinct printable bytes, sent in rotation so an echo can be matched to its sample.
ALPHABET = (string.ascii_letters + string.digits).encode("ascii")

# The display facts sent with the attach. The harness renders nothing; these only make the
# attach valid.
DISPLAY = {
    "foreground": "#ffffff",
    "background": "#000000",
    "cursor_color": "#ffffff",
    "cell_width": 8,
    "cell_height": 16,
}

# Events that carry a cursor and are acked (SPEC.md, "Daemon to client", attachment events).
CURSOR_EVENTS = frozenset(
    ["started", "data", "resized", "facts", "resynced", "closed", "lost"]
)

DAEMON_FLUSH_MS = 16  # the daemon flushes output at most once per 16 ms (SPEC.md, Architecture)


class HarnessError(Exception):
    """A failure that ends the run. Its text is shown to the user."""


class ProtocolError(HarnessError):
    """The bridge sent something that is not Combine's wire protocol."""


class Timeout(HarnessError):
    """A wait ran out of time."""


# ---- frame encoding ----


def u64(value):
    if not isinstance(value, int) or value < 0 or value >= 1 << 64:
        raise ValueError("not a u64: %r" % (value,))
    return value.to_bytes(8, "big")


def frame(kind, payload):
    """`u32 length | u8 kind | payload`, big-endian; the length counts kind and payload."""
    length = 1 + len(payload)
    if length > MAX_FRAME_LEN:
        raise ValueError("frame of %d bytes is over the 1 MiB cap" % length)
    return length.to_bytes(4, "big") + bytes([kind]) + payload


def encode_control(message):
    """A control frame: one JSON object tagged by `type`."""
    if not isinstance(message, dict) or "type" not in message:
        raise ValueError("a control message is a JSON object with a type")
    payload = json.dumps(message, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return frame(KIND_CONTROL, payload)


def encode_input(attachment, data):
    """Typed input: frame kind 3, `u64 attachment` then the bytes. Never JSON."""
    return frame(KIND_INPUT, u64(attachment) + bytes(data))


def safe_text(text, limit=MAX_TEXT):
    """Peer text made safe to print: control and bidi formatting characters become spaces,
    whitespace runs collapse, and the result is cut after `limit` characters."""
    if not isinstance(text, str):
        text = str(text)
    bidi = set("؜‎‏‪‫‬‭‮⁦⁧⁨⁩")
    cleaned = []
    for ch in text:
        code = ord(ch)
        if code < 0x20 or 0x7F <= code < 0xA0 or ch in bidi:
            cleaned.append(" ")
        else:
            cleaned.append(ch)
    out = " ".join("".join(cleaned).split())
    if len(out) > limit:
        out = out[:limit] + "…"
    return out


def parse_daemon_frame(kind, payload):
    """Decodes one daemon-to-client frame into a dict with a `type`.

    A data frame becomes `{"type": "data", "cursor": {...}, "bytes": b"..."}`; a control frame
    is its JSON object. Anything else is a ProtocolError.
    """
    if kind == KIND_DATA:
        if len(payload) < DATA_HEADER:
            raise ProtocolError(
                "data frame payload is %d bytes, shorter than its 24-byte header" % len(payload)
            )
        return {
            "type": "data",
            "cursor": {
                "attachment": int.from_bytes(payload[0:8], "big"),
                "epoch": int.from_bytes(payload[8:16], "big"),
                "seq": int.from_bytes(payload[16:24], "big"),
            },
            "bytes": bytes(payload[DATA_HEADER:]),
        }
    if kind == KIND_CONTROL:
        try:
            message = json.loads(bytes(payload).decode("utf-8"))
        except (UnicodeDecodeError, ValueError) as error:
            raise ProtocolError("control frame is not valid JSON: %s" % safe_text(error))
        if not isinstance(message, dict):
            raise ProtocolError("control frame is not a JSON object")
        if not isinstance(message.get("type"), str):
            raise ProtocolError("control frame has no type")
        return message
    raise ProtocolError("unknown frame kind %d from the daemon" % kind)


class FrameDecoder:
    """Splits a byte stream into frames. The length is checked before anything is buffered
    for it, so a bad prefix fails at once."""

    def __init__(self):
        self.buffer = bytearray()

    def feed(self, data):
        self.buffer += data

    def next_frame(self):
        """Returns `(kind, payload)` for the next whole frame, or None if more bytes are needed."""
        if len(self.buffer) < 4:
            return None
        length = int.from_bytes(self.buffer[0:4], "big")
        if length == 0:
            raise ProtocolError("empty frame (length 0)")
        if length > MAX_FRAME_LEN:
            raise ProtocolError("frame length %d is over the 1 MiB cap" % length)
        if len(self.buffer) < 4 + length:
            return None
        kind = self.buffer[4]
        payload = bytes(self.buffer[5 : 4 + length])
        del self.buffer[: 4 + length]
        return kind, payload

    def pending(self):
        return len(self.buffer)


# ---- statistics ----


def percentile(sorted_values, p):
    """Nearest-rank percentile of an ascending list: the smallest value with at least p% of
    the values at or below it."""
    if not sorted_values:
        return None
    rank = max(1, int(math.ceil(p / 100.0 * len(sorted_values))))
    return sorted_values[min(rank, len(sorted_values)) - 1]


def summarize(latencies_ns):
    """p50, p95, p99, max and mean in ms, or None for each when there are no samples."""
    values = sorted(latencies_ns)
    if not values:
        return {"p50_ms": None, "p95_ms": None, "p99_ms": None, "max_ms": None, "mean_ms": None,
                "min_ms": None}

    def ms(ns):
        return round(ns / 1e6, 4)

    return {
        "p50_ms": ms(percentile(values, 50)),
        "p95_ms": ms(percentile(values, 95)),
        "p99_ms": ms(percentile(values, 99)),
        "max_ms": ms(values[-1]),
        "mean_ms": ms(sum(values) / len(values)),
        "min_ms": ms(values[0]),
    }


# ---- terminal choice ----


def parse_label(text):
    key, sep, value = text.partition("=")
    if not sep or not key:
        raise argparse.ArgumentTypeError("a label is key=value, got %r" % text)
    return key, value


def choose_terminal(terminals, terminal_id=None, label=None):
    """Picks the terminal to measure from a `terminals` snapshot's list.

    With `terminal_id`, that terminal; with `label` (a `(key, value)` pair), the one running
    terminal carrying it. Raises HarnessError when there is none, or more than one.
    """
    if terminal_id is not None:
        for info in terminals:
            if info.get("id") == terminal_id:
                if info.get("state") != "running":
                    raise HarnessError(
                        "terminal %s is %s, not running" % (terminal_id, safe_text(info.get("state")))
                    )
                return info
        raise HarnessError("the daemon lists no terminal %s" % terminal_id)
    key, value = label
    matches = [
        info
        for info in terminals
        if info.get("state") == "running" and (info.get("labels") or {}).get(key) == value
    ]
    if not matches:
        raise HarnessError("no running terminal has the label %s=%s" % (key, value))
    if len(matches) > 1:
        ids = ", ".join(safe_text(info.get("id")) for info in matches)
        raise HarnessError(
            "%d running terminals have the label %s=%s (%s); pick one with --terminal"
            % (len(matches), key, value, ids)
        )
    return matches[0]


# ---- the bridge ----


class Bridge:
    """The bridge process and its pipes. Reads and writes never block past a deadline."""

    def __init__(self, argv):
        self.argv = list(argv)
        try:
            self.proc = subprocess.Popen(
                self.argv,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
                close_fds=True,
            )
        except OSError as error:
            raise HarnessError("cannot start the bridge %s: %s" % (self.argv[0], error))
        self.out_fd = self.proc.stdout.fileno()
        self.in_fd = self.proc.stdin.fileno()
        os.set_blocking(self.out_fd, False)
        os.set_blocking(self.in_fd, False)
        self.decoder = FrameDecoder()
        self.eof = False
        self.stderr_tail = bytearray()
        self.stderr_lock = threading.Lock()
        self.stderr_thread = threading.Thread(target=self._drain_stderr, daemon=True)
        self.stderr_thread.start()
        self.bytes_in = 0
        self.bytes_out = 0

    def _drain_stderr(self):
        # Keeps the bridge's stderr pipe from filling, and keeps its last 8 KiB for messages.
        try:
            while True:
                chunk = self.proc.stderr.read(4096)
                if not chunk:
                    return
                with self.stderr_lock:
                    self.stderr_tail += chunk
                    if len(self.stderr_tail) > 8192:
                        del self.stderr_tail[:-8192]
        except (OSError, ValueError):
            return

    def stderr_text(self):
        with self.stderr_lock:
            text = bytes(self.stderr_tail).decode("utf-8", "replace")
        return safe_text(text, 512)

    def closed_message(self):
        code = self.proc.poll()
        text = "the bridge closed its output"
        if code is not None:
            text += " (exit code %d)" % code
        err = self.stderr_text()
        if err:
            text += "; its stderr: " + err
        return text

    def send(self, data, deadline):
        view = memoryview(data)
        while view:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise Timeout("the bridge stopped reading its input")
            try:
                _, writable, _ = select.select([], [self.in_fd], [], remaining)
            except InterruptedError:
                continue
            if not writable:
                continue
            try:
                written = os.write(self.in_fd, view)
            except BlockingIOError:
                continue
            except BrokenPipeError:
                raise HarnessError(self.closed_message())
            self.bytes_out += written
            view = view[written:]

    def read_message(self, deadline):
        """The next daemon message, or Timeout once `deadline` (time.monotonic) passes."""
        while True:
            got = self.decoder.next_frame()
            if got is not None:
                return parse_daemon_frame(*got)
            if self.eof:
                if self.decoder.pending():
                    raise ProtocolError(
                        "the bridge closed in the middle of a frame (%d bytes left); %s"
                        % (self.decoder.pending(), self.closed_message())
                    )
                raise HarnessError(self.closed_message())
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise Timeout("no frame from the bridge in time")
            try:
                readable, _, _ = select.select([self.out_fd], [], [], remaining)
            except InterruptedError:
                continue
            if not readable:
                continue
            try:
                chunk = os.read(self.out_fd, 1 << 16)
            except BlockingIOError:
                continue
            if not chunk:
                self.eof = True
                continue
            self.bytes_in += len(chunk)
            self.decoder.feed(chunk)

    def await_ready(self, marker, timeout_s):
        """Discards bridge output up to and including the line `marker`, before any frame.

        For a bridge that runs through a remote pty (`ssh -tt`), the remote side prints the
        marker once `stty raw -echo` has run, so the hello isn't echoed or cooked by a line
        discipline still in its default mode. Bytes after the marker go to the decoder."""
        want = marker.encode() + b"\n"
        seen = bytearray()
        deadline = time.monotonic() + timeout_s
        while True:
            at = seen.find(want)
            if at >= 0:
                rest = bytes(seen[at + len(want):])
                if rest:
                    self.bytes_in += len(rest)
                    self.decoder.feed(rest)
                return
            if self.eof:
                raise HarnessError("the bridge ended before printing %r; %s"
                                   % (marker, self.closed_message()))
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise Timeout("the bridge didn't print %r in time" % marker)
            readable, _, _ = select.select([self.out_fd], [], [], remaining)
            if not readable:
                continue
            try:
                chunk = os.read(self.out_fd, 1 << 16)
            except BlockingIOError:
                continue
            if not chunk:
                self.eof = True
                continue
            seen += chunk
            if len(seen) > 1 << 16:
                raise HarnessError("over 64 KiB of bridge output before %r" % marker)

    def close(self, timeout_s=5.0):
        """Closes stdin so the bridge ends, waits, then terminates and at last kills it."""
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        code = None
        for stop in (None, self.proc.terminate, self.proc.kill):
            if stop is not None:
                stop()
            try:
                code = self.proc.wait(timeout=timeout_s)
                break
            except subprocess.TimeoutExpired:
                continue
        self.stderr_thread.join(timeout=1.0)
        return code


# ---- the session ----


class Session:
    """One connection: handshake, terminal list, attach, samples, detach."""

    def __init__(self, bridge, args, log):
        self.bridge = bridge
        self.args = args
        self.log = log
        self.next_request = 1
        self.attachment = None
        self.last_cursor = None  # the newest cursor received, acked lazily
        self.acked_cursor = None
        self.ended = False  # the attachment's stream ended (gap, lost or closed)
        self.owner = None
        self.ownership_generation = None
        self.counts = {"resized": 0, "facts": 0, "resynced": 0, "other_data": 0, "terminals": 0}
        self.host_id = None
        self.terminal = None

    def request_id(self):
        value = self.next_request
        self.next_request += 1
        return value

    def send(self, message, timeout_s=None):
        timeout_s = self.args.io_timeout_s if timeout_s is None else timeout_s
        self.bridge.send(encode_control(message), time.monotonic() + timeout_s)

    def ack(self):
        if self.last_cursor is not None and self.last_cursor != self.acked_cursor:
            self.send({"type": "ack", "cursor": dict(self.last_cursor)})
            self.acked_cursor = self.last_cursor

    def fail_on(self, message):
        """Raises for the messages that end a measurement: error, gap, lost, closed."""
        kind = message["type"]
        if kind == "error":
            raise HarnessError(
                "the daemon sent error %s: %s"
                % (safe_text(message.get("code")), safe_text(message.get("message", "")))
            )
        if kind == "gap" and self.attachment is not None \
                and message.get("attachment") == self.attachment:
            self.ended = True
            raise HarnessError("the daemon ended the attachment with a gap")
        if kind == "lost" and self._ours(message):
            self.ended = True
            raise HarnessError("the backend lost the terminal (lost)")
        if kind == "closed" and self._ours(message):
            self.ended = True
            raise HarnessError(
                "the attachment closed (%s)" % safe_text(message.get("reason"))
            )

    def _ours(self, message):
        cursor = message.get("cursor") or {}
        return self.attachment is not None and cursor.get("attachment") == self.attachment

    def note(self, message):
        """Bookkeeping common to every message: cursors to ack, ownership, counters."""
        kind = message["type"]
        if kind in CURSOR_EVENTS and self._ours(message):
            self.last_cursor = message["cursor"]
        if kind == "ownership" and message.get("attachment") == self.attachment:
            self.owner = message.get("owner")
            self.ownership_generation = message.get("ownership_generation")
        if kind in ("resized", "facts", "resynced") and self._ours(message):
            self.counts[kind] += 1
        if kind == "terminals":
            self.counts["terminals"] += 1

    def wait_for(self, what, predicate, timeout_s, fatal=True):
        """Reads until `predicate(message)` holds; fails on error, gap, lost and closed first."""
        deadline = time.monotonic() + timeout_s
        while True:
            try:
                message = self.bridge.read_message(deadline)
            except Timeout:
                raise Timeout("timed out after %.1f s waiting for %s" % (timeout_s, what))
            self.note(message)
            if predicate(message):
                return message
            if fatal:
                self.fail_on(message)

    def handshake(self):
        self.send({"type": "hello", "protocol_version": PROTOCOL_VERSION},
                  self.args.connect_timeout_s)
        # The daemon's first frame is `welcome` or the error that refuses the hello.
        reply = self.wait_for("welcome", lambda m: True, self.args.connect_timeout_s)
        if reply["type"] not in ("welcome", "error"):
            raise ProtocolError(
                "expected welcome, got %s: is the bridge `combined connect`?"
                % safe_text(reply["type"])
            )
        if reply["type"] == "error":
            if reply.get("code") == "version_mismatch":
                raise HarnessError(
                    "protocol version mismatch: the harness speaks %d; daemon says: %s"
                    % (PROTOCOL_VERSION, safe_text(reply.get("message", "")))
                )
            raise HarnessError(
                "the daemon refused the handshake (%s): %s"
                % (safe_text(reply.get("code")), safe_text(reply.get("message", "")))
            )
        if reply.get("protocol_version") != PROTOCOL_VERSION:
            raise HarnessError(
                "protocol version mismatch: harness %d, daemon %s"
                % (PROTOCOL_VERSION, safe_text(reply.get("protocol_version")))
            )
        self.host_id = reply.get("host_id")
        self.log("connected: daemon host %s, protocol %d" % (safe_text(self.host_id),
                                                             PROTOCOL_VERSION))

    def pick_terminal(self):
        request = self.request_id()
        self.send({"type": "watch_terminals", "request": request})
        snapshot = self.wait_for(
            "the terminal list",
            lambda m: m["type"] == "terminals" and m.get("request") == request
            or m["type"] == "error" and m.get("request") == request,
            self.args.io_timeout_s,
        )
        if snapshot["type"] == "error":
            self.fail_on(snapshot)
        self.terminal = choose_terminal(
            snapshot.get("terminals") or [], self.args.terminal, self.args.label
        )
        self.log(
            "terminal %s: argv %s, %sx%s"
            % (
                safe_text(self.terminal.get("id")),
                safe_text(" ".join(self.terminal.get("argv") or [])),
                self.terminal.get("cols"),
                self.terminal.get("rows"),
            )
        )

    def attach(self):
        request = self.request_id()
        cols = self.args.cols or self.terminal.get("cols") or 80
        rows = self.args.rows or self.terminal.get("rows") or 24
        self.send({
            "type": "attach",
            "request": request,
            "terminal": self.terminal["id"],
            "options": {
                "cols": int(cols),
                "rows": int(rows),
                "display": DISPLAY,
                "observe": False,
                "history": self.args.history,
            },
        })
        reply = self.wait_for(
            "attached",
            lambda m: m.get("request") == request and m["type"] in ("attached", "error"),
            self.args.io_timeout_s,
        )
        if reply["type"] == "error":
            self.fail_on(reply)
        self.attachment = reply["attachment"]
        started = self.wait_for(
            "the attachment's started", lambda m: m["type"] == "started" and self._ours(m),
            self.args.io_timeout_s,
        )
        self.log(
            "attached %s: epoch %s, %sx%s, fidelity %s"
            % (self.attachment, started["cursor"].get("epoch"), started.get("cols"),
               started.get("rows"), safe_text(started.get("fidelity")))
        )
        self.ack()

    def take_input(self):
        """Asks for input ownership and focus, and waits until this attachment is the owner."""
        self.send({"type": "request_ownership", "attachment": self.attachment})
        self.send({"type": "focus", "attachment": self.attachment})
        if self.owner != self.attachment:
            self.wait_for(
                "input ownership",
                lambda m: m["type"] == "ownership" and m.get("attachment") == self.attachment
                and m.get("owner") == self.attachment,
                self.args.io_timeout_s,
            )
        self.log("owner of input, generation %s" % self.ownership_generation)

    def settle(self):
        """Reads and acks until nothing arrives for settle_ms (the first paint, any repaint)."""
        quiet_s = self.args.settle_ms / 1000.0
        limit = time.monotonic() + max(3.0, quiet_s * 10)
        while True:
            deadline = min(time.monotonic() + quiet_s, limit)
            try:
                message = self.bridge.read_message(deadline)
            except Timeout:
                break
            self.note(message)
            self.fail_on(message)
            self.ack()
            if time.monotonic() >= limit:
                break
        self.ack()

    def check_owner(self):
        if self.owner != self.attachment:
            raise HarnessError(
                "another attachment took input ownership (owner %s); stop the other client and "
                "run again" % safe_text(self.owner)
            )

    def idle(self, seconds):
        """Waits `seconds`, reading and acking whatever arrives meanwhile."""
        deadline = time.monotonic() + seconds
        while True:
            try:
                message = self.bridge.read_message(deadline)
            except Timeout:
                return
            self.note(message)
            self.fail_on(message)
            if message["type"] == "data":
                self.counts["other_data"] += 1
            self.check_owner()

    def sample(self, byte):
        """One keystroke: send `byte`, return the ns until its echo arrives, or None on timeout."""
        frame_bytes = encode_input(self.attachment, bytes([byte]))
        timeout_s = self.args.timeout_ms / 1000.0
        start = time.perf_counter_ns()
        deadline = time.monotonic() + timeout_s
        self.bridge.send(frame_bytes, deadline)
        while True:
            try:
                message = self.bridge.read_message(deadline)
            except Timeout:
                return None
            if message["type"] == "data" and self._ours(message) and byte in message["bytes"]:
                elapsed = time.perf_counter_ns() - start
                self.note(message)
                return elapsed
            self.note(message)
            self.fail_on(message)
            if message["type"] == "data":
                self.counts["other_data"] += 1
            self.check_owner()

    def detach(self):
        """Detaches and acks the final `closed`, so the daemon finishes the attachment."""
        if self.attachment is None:
            return
        self.ack()
        self.send({"type": "detach", "attachment": self.attachment})
        final = self.wait_for(
            "the final closed",
            lambda m: (m["type"] == "closed" and self._ours(m))
            or (m["type"] == "gap" and m.get("attachment") == self.attachment)
            or (m["type"] == "lost" and self._ours(m)),
            self.args.io_timeout_s,
        )
        if final["type"] == "closed":
            self.ack()
        self.attachment = None


# ---- running ----


def run(args, argv, log):
    started_at = datetime.datetime.now(datetime.timezone.utc)
    result = {
        "tool": "tools/latency/echo_latency.py",
        "protocol_version": PROTOCOL_VERSION,
        "timestamp": started_at.isoformat(timespec="seconds"),
        "bridge_argv": list(argv),
        "client_host": socket.gethostname(),
        "client_platform": platform.platform(),
        "python": platform.python_version(),
        "daemon_host_id": None,
        "terminal": None,
        "settings": {
            "samples": args.samples,
            "warmup": args.warmup,
            "interval_ms": args.interval_ms,
            "timeout_ms": args.timeout_ms,
            "alphabet": ALPHABET.decode("ascii"),
            "clock": "time.perf_counter_ns",
            "stops_at": "data frame read from the bridge's stdout, before any rendering",
        },
        "requested": args.samples,
        "completed": 0,
        "timed_out": 0,
        "stats": summarize([]),
        "events": {},
        "error": None,
        "ok": False,
    }
    latencies = []
    bridge = None
    session = None
    try:
        bridge = Bridge(argv)
        session = Session(bridge, args, log)
        if args.ready_line:
            bridge.await_ready(args.ready_line, args.connect_timeout_s)
        session.handshake()
        result["daemon_host_id"] = session.host_id
        session.pick_terminal()
        result["terminal"] = {
            "id": session.terminal.get("id"),
            "argv": session.terminal.get("argv"),
            "labels": session.terminal.get("labels"),
            "cols": session.terminal.get("cols"),
            "rows": session.terminal.get("rows"),
        }
        session.attach()
        session.take_input()
        session.settle()
        if args.interval_ms < DAEMON_FLUSH_MS:
            log(
                "note: the daemon flushes output at most once per %d ms, at once only after an "
                "idle %d ms; at --interval-ms %g most echoes wait for the next flush"
                % (DAEMON_FLUSH_MS, DAEMON_FLUSH_MS, args.interval_ms)
            )
        interval_s = args.interval_ms / 1000.0
        consecutive = 0
        total = args.warmup + args.samples
        index = 0
        for index in range(total):
            byte = ALPHABET[index % len(ALPHABET)]
            elapsed = session.sample(byte)
            measured = index >= args.warmup
            if elapsed is None:
                consecutive += 1
                if measured:
                    result["timed_out"] += 1
                if consecutive >= args.max_consecutive_timeouts:
                    raise HarnessError(
                        "%d samples in a row got no echo within %d ms; is the terminal running "
                        "tools/latency/echo.py?" % (consecutive, args.timeout_ms)
                    )
            else:
                consecutive = 0
                if measured:
                    latencies.append(elapsed)
            session.ack()
            if args.progress and measured and (index - args.warmup + 1) % args.progress == 0:
                log("  %d/%d samples" % (index - args.warmup + 1, args.samples))
            if interval_s > 0:
                session.idle(interval_s)
        session.ack()
        session.detach()
        result["ok"] = True
    except HarnessError as error:
        result["error"] = str(error)
    except KeyboardInterrupt:
        result["error"] = "interrupted"
    finally:
        if session is not None and session.attachment is not None:
            # Best effort: a failed run still detaches when the bridge is alive, and acks a
            # final `closed` or `lost` so the daemon finishes the attachment.
            try:
                if session.ended:
                    session.ack()
                else:
                    session.detach()
            except HarnessError:
                pass
        exit_code = None
        if bridge is not None:
            exit_code = bridge.close()
            if result["error"] and bridge.stderr_text() and "stderr" not in result["error"]:
                result["bridge_stderr"] = bridge.stderr_text()
        result["bridge_exit_code"] = exit_code
        if session is not None:
            result["events"] = dict(session.counts)
            if bridge is not None:
                result["events"]["bytes_from_bridge"] = bridge.bytes_in
                result["events"]["bytes_to_bridge"] = bridge.bytes_out
    result["completed"] = len(latencies)
    result["stats"] = summarize(latencies)
    result["finished"] = datetime.datetime.now(datetime.timezone.utc).isoformat(
        timespec="seconds"
    )
    if args.raw:
        result["latencies_ns"] = latencies
    return result


def format_report(result):
    stats = result["stats"]

    def ms(value):
        return "-" if value is None else "%.3f ms" % value

    lines = [
        "keystroke echo latency (%s)" % result["timestamp"],
        "  bridge:    %s" % " ".join(result["bridge_argv"]),
        "  client:    %s" % result["client_host"],
        "  daemon:    host %s" % (result["daemon_host_id"] or "-"),
        "  terminal:  %s" % ((result["terminal"] or {}).get("id") or "-"),
        "  samples:   requested %d, completed %d, timed out %d (interval %g ms, timeout %d ms)"
        % (result["requested"], result["completed"], result["timed_out"],
           result["settings"]["interval_ms"], result["settings"]["timeout_ms"]),
        "  p50 %s  p95 %s  p99 %s  max %s  mean %s"
        % (ms(stats["p50_ms"]), ms(stats["p95_ms"]), ms(stats["p99_ms"]), ms(stats["max_ms"]),
           ms(stats["mean_ms"])),
    ]
    events = result.get("events") or {}
    extra = ["%s %d" % (k, events[k]) for k in ("resized", "resynced", "other_data") if events.get(k)]
    if extra:
        lines.append("  during the run: " + ", ".join(extra))
    if result["error"]:
        lines.append("FAILED: " + result["error"])
        if result.get("bridge_stderr"):
            lines.append("  bridge stderr: " + result["bridge_stderr"])
    return "\n".join(lines)


def parse_args(argv):
    if "--" in argv:
        split = argv.index("--")
        own, bridge = argv[:split], argv[split + 1 :]
    else:
        own, bridge = argv, []
    parser = argparse.ArgumentParser(
        prog="echo_latency.py",
        description="Keystroke-echo latency through a combined daemon. The bridge command "
        "follows `--`, e.g. -- ssh -T -o BatchMode=yes mini /opt/homebrew/bin/combined connect",
    )
    pick = parser.add_mutually_exclusive_group(required=True)
    pick.add_argument("--terminal", help="the TermId to measure, e.g. hc:t3")
    pick.add_argument("--label", type=parse_label, metavar="KEY=VALUE",
                      help="measure the one running terminal carrying this label")
    parser.add_argument("--samples", type=int, default=2000)
    parser.add_argument("--interval-ms", type=float, default=3.0,
                        help="pause after each echo before the next keystroke (default 3)")
    parser.add_argument("--timeout-ms", type=int, default=1000, help="per sample (default 1000)")
    parser.add_argument("--warmup", type=int, default=10,
                        help="keystrokes sent and discarded before the samples (default 10)")
    parser.add_argument("--json", metavar="PATH", help="write the results here")
    parser.add_argument("--raw", action="store_true", help="include every latency in the JSON")
    parser.add_argument("--connect-timeout-ms", type=int, default=20000,
                        help="for the bridge to start and the handshake (default 20000)")
    parser.add_argument("--io-timeout-ms", type=int, default=10000,
                        help="for each other reply: terminal list, attach, ownership, detach")
    parser.add_argument("--settle-ms", type=int, default=300,
                        help="quiet time after attaching before sampling (default 300)")
    parser.add_argument("--max-consecutive-timeouts", type=int, default=10)
    parser.add_argument("--ready-line", metavar="MARKER",
                        help="before the handshake, skip bridge output up to a line MARKER "
                             "(for an `ssh -tt` bridge that runs `stty raw -echo` first)")
    parser.add_argument("--cols", type=int, help="attach size; default: the terminal's own")
    parser.add_argument("--rows", type=int)
    parser.add_argument("--history", type=int, default=0,
                        help="attach.options.history (default 0)")
    parser.add_argument("--progress", type=int, default=0, metavar="N",
                        help="log every N samples")
    parser.add_argument("--quiet", action="store_true", help="only the final report")
    args = parser.parse_args(own)
    if not bridge:
        parser.error("give the bridge command after --")
    if args.samples < 1:
        parser.error("--samples must be at least 1")
    if args.warmup < 0 or args.interval_ms < 0 or args.timeout_ms < 1:
        parser.error("--warmup and --interval-ms must be 0 or more, --timeout-ms at least 1")
    if args.max_consecutive_timeouts < 1:
        parser.error("--max-consecutive-timeouts must be at least 1")
    args.connect_timeout_s = args.connect_timeout_ms / 1000.0
    args.io_timeout_s = args.io_timeout_ms / 1000.0
    return args, bridge


def main(argv=None):
    args, bridge = parse_args(sys.argv[1:] if argv is None else argv)

    def log(text):
        if not args.quiet:
            print(text, file=sys.stderr, flush=True)

    result = run(args, bridge, log)
    print(format_report(result), flush=True)
    if args.json:
        try:
            with open(args.json, "w", encoding="utf-8") as out:
                json.dump(result, out, indent=2)
                out.write("\n")
        except OSError as error:
            print("cannot write %s: %s" % (args.json, error), file=sys.stderr)
            return 1
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
