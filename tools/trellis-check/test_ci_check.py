"""Tests for tools/trellis-check: which trellis failures CI retries, how often, and the summary
of a report (summary.py).

    python3 -I -B tools/trellis-check/test_ci_check.py

Each case runs the real ci-check.sh against a stand-in `trellis` that plays back a sequence of
answers (stdout, stderr, exit status), and a stand-in `sleep` that returns at once. The two
typed cold-start answers are trellis 0.2.2's own output, captured on a slowed cold daemon.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
CI_CHECK = HERE / "ci-check.sh"

spec = importlib.util.spec_from_file_location("classify", HERE / "classify.py")
classify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(classify)

summary_spec = importlib.util.spec_from_file_location("summary", HERE / "summary.py")
summary = importlib.util.module_from_spec(summary_spec)
summary_spec.loader.exec_module(summary)

# Captured from trellis 0.2.2 (`trellis rules check --format json`, the release binary CI
# installs) against a cold daemon of its own, slowed by pausing it with SIGSTOP/SIGCONT. Each
# had empty stdout and exit status 1.
# The first index outlasted the CLI's 90 s wait:
INDEX_NOT_READY = (
    b"trellis: indexing this checkout (0s elapsed, waiting up to 90s)\n"
    b"trellis: indexing this checkout (51s elapsed, waiting up to 90s)\n"
    b"trellis: indexing this checkout (88s elapsed, waiting up to 90s)\n"
    b"Error: index_not_ready: the first index of this checkout is still running; "
    b"indexing continues in the daemon, retry shortly\n"
)
# The daemon paused during the architecture build, and again past the CLI's 60 s reply bound;
# both answered the bare code:
DEADLINE_EXCEEDED = b"Error: deadline_exceeded\n"
# A checkout ten times this one's size, unpaused: a typed failure that is not retried.
BOUNDED_WORK = (
    b"trellis: indexing this checkout (0s elapsed, waiting up to 90s)\n"
    b"Error: bounded_work: architecture source\n"
)
# Not captured: the form trellis's source gives the architecture wait's answer
# (`ArchitectureBuilding`'s Display in src/http/architecture.rs).
DEADLINE_EXCEEDED_WITH_MESSAGE = (
    b"Error: deadline_exceeded: Architecture is being computed for this revision; retry shortly\n"
)

SUCCESS = (json.dumps({"catchingUp": False, "payload": {"counts": {"deviates": 0}}}), b"", 0)


def deviation_report(path: str) -> str:
    return json.dumps(
        {
            "catchingUp": False,
            "payload": {
                "counts": {"deviates": 1},
                "deviations": [{"source": path, "target": "crates/combined/src/adapters/fs/mod.rs"}],
            },
        }
    )


DEVIATION = (deviation_report("crates/combined/src/core/spaces.rs"), b"Error: architecture deviations\n", 1)
DEVIATION_MATCHING_PATH = (
    deviation_report("crates/combined/src/core/deadline_exceeded.rs"),
    b"Error: architecture deviations\n",
    1,
)
WRONG_CODE_IN_REPORT = (
    json.dumps(
        {
            "catchingUp": False,
            "error": {"code": "invalid_request", "message": "index_not_ready is not a rules key"},
        }
    ),
    b"Error: architecture evaluation failed\n",
    1,
)
WRONG_CODE_ON_STDERR = ("", b"Error: invalid_request: the index_not_ready flag is unknown\n", 1)
UNTYPED_STDERR = ("", b"Error: failed to open /tmp/deadline_exceeded/config\n", 1)
NOT_READY = ("", INDEX_NOT_READY, 1)
BUILDING = ("", DEADLINE_EXCEEDED, 1)
BUILDING_WITH_MESSAGE = ("", DEADLINE_EXCEEDED_WITH_MESSAGE, 1)
OTHER_TYPED_CODE = ("", BOUNDED_WORK, 1)

STUB_TRELLIS = """#!/bin/sh
n=$(cat "$STUB/count"); n=$((n + 1)); echo "$n" >"$STUB/count"
[ -f "$STUB/$n.code" ] || n=$(cat "$STUB/last")
cat "$STUB/$n.out"
cat "$STUB/$n.err" >&2
exit "$(cat "$STUB/$n.code")"
"""


def run_ci_check(answers: list[tuple[str, bytes, int]]) -> tuple[int, int, str]:
    """Runs ci-check.sh over `answers` (the last one repeats). Returns (status, calls, stderr)."""
    status, calls, _stdout, stderr = run_ci_check_output(answers)
    return status, calls, stderr


def run_ci_check_output(answers: list[tuple[str, bytes, int]]) -> tuple[int, int, str, str]:
    """[`run_ci_check`], with stdout too: (status, calls, stdout, stderr)."""
    with tempfile.TemporaryDirectory() as scratch:
        stub = Path(scratch) / "stub"
        bin_dir = Path(scratch) / "bin"
        stub.mkdir()
        bin_dir.mkdir()
        for index, (out, err, code) in enumerate(answers, start=1):
            (stub / f"{index}.out").write_text(out)
            (stub / f"{index}.err").write_bytes(err)
            (stub / f"{index}.code").write_text(str(code))
        (stub / "last").write_text(str(len(answers)))
        (stub / "count").write_text("0")
        (bin_dir / "trellis").write_text(STUB_TRELLIS)
        (bin_dir / "sleep").write_text("#!/bin/sh\nexit 0\n")
        for tool in ("trellis", "sleep"):
            (bin_dir / tool).chmod(0o755)
        env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}", STUB=str(stub), TMPDIR=scratch)
        done = subprocess.run(["sh", str(CI_CHECK)], cwd=scratch, env=env, capture_output=True, timeout=60)
        calls = int((stub / "count").read_text())
        leftovers = [p.name for p in Path(scratch).iterdir() if p.name.startswith("combine-trellis")]
        assert not leftovers, f"ci-check.sh left {leftovers}"
        return (
            done.returncode,
            calls,
            done.stdout.decode("utf-8", "replace"),
            done.stderr.decode("utf-8", "replace"),
        )


class RetryLoop(unittest.TestCase):
    def check(self, answers, status, calls):
        got_status, got_calls, stderr = run_ci_check(answers)
        self.assertEqual((got_status, got_calls), (status, calls), stderr)
        return stderr

    def test_success_is_one_call(self):
        self.check([SUCCESS], 0, 1)

    def test_index_not_ready_then_success_passes(self):
        stderr = self.check([NOT_READY, SUCCESS], 0, 2)
        self.assertIn("trellis answered index_not_ready (attempt 1 of 4)", stderr)

    def test_deadline_exceeded_then_success_passes(self):
        stderr = self.check([BUILDING, SUCCESS], 0, 2)
        self.assertIn("trellis answered deadline_exceeded (attempt 1 of 4)", stderr)

    def test_both_codes_then_success_passes(self):
        self.check([NOT_READY, BUILDING, BUILDING_WITH_MESSAGE, SUCCESS], 0, 4)

    def test_persistent_index_not_ready_stops_at_four(self):
        stderr = self.check([NOT_READY], 1, 4)
        self.assertIn("still building after 4 attempts", stderr)

    def test_persistent_deadline_exceeded_stops_at_four(self):
        self.check([BUILDING], 1, 4)

    def test_another_typed_code_fails_at_once(self):
        self.check([OTHER_TYPED_CODE, SUCCESS], 1, 1)

    def test_deviation_fails_at_once(self):
        self.check([DEVIATION, SUCCESS], 1, 1)

    def test_deviation_with_a_matching_path_fails_at_once(self):
        self.check([DEVIATION_MATCHING_PATH, SUCCESS], 1, 1)

    def test_wrong_code_with_a_matching_message_fails_at_once(self):
        self.check([WRONG_CODE_IN_REPORT, SUCCESS], 1, 1)
        self.check([WRONG_CODE_ON_STDERR, SUCCESS], 1, 1)

    def test_untyped_stderr_fails_at_once(self):
        self.check([UNTYPED_STDERR, SUCCESS], 1, 1)

    def test_malformed_output_fails_at_once(self):
        malformed = [
            ("{not json", DEADLINE_EXCEEDED, 1),
            ("", b"Error: deadline_exceeded\xff\n", 1),
            ("", b"warning: something else\n" + DEADLINE_EXCEEDED, 1),
            ("", DEADLINE_EXCEEDED + b"Error: deadline_exceeded\n", 1),
            ("", b"", 1),
            ("", DEADLINE_EXCEEDED, 101),
        ]
        for answer in malformed:
            with self.subTest(answer=answer):
                self.check([answer, SUCCESS], answer[2], 1)

    def test_a_retry_after_a_non_retryable_answer_never_happens(self):
        self.check([NOT_READY, DEVIATION, SUCCESS], 1, 2)


class Classify(unittest.TestCase):
    def code(self, status, stdout, stderr):
        return classify.classify(status, stdout, stderr)[0]

    def test_the_captured_answers_are_retried(self):
        self.assertEqual(self.code(1, b"", INDEX_NOT_READY), "index_not_ready")
        self.assertEqual(self.code(1, b"", DEADLINE_EXCEEDED), "deadline_exceeded")
        self.assertEqual(self.code(1, b"", DEADLINE_EXCEEDED_WITH_MESSAGE), "deadline_exceeded")
        self.assertIsNone(self.code(1, b"", BOUNDED_WORK))

    def test_a_bare_code_is_retried(self):
        self.assertEqual(self.code(1, b"\n", b"Error: index_not_ready\n\n"), "index_not_ready")

    def test_near_misses_are_not_retried(self):
        for stderr in (
            b"Error: index_not_ready_soon: no\n",
            b"Error: rules: deadline_exceeded: wrapped\n",
            b"error: deadline_exceeded\n",
            b"Error:  deadline_exceeded\n",
            b"Error: deadline_exceeded:no space\n",
            b"Error: deadline_exceeded\nCaused by: something\n",
        ):
            with self.subTest(stderr=stderr):
                self.assertIsNone(self.code(1, b"", stderr))

    def test_any_stdout_is_not_retried(self):
        self.assertIsNone(self.code(1, b"{}", DEADLINE_EXCEEDED))

    def test_success_is_not_a_retry(self):
        self.assertIsNone(self.code(0, b"", DEADLINE_EXCEEDED))

    def test_usage_errors_fail(self):
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(classify.main(["classify.py"]), 2)
            self.assertEqual(classify.main(["classify.py", "x", "a", "b"]), 2)


# The shape of trellis 0.2.2's report, cut down: the fields summary.py reads.
FULL_REPORT = {
    "catchingUp": False,
    "payload": {
        "counts": {"comparable": 9, "conforms": 6, "deviates": 1, "excepted": 0, "unknown": 2},
        "testCounts": {"comparable": 5, "conforms": 4, "deviates": 0, "excepted": 0, "unknown": 1},
        "unassigned": [
            {"language": "typescript", "path": "app/web/vite.config.ts", "sid": "sid:v1:b"},
            {"language": "rust", "path": "app/src-tauri/build.rs", "sid": "sid:v1:a"},
            {"kind": "externalPackage", "language": "javascript", "package": "@xterm/headless"},
        ],
        "unassignedFileCount": 2,
    },
}
FULL_SUMMARY = [
    "trellis: production: 6 conform, 1 deviate, 0 excepted, 2 unknown",
    "trellis: test only: 4 conform, 0 deviate, 0 excepted, 1 unknown",
    "trellis: 2 unassigned files, 1 unassigned external packages",
    "  app/src-tauri/build.rs",
    "  app/web/vite.config.ts",
]


class Summary(unittest.TestCase):
    def test_counts_and_unassigned_files(self):
        self.assertEqual(summary.summarize(FULL_REPORT), FULL_SUMMARY)

    def test_missing_parts_count_as_none(self):
        self.assertEqual(
            summary.summarize({"payload": {"counts": {"deviates": 0}}}),
            [
                "trellis: production: 0 conform, 0 deviate, 0 excepted, 0 unknown",
                "trellis: 0 unassigned files, 0 unassigned external packages",
            ],
        )

    def test_a_file_without_a_report_is_refused(self):
        for text in ("", "{not json", json.dumps({"error": {"code": "invalid_request"}}), "[]"):
            with self.subTest(text=text), tempfile.TemporaryDirectory() as scratch:
                path = Path(scratch) / "report.json"
                path.write_text(text)
                err = io.StringIO()
                with contextlib.redirect_stderr(err):
                    self.assertEqual(summary.main(["summary.py", str(path)]), 1)
                self.assertIn("trellis: no summary:", err.getvalue())

    def test_ci_check_prints_the_summary_on_success(self):
        status, calls, stdout, _ = run_ci_check_output([(json.dumps(FULL_REPORT), b"", 0)])
        self.assertEqual((status, calls), (0, 1))
        self.assertEqual(stdout.splitlines(), FULL_SUMMARY)

    def test_ci_check_prints_the_summary_of_a_deviation_and_still_fails(self):
        answer = (json.dumps(FULL_REPORT), b"Error: architecture deviations\n", 1)
        status, calls, stdout, _ = run_ci_check_output([answer, SUCCESS])
        self.assertEqual((status, calls), (1, 1))
        self.assertEqual(stdout.splitlines(), FULL_SUMMARY)

    def test_the_summary_never_changes_the_status(self):
        # SUCCESS has no testCounts or unassigned list; an error report has no counts at all.
        self.check_status([SUCCESS], 0)
        self.check_status([WRONG_CODE_IN_REPORT, SUCCESS], 1)

    def check_status(self, answers, status):
        got, _, _, stderr = run_ci_check_output(answers)
        self.assertEqual(got, status, stderr)


if __name__ == "__main__":
    result = unittest.main(exit=False, verbosity=1).result
    sys.exit(0 if result.wasSuccessful() else 1)
