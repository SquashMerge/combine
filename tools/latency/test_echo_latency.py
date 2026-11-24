#!/usr/bin/env python3
"""Pure tests for echo_latency.py: frame encoding against known bytes, decoding, the
percentiles and the terminal choice. Run with `python3 -I tools/latency/test_echo_latency.py`.
"""
import importlib.util
import os
import sys
import unittest

sys.dont_write_bytecode = True  # keep tools/latency free of __pycache__
_HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("echo_latency", os.path.join(_HERE, "echo_latency.py"))
el = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(el)


class Encoding(unittest.TestCase):
    def test_hello_frame(self):
        payload = b'{"type":"hello","protocol_version":4}'
        self.assertEqual(
            el.encode_control({"type": "hello", "protocol_version": 4}),
            (len(payload) + 1).to_bytes(4, "big") + b"\x01" + payload,
        )

    def test_ack_nests_the_cursor_object(self):
        frame = el.encode_control(
            {"type": "ack", "cursor": {"attachment": 7, "epoch": 1, "seq": 9}}
        )
        self.assertEqual(frame[4], 1)
        self.assertEqual(
            frame[5:], b'{"type":"ack","cursor":{"attachment":7,"epoch":1,"seq":9}}'
        )

    def test_input_frame(self):
        self.assertEqual(
            el.encode_input(0x0102030405060708, b"a"),
            b"\x00\x00\x00\x0a\x03\x01\x02\x03\x04\x05\x06\x07\x08a",
        )

    def test_frame_cap(self):
        el.frame(el.KIND_INPUT, b"x" * (el.MAX_FRAME_LEN - 1))
        with self.assertRaises(ValueError):
            el.frame(el.KIND_INPUT, b"x" * el.MAX_FRAME_LEN)

    def test_u64_bounds(self):
        with self.assertRaises(ValueError):
            el.u64(-1)
        with self.assertRaises(ValueError):
            el.u64(1 << 64)


class Decoding(unittest.TestCase):
    def data_frame(self, attachment, epoch, seq, body):
        payload = attachment.to_bytes(8, "big") + epoch.to_bytes(8, "big") + seq.to_bytes(8, "big")
        return el.frame(el.KIND_DATA, payload + body)

    def test_data_frame(self):
        decoder = el.FrameDecoder()
        decoder.feed(self.data_frame(5, 2, 300, b"hi"))
        kind, payload = decoder.next_frame()
        message = el.parse_daemon_frame(kind, payload)
        self.assertEqual(message["type"], "data")
        self.assertEqual(message["cursor"], {"attachment": 5, "epoch": 2, "seq": 300})
        self.assertEqual(message["bytes"], b"hi")
        self.assertIsNone(decoder.next_frame())

    def test_frames_split_across_reads(self):
        stream = el.encode_control({"type": "welcome", "protocol_version": 4, "host_id": "h"})
        stream += self.data_frame(1, 1, 1, b"x")
        decoder = el.FrameDecoder()
        got = []
        for byte in stream:
            decoder.feed(bytes([byte]))
            frame = decoder.next_frame()
            if frame is not None:
                got.append(el.parse_daemon_frame(*frame))
        self.assertEqual([m["type"] for m in got], ["welcome", "data"])
        self.assertEqual(got[0]["host_id"], "h")

    def test_bad_lengths(self):
        for prefix in (b"\x00\x00\x00\x00", (el.MAX_FRAME_LEN + 1).to_bytes(4, "big")):
            decoder = el.FrameDecoder()
            decoder.feed(prefix + b"\x01")
            with self.assertRaises(el.ProtocolError):
                decoder.next_frame()

    def test_bad_payloads(self):
        with self.assertRaises(el.ProtocolError):
            el.parse_daemon_frame(el.KIND_DATA, b"\x00" * 23)
        with self.assertRaises(el.ProtocolError):
            el.parse_daemon_frame(el.KIND_CONTROL, b"[1]")
        with self.assertRaises(el.ProtocolError):
            el.parse_daemon_frame(el.KIND_CONTROL, b"{nope")
        with self.assertRaises(el.ProtocolError):
            el.parse_daemon_frame(9, b"")


class Stats(unittest.TestCase):
    def test_nearest_rank(self):
        values = list(range(1, 101))
        self.assertEqual(el.percentile(values, 50), 50)
        self.assertEqual(el.percentile(values, 95), 95)
        self.assertEqual(el.percentile(values, 99), 99)
        self.assertEqual(el.percentile([7], 99), 7)
        self.assertIsNone(el.percentile([], 50))

    def test_summary_in_ms(self):
        stats = el.summarize([1_000_000, 3_000_000, 2_000_000])
        self.assertEqual(stats["p50_ms"], 2.0)
        self.assertEqual(stats["max_ms"], 3.0)
        self.assertEqual(stats["mean_ms"], 2.0)
        self.assertIsNone(el.summarize([])["p99_ms"])


class Choice(unittest.TestCase):
    terminals = [
        {"id": "hc:t1", "state": "running", "labels": {"role": "echo"}},
        {"id": "hc:t2", "state": "exited", "labels": {"role": "echo"}},
        {"id": "hc:t3", "state": "running", "labels": {"role": "shell"}},
    ]

    def test_by_label_skips_exited(self):
        self.assertEqual(el.choose_terminal(self.terminals, label=("role", "echo"))["id"], "hc:t1")

    def test_by_id(self):
        self.assertEqual(el.choose_terminal(self.terminals, terminal_id="hc:t3")["id"], "hc:t3")
        with self.assertRaises(el.HarnessError):
            el.choose_terminal(self.terminals, terminal_id="hc:t2")
        with self.assertRaises(el.HarnessError):
            el.choose_terminal(self.terminals, terminal_id="hc:t9")

    def test_label_must_match_one(self):
        with self.assertRaises(el.HarnessError):
            el.choose_terminal(self.terminals, label=("role", "none"))
        twice = self.terminals + [{"id": "hc:t4", "state": "running", "labels": {"role": "echo"}}]
        with self.assertRaises(el.HarnessError):
            el.choose_terminal(twice, label=("role", "echo"))


class Text(unittest.TestCase):
    def test_safe_text(self):
        self.assertEqual(el.safe_text("a\x1b[31m‮b \n c"), "a [31m b c")
        self.assertEqual(el.safe_text("x" * 300), "x" * 256 + "…")


class Args(unittest.TestCase):
    def test_bridge_after_double_dash(self):
        args, bridge = el.parse_args(["--label", "role=echo", "--", "ssh", "-T", "mini", "--x"])
        self.assertEqual(bridge, ["ssh", "-T", "mini", "--x"])
        self.assertEqual(args.label, ("role", "echo"))
        self.assertEqual((args.samples, args.interval_ms, args.timeout_ms), (2000, 3.0, 1000))


if __name__ == "__main__":
    unittest.main()
