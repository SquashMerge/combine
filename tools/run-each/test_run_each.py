"""Tests for tools/run-each.sh, the sequential runner the factory's evidence entries use.

    python3 -I -B tools/run-each/test_run_each.py

Each case runs the real script in a scratch directory. A helper command logs the arguments it
got as JSON and exits with the status its first argument names, so a case can tell which groups
ran, in what order, and with which words.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "run-each.sh"

HELPER = """\
import json, os, sys
with open(os.environ["RUN_EACH_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[2:]) + "\\n")
sys.exit(int(sys.argv[1]))
"""

USAGE = "usage: sh tools/run-each.sh CMD [ARG...] [-- CMD [ARG...]]..."


class RunEachTest(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.dir = Path(scratch.name)
        self.log = self.dir / "log"
        self.helper = self.dir / "helper.py"
        self.helper.write_text(HELPER)
        # Files a glob would match, and a variable a shell would expand.
        (self.dir / "one.txt").write_text("")
        (self.dir / "two.txt").write_text("")

    def run_each(self, *args):
        env = dict(os.environ, RUN_EACH_LOG=str(self.log), HOME="/home/expanded")
        return subprocess.run(
            ["sh", str(SCRIPT), *args],
            cwd=self.dir,
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def call(self, status, *words):
        return [sys.executable, "-I", "-B", str(self.helper), str(status), *words]

    def logged(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_groups_run_in_order(self):
        result = self.run_each(
            *self.call(0, "first"), "--", *self.call(0, "second"), "--", *self.call(0, "third")
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.logged(), [["first"], ["second"], ["third"]])

    def test_arguments_arrive_unchanged(self):
        words = ["a b", "*", "$HOME", "it's", 'say "hi"', "x;y", "", "`id`", "-n", "*.txt"]
        result = self.run_each(*self.call(0, *words), "--", *self.call(0, ""))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.logged(), [words, [""]])

    def test_a_single_group_without_a_separator_runs(self):
        result = self.run_each(*self.call(0, "only"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.logged(), [["only"]])

    def test_the_first_failure_stops_the_run_and_sets_the_status(self):
        result = self.run_each(
            *self.call(0, "a"), "--", *self.call(3, "b"), "--", *self.call(5, "c")
        )
        self.assertEqual(result.returncode, 3)
        self.assertEqual(self.logged(), [["a"], ["b"]])

    def test_a_missing_command_gives_127_and_stops_the_run(self):
        result = self.run_each(
            *self.call(0, "a"), "--", "no-such-command-run-each", "--", *self.call(0, "c")
        )
        self.assertEqual(result.returncode, 127)
        self.assertEqual(self.logged(), [["a"]])

    def test_a_failure_in_the_last_group_is_reported(self):
        result = self.run_each(*self.call(0, "a"), "--", *self.call(9, "b"))
        self.assertEqual(result.returncode, 9)
        self.assertEqual(self.logged(), [["a"], ["b"]])

    def test_each_group_is_printed_and_a_failing_one_names_its_status(self):
        result = self.run_each(*self.call(0, "a b"), "--", *self.call(4, "c"))
        self.assertIn("run-each: + " + " ".join(self.call(0, "a b")), result.stderr)
        self.assertIn("run-each: + " + " ".join(self.call(4, "c")), result.stderr)
        self.assertIn("failed with status 4: " + " ".join(self.call(4, "c")), result.stderr)
        self.assertNotIn("failed with status 0", result.stderr)

    def test_a_malformed_list_gives_usage_and_runs_nothing(self):
        cases = {
            "no arguments": [],
            "a leading separator": ["--", *self.call(0, "a")],
            "a trailing separator": [*self.call(0, "a"), "--"],
            "an empty group": [*self.call(0, "a"), "--", "--", *self.call(0, "b")],
            "only separators": ["--", "--"],
            "one separator": ["--"],
        }
        for name, args in cases.items():
            with self.subTest(name):
                result = self.run_each(*args)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertEqual(result.stderr.strip(), USAGE)
                self.assertEqual(self.logged(), [])


if __name__ == "__main__":
    unittest.main()
