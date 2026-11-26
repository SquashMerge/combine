"""Tests for tools/ci_verify.sh, the factory's verify on GitHub Actions, and its evaluate.py.

    python3 -I -B tools/ci-verify/test_ci_verify.py

Each case runs the real ci_verify.sh with stand-in `git` and `gh` first on PATH. They replay a
scripted scenario and log every call; nothing reaches GitHub. `sleep` and `date` are stand-ins
too: sleeping advances a fake clock that `date +%s` reads, so the deadline and start-window
cases run at once and check the time the script believed had passed. A scenario's `cost` makes
each push and gh call take that many fake seconds, or the bound the script gave it
(CI_VERIFY_LIMIT_S) if that is less, when it is "killed" with status 143.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE.parent / "ci_verify.sh"

spec = importlib.util.spec_from_file_location("evaluate", HERE / "evaluate.py")
evaluate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluate)

SHA = "0123456789abcdef0123456789abcdef01234567"
REF = f"refs/heads/qualify/factory-{SHA}"
REMOTE = "git@github.com:SquashMerge/combine.git"
REPO = "SquashMerge/combine"
CLOCK_START = 1_000_000
SECRET = "s3cr3t-dummy-credential"
SECRET_REMOTE = f"https://x-access-token:{SECRET}@github.com/SquashMerge/combine.git"
# Check suites: a push of SHA started one; a pull_request run on SHA's PR started the other.
PUSH_SUITE, PR_SUITE = 7001, 7002
WORKFLOWS = {
    "pages": [
        {
            "total_count": 3,
            "workflow_runs": [
                {"id": 1, "event": "push", "head_sha": SHA, "check_suite_id": PUSH_SUITE},
                {"id": 2, "event": "pull_request", "head_sha": SHA, "check_suite_id": PR_SUITE},
            ],
        }
    ]
}

# The stand-ins. Each reads $STUB/scenario.json and appends its argv to $STUB/calls.jsonl.
STUB_COMMON = r"""
import json, os, sys
stub = os.environ["STUB"]
scenario = json.load(open(os.path.join(stub, "scenario.json")))
def clock():
    return int(open(os.path.join(stub, "clock")).read())
def log(tool):
    with open(os.path.join(stub, "calls.jsonl"), "a") as f:
        f.write(json.dumps({"tool": tool, "argv": sys.argv[1:], "clock": clock()}) + "\n")
def bump(name):
    path = os.path.join(stub, "count-" + name)
    n = int(open(path).read()) if os.path.exists(path) else 0
    open(path, "w").write(str(n + 1))
    return n
def spend():
    cost = scenario.get("cost", 0)
    if not cost:
        return
    limit = int(os.environ.get("CI_VERIFY_LIMIT_S") or 0)
    took = min(cost, limit) if limit else cost
    later = clock() + took
    open(os.path.join(stub, "clock"), "w").write(str(later))
    if limit and cost > limit:
        sys.exit(143)
def answer(item):
    sys.stdout.write(item.get("out", ""))
    sys.stderr.write(item.get("err", ""))
    if item.get("hang"):
        open(os.path.join(stub, "hang-pid"), "w").write(str(os.getpid()))
    if item.get("hang") == "ignore-term":
        import signal, time
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        time.sleep(60)
    if item.get("hang"):
        os.execv("/bin/sleep", ["sleep", "30"])
    sys.exit(item.get("code", 0))
"""

STUB_GIT = STUB_COMMON + r"""
log("git")
args = sys.argv[1:]
if args[:3] == ["rev-parse", "--verify", "HEAD"]:
    answer({"out": scenario.get("sha", "") + "\n", "code": scenario.get("rev_parse_code", 0)})
if args[:4] == ["remote", "get-url", "--push", "origin"]:
    remote = scenario.get("remote")
    answer({"out": remote + "\n"} if remote is not None else {"code": 2, "err": "error: No such remote\n"})
if args[:1] == ["push"]:
    spend()
    refusal = f"fatal: unable to access '{scenario.get('remote')}/': The requested URL returned error: 403\n"
    if "--delete" in args:
        code = scenario.get("delete_code", 0)
        answer({"code": code, "err": refusal if code else ""})
    codes = scenario.get("push_codes", [0])
    n = bump("push")
    code = codes[min(n, len(codes) - 1)]
    answer({"code": code, "err": refusal if code else ""})
answer({"code": 99, "err": "stub git: unexpected call\n"})
"""

STUB_GH = STUB_COMMON + r"""
log("gh")
args = sys.argv[1:]
path = next((a for a in args[1:] if not a.startswith("-")), "")
if args[:1] != ["api"]:
    answer({"code": 99, "err": "stub gh: unexpected call\n"})
spend()
if "/annotations" in path:
    answer({"code": 98, "err": "stub gh: annotations are never read\n"})
def pages(item):
    if "pages" in item:
        answer({"out": "".join(json.dumps(p) for p in item["pages"])})
    answer(item)
if "/actions/runs?" in path:
    pages(scenario["workflows"])
if "/check-runs" in path:
    polls = scenario["polls"]
    n = bump("poll")
    pages(polls[min(n, len(polls) - 1)])
answer(scenario.get("probe", {"out": "{}"}))
"""

STUB_SLEEP = '#!/bin/sh\nn=$(cat "$STUB/clock"); echo $((n + $1)) >"$STUB/clock"\n'
STUB_DATE = '#!/bin/sh\ncat "$STUB/clock"\n'


def check(run_id, conclusion="success", status="completed", name="verify", app="github-actions", suite=PUSH_SUITE):
    return {
        "id": run_id,
        "name": name,
        "status": status,
        "conclusion": conclusion if status == "completed" else None,
        "app": {"slug": app},
        "check_suite": {"id": suite},
        "html_url": f"https://github.com/{REPO}/runs/{run_id}",
        "details_url": f"https://github.com/{REPO}/actions/runs/9{run_id}/job/{run_id}",
    }


def page(*runs):
    return {"total_count": len(runs), "check_runs": list(runs)}


def poll(*runs):
    return {"pages": [page(*runs)]}


NOTHING = poll()
# What GitHub answers for a commit it has never seen, before the first push.
UNKNOWN_COMMIT = {"code": 1, "err": f"gh: No commit found for SHA: {SHA} (HTTP 422)\n"}
PENDING = poll(check(100, status="in_progress"))
QUEUED = poll(check(100, status="queued"))
PASSED = poll(check(100))
FAILED = poll(check(100, "failure"))
CANCELLED = poll(check(100, "cancelled"))
UNREACHABLE = {"code": 1, "err": "error connecting to api.github.com\n"}


class Result:
    def __init__(self, status, stderr, calls, clock):
        self.status, self.stderr, self.calls, self.clock = status, stderr, calls, clock

    def git(self):
        return [c["argv"] for c in self.calls if c["tool"] == "git"]

    def gh(self):
        return [c["argv"] for c in self.calls if c["tool"] == "gh"]

    def pushes(self):
        return [a for a in self.git() if a[:1] == ["push"] and "--delete" not in a]

    def deletes(self):
        return [a for a in self.git() if a[:1] == ["push"] and "--delete" in a]

    def polls(self):
        return [a for a in self.gh() if "/check-runs?" in " ".join(a)]


def run_script(scenario, env=None, fake_time=True, on_start=None):
    scenario = dict({"sha": SHA, "remote": REMOTE, "workflows": WORKFLOWS}, **scenario)
    with tempfile.TemporaryDirectory() as scratch:
        stub = Path(scratch) / "stub"
        bin_dir = Path(scratch) / "bin"
        stub.mkdir()
        bin_dir.mkdir()
        (stub / "scenario.json").write_text(json.dumps(scenario))
        (stub / "clock").write_text(str(CLOCK_START))
        (stub / "calls.jsonl").write_text("")
        tools = {
            "git": f"#!{sys.executable} -I\n{STUB_GIT}",
            "gh": f"#!{sys.executable} -I\n{STUB_GH}",
        }
        if fake_time:
            tools.update(sleep=STUB_SLEEP, date=STUB_DATE)
        for tool, text in tools.items():
            (bin_dir / tool).write_text(text)
            (bin_dir / tool).chmod(0o755)
        base = {
            "CI_VERIFY_CALL_TIMEOUT_S": "0",
            "CI_VERIFY_DEADLINE_S": "4800",
            "CI_VERIFY_START_S": "600",
            "CI_VERIFY_POLL_S": "100",
        }
        full_env = {k: v for k, v in os.environ.items() if not k.startswith("CI_VERIFY_")}
        full_env.update(base, **(env or {}))
        full_env.update(PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}", STUB=str(stub), TMPDIR=scratch)
        proc = subprocess.Popen(
            ["sh", str(SCRIPT)],
            cwd=scratch,
            env=full_env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if on_start:
            on_start(proc, stub)
        try:
            stdout, stderr = proc.communicate(timeout=60)
        except subprocess.TimeoutExpired:
            proc.kill()
            raise
        calls = [json.loads(line) for line in (stub / "calls.jsonl").read_text().splitlines()]
        output = stdout.decode("utf-8", "replace") + stderr.decode("utf-8", "replace")
        leftovers = [p.name for p in Path(scratch).iterdir() if p.name.startswith("ci-verify.")]
        assert not leftovers, f"ci_verify.sh left {leftovers}:\n{output}"
        clock = int((stub / "clock").read_text()) - CLOCK_START
        return Result(proc.returncode, output, calls, clock)


class CiVerify(unittest.TestCase):
    def go(self, scenario, status, env=None, **kwargs):
        result = run_script(scenario, env, **kwargs)
        self.assertEqual(result.status, status, result.stderr)
        return result

    def assert_cleaned_up(self, result):
        """The qualify branch was deleted as the script's last act."""
        self.assertTrue(result.git(), result.stderr)
        self.assertEqual(result.git()[-1], ["push", "--quiet", "origin", "--delete", REF], result.stderr)

    # -- passes and failures

    def test_required_check_passing_gives_0(self):
        result = self.go({"polls": [UNKNOWN_COMMIT, PENDING, PASSED]}, 0)
        self.assertNotIn("did not answer", result.stderr, "an unknown commit is not a failed poll")
        self.assertEqual(result.pushes(), [["push", "--quiet", "origin", f"{SHA}:{REF}"]])
        self.assertTrue(all(f"repos/{REPO}/commits/{SHA}/check-runs" in " ".join(p) for p in result.polls()))
        self.assertIn(f"pushed {SHA} to qualify/factory-{SHA}", result.stderr)
        self.assertIn(f"https://github.com/{REPO}/actions/runs/9100/job/100", result.stderr)
        self.assert_cleaned_up(result)

    def test_every_required_check_must_pass(self):
        both = poll(check(100), check(101, name="lint"))
        result = self.go({"polls": [NOTHING, both]}, 0, {"CI_VERIFY_REQUIRED": "verify lint"})
        self.assert_cleaned_up(result)

    def test_required_check_failing_gives_1(self):
        result = self.go({"polls": [NOTHING, PENDING, FAILED]}, 1)
        self.assertIn("verify: failed:failure", result.stderr)
        self.assert_cleaned_up(result)

    def test_one_failure_among_required_checks_gives_1(self):
        mixed = poll(check(100), check(101, "failure", name="lint"))
        result = self.go({"polls": [NOTHING, mixed]}, 1, {"CI_VERIFY_REQUIRED": "verify lint"})
        self.assert_cleaned_up(result)

    def test_a_failure_beside_a_platform_stop_gives_1(self):
        mixed = poll(check(100, "failure"), check(101, "cancelled", name="lint"))
        result = self.go({"polls": [NOTHING, mixed]}, 1, {"CI_VERIFY_REQUIRED": "verify lint"})
        self.assertEqual(len(result.pushes()), 1, "no fresh run for a real failure")

    def test_timed_out_and_skipped_are_failures(self):
        for conclusion in ("timed_out", "skipped", "neutral"):
            with self.subTest(conclusion=conclusion):
                self.go({"polls": [NOTHING, poll(check(100, conclusion))]}, 1)

    # -- missing checks, deadlines

    def test_check_missing_until_the_deadline_gives_75(self):
        # verify passes, but the second required check never appears.
        result = self.go(
            {"polls": [NOTHING, PASSED]},
            75,
            {"CI_VERIFY_REQUIRED": "verify lint", "CI_VERIFY_START_S": "99999", "CI_VERIFY_DEADLINE_S": "600"},
        )
        self.assertIn("lint: missing", result.stderr)
        self.assertIn("no answer for", result.stderr)
        self.assertEqual(result.clock, 300, "no work starts in the last CI_VERIFY_RESERVE_S")
        self.assert_cleaned_up(result)

    def test_nothing_started_after_the_push_gives_75(self):
        result = self.go({"polls": [NOTHING]}, 75, {"CI_VERIFY_START_S": "300"})
        self.assertEqual(len(result.pushes()), 1, "a missing check is never pushed for twice")
        self.assertIn("had not started 5m00s after the push", result.stderr)
        self.assertLess(result.clock, 400)
        self.assert_cleaned_up(result)

    def test_a_run_still_going_at_the_deadline_gives_75(self):
        result = self.go(
            {"polls": [NOTHING, QUEUED, PENDING]}, 75, {"CI_VERIFY_DEADLINE_S": "4800", "CI_VERIFY_POLL_S": "700"}
        )
        self.assertEqual(result.clock, 4500, "the script never sleeps into the cleanup reserve")
        self.assert_cleaned_up(result)

    def test_the_default_deadline_is_80_minutes(self):
        # An empty setting falls back to the default.
        result = self.go({"polls": [NOTHING, PENDING]}, 75, {"CI_VERIFY_DEADLINE_S": "", "CI_VERIFY_POLL_S": "1000"})
        self.assertEqual(result.clock, 4500)
        self.assertIn("gives up by 80m00s, cleanup included", result.stderr)

    def test_a_check_from_another_app_does_not_count(self):
        impostor = poll(check(100, app="some-other-app"))
        result = self.go({"polls": [NOTHING, impostor]}, 75, {"CI_VERIFY_START_S": "300"})
        self.assertIn("verify: missing", result.stderr)

    # -- newest run per name

    def test_a_newer_pass_supersedes_an_older_failure(self):
        # Across two pages, newest first, as GitHub may list them.
        runs = {"pages": [page(check(200)), page(check(100, "failure"))]}
        result = self.go({"polls": [runs]}, 0)
        self.assertEqual(result.pushes(), [], "a passed run on this sha is reused without a push")
        self.assert_cleaned_up(result)

    def test_a_newer_failure_supersedes_an_older_pass(self):
        runs = {"pages": [page(check(100)), page(check(200, "failure"))]}
        self.go({"polls": [runs]}, 1)

    def test_a_newer_run_still_going_is_waited_on(self):
        runs = poll(check(100), check(200, status="in_progress"))
        later = poll(check(100), check(200, "failure"))
        result = self.go({"polls": [runs, later]}, 1)
        self.assertEqual(result.pushes(), [], "a run still going on this sha is waited on without a push")

    # -- the platform stopping a run

    def test_a_cancelled_run_gets_one_fresh_run(self):
        for conclusion in ("cancelled", "startup_failure", "stale"):
            with self.subTest(conclusion=conclusion):
                stopped = poll(check(100, conclusion))
                rerun = poll(check(100, conclusion), check(300))
                result = self.go({"polls": [NOTHING, stopped, stopped, rerun]}, 0)
                self.assertEqual(len(result.pushes()), 2)
                # Deleted and pushed again: each push is preceded by a delete.
                pushes = [a for a in result.git() if a[:1] == ["push"]]
                self.assertEqual([("--delete" in a) for a in pushes], [True, False, True, False, True])
                self.assertIn("starting a fresh one (1 of 2)", result.stderr)

    def test_a_cancelled_run_found_before_any_push_gets_a_fresh_run(self):
        stopped = poll(check(100, "cancelled"))
        result = self.go({"polls": [stopped, poll(check(100, "cancelled"), check(300))]}, 0)
        self.assertEqual(len(result.pushes()), 1)

    def test_runs_older_than_the_stopped_one_never_count_again(self):
        # After the fresh push, only the stopped run (and an older pass) is listed: still missing.
        listing = poll(check(50), check(100, "cancelled"))
        result = self.go({"polls": [listing]}, 75, {"CI_VERIFY_START_S": "300"})
        self.assertEqual(len(result.pushes()), 1)
        self.assertIn("had not started", result.stderr)

    def test_repeated_cancellation_gives_75(self):
        polls = [NOTHING, poll(check(100, "cancelled")), poll(check(200, "cancelled")), poll(check(300, "cancelled"))]
        result = self.go({"polls": polls}, 75)
        self.assertEqual(len(result.pushes()), 3, "the first push and two fresh runs")
        self.assertIn("2 fresh runs did not fix it", result.stderr)
        self.assert_cleaned_up(result)

    def test_no_fresh_runs_when_disabled(self):
        result = self.go({"polls": [NOTHING, poll(check(100, "startup_failure"))]}, 75, {"CI_VERIFY_MAX_RERUNS": "0"})
        self.assertEqual(len(result.pushes()), 1)

    def test_failure_text_never_makes_a_platform_stop(self):
        # A failing test can print any text, GitHub's runner messages included.
        failed = check(100, "failure")
        failed["output"] = {"title": "lost communication with the server", "summary": "not acquired by Runner"}
        result = self.go({"polls": [NOTHING, poll(failed), poll(failed, check(300))]}, 1)
        self.assertEqual(len(result.pushes()), 1)
        self.assertFalse(any("annotations" in " ".join(a) for a in result.gh()))

    # -- only push runs of this sha count

    def test_a_pull_request_pass_on_this_sha_does_not_count(self):
        pr_pass = check(100, suite=PR_SUITE)
        result = self.go({"polls": [poll(pr_pass), poll(pr_pass), poll(pr_pass, check(200))]}, 0)
        self.assertEqual(len(result.pushes()), 1, "a pull_request run never stands in for a push run")
        self.assertIn("verify: missing", result.stderr)
        self.assertIn("job/200", result.stderr)

    def test_a_pull_request_failure_on_this_sha_does_not_count(self):
        pr_fail = check(300, "failure", suite=PR_SUITE)
        result = self.go({"polls": [poll(pr_fail), poll(pr_fail, check(200))]}, 0)
        self.assertEqual(len(result.pushes()), 1)

    def test_a_push_pass_on_this_sha_counts_without_a_push(self):
        # A push run's answer stands, whatever a newer pull_request run on the same sha says.
        result = self.go({"polls": [poll(check(100, "failure", suite=PR_SUITE), check(90))]}, 0)
        self.assertEqual(result.pushes(), [])
        result = self.go({"polls": [poll(check(100, suite=PR_SUITE), check(90, "failure"))]}, 1)
        self.assertEqual(result.pushes(), [])
        self.assertTrue(any(f"actions/runs?head_sha={SHA}&event=push" in " ".join(a) for a in result.gh()))

    def test_workflow_runs_unreachable_is_a_failed_poll(self):
        result = self.go({"polls": [PASSED], "workflows": UNREACHABLE}, 75)
        self.assertIn("did not answer 5 polls in a row", result.stderr)

    # -- GitHub, the push and the remote

    def test_gh_unreachable_or_unauthenticated_gives_75(self):
        for probe in (UNREACHABLE, {"code": 4, "err": "To get started with GitHub CLI, please run:  gh auth login\n"}):
            with self.subTest(probe=probe):
                result = self.go({"probe": probe, "polls": [PASSED]}, 75)
                self.assertIn("unreachable, or gh is not authenticated", result.stderr)
                self.assertEqual(result.polls(), [])
                self.assert_cleaned_up(result)

    def test_github_dropping_out_mid_wait_is_retried(self):
        result = self.go({"polls": [NOTHING, UNREACHABLE, {"out": "{not json"}, PASSED]}, 0)
        self.assertIn("poll 2 of 5", result.stderr)

    def test_an_unknown_commit_after_the_push_waits_for_the_start_window(self):
        result = self.go({"polls": [UNKNOWN_COMMIT]}, 75, {"CI_VERIFY_START_S": "300"})
        self.assertEqual(len(result.pushes()), 1)
        self.assertIn("had not started", result.stderr)

    def test_other_422s_are_failed_polls(self):
        other = {"code": 1, "err": "gh: Validation Failed (HTTP 422)\n"}
        result = self.go({"polls": [other]}, 75)
        self.assertEqual(result.pushes(), [])
        self.assertIn("did not answer 5 polls in a row", result.stderr)

    def test_github_gone_for_five_polls_gives_75(self):
        result = self.go({"polls": [NOTHING, UNREACHABLE]}, 75)
        self.assertIn("did not answer 5 polls in a row", result.stderr)
        self.assert_cleaned_up(result)

    def test_the_push_failing_gives_75(self):
        result = self.go({"push_codes": [128], "polls": [NOTHING, PASSED]}, 75)
        self.assertEqual(len(result.pushes()), 3, "three attempts")
        self.assertIn("cannot push", result.stderr)
        self.assertIn("git push exited 128 (attempt 3 of 3)", result.stderr)
        self.assert_cleaned_up(result)

    def test_the_push_failing_once_is_retried(self):
        result = self.go({"push_codes": [128, 0], "polls": [NOTHING, PASSED]}, 0)
        self.assertEqual(len(result.pushes()), 2)

    def test_the_remote_forms_name_the_repository(self):
        for remote in (
            "git@github.com:SquashMerge/combine.git",
            "git@github.com:SquashMerge/combine",
            "ssh://git@github.com/SquashMerge/combine.git",
            "ssh://git@github.com:22/SquashMerge/combine.git",
            "https://github.com/SquashMerge/combine.git",
            "https://github.com/SquashMerge/combine/",
            SECRET_REMOTE,
        ):
            with self.subTest(remote=remote):
                result = self.go({"remote": remote, "polls": [PASSED]}, 0)
                self.assertEqual(result.gh()[0], ["api", f"repos/{REPO}"])
                self.assertNotIn(SECRET, result.stderr)

    def test_an_unparseable_remote_gives_75(self):
        for remote in (
            "/Users/someone/projects/combine",
            "../combine.git",
            "git@gitlab.com:SquashMerge/combine.git",
            "https://github.com.evil.example/SquashMerge/combine.git",
            "https://evil.example/@github.com/SquashMerge/combine.git",
            "http://github.com/SquashMerge/combine.git",
            "https://github.com/SquashMerge",
            "https://github.com/SquashMerge/combine/tree/main",
            "git@github.com:SquashMerge/com bine.git",
            f"https://x-access-token:{SECRET}@gitlab.com/o/r.git",
        ):
            with self.subTest(remote=remote):
                result = self.go({"remote": remote, "polls": [PASSED]}, 75)
                self.assertIn("does not name a github.com repository", result.stderr)
                self.assertEqual(result.gh(), [])
                self.assertEqual(result.pushes() + result.deletes(), [])
                self.assertNotIn(SECRET, result.stderr)

    def test_no_push_url_gives_75(self):
        result = self.go({"remote": None, "polls": [PASSED]}, 75)
        self.assertEqual(result.gh(), [])

    def test_a_bad_setting_gives_75(self):
        result = self.go({"polls": [PASSED]}, 75, {"CI_VERIFY_POLL_S": "ten"})
        self.assertEqual(result.calls, [])

    # -- credentials

    def test_a_credential_in_the_remote_never_appears(self):
        leaky = {"code": 1, "err": f"error: {SECRET_REMOTE} ghp_abcdefghijklmnopqrstuvwxyz0123 github_pat_11AAAA{SECRET}\n"}
        scenarios = [
            ({"polls": [NOTHING, PASSED]}, 0),
            ({"polls": [NOTHING, FAILED]}, 1),
            ({"push_codes": [128], "polls": [NOTHING]}, 75),
            ({"push_codes": [128], "delete_code": 1, "polls": [NOTHING]}, 75),
            ({"polls": [NOTHING, CANCELLED], "push_codes": [0, 128]}, 75),
            ({"probe": leaky, "polls": [PASSED]}, 75),
            ({"polls": [leaky]}, 75),
            ({"polls": [NOTHING, PASSED], "workflows": leaky}, 75),
        ]
        for scenario, status in scenarios:
            with self.subTest(scenario=scenario):
                result = self.go(dict(scenario, remote=SECRET_REMOTE), status)
                for secret in (SECRET, "ghp_abcdef", "github_pat_11"):
                    self.assertNotIn(secret, result.stderr)

    # -- the time budget (fake clock)

    def test_the_worst_case_finishes_inside_the_factory_timeout(self):
        # Defaults throughout. Every push and gh call takes 119 s, just under its 120 s bound; the
        # platform cancels the run late, and every push after the first fails.
        worst = 0
        for late in range(12, 17):
            with self.subTest(late=late):
                polls = [NOTHING] + [PENDING] * late + [CANCELLED]
                result = run_script(
                    {"polls": polls, "push_codes": [0, 128], "cost": 119},
                    {k: "" for k in ("CI_VERIFY_CALL_TIMEOUT_S", "CI_VERIFY_DEADLINE_S", "CI_VERIFY_START_S", "CI_VERIFY_POLL_S")},
                )
                self.assertEqual(result.status, 75, result.stderr)
                self.assertLessEqual(result.clock, 4800, result.stderr)
                worst = max(worst, result.clock)
        self.assertLess(worst, 5400 - 300, "at least five minutes inside verify_timeout_ms")

    def test_slow_calls_are_cut_at_the_time_left(self):
        # No per-call bound: a call that would take 10000 s gets only what is left of the budget,
        # and the branch delete still runs inside the reserve.
        result = self.go({"polls": [PASSED], "cost": 10000}, 75, {"CI_VERIFY_CALL_TIMEOUT_S": "0"})
        self.assertEqual(result.clock, 4800 - 5, "each call's 5 s TERM-to-KILL grace stays inside the budget")
        self.assertEqual(result.git()[-1], ["push", "--quiet", "origin", "--delete", REF])
        result = self.go({"polls": [PASSED], "cost": 10000}, 75, {"CI_VERIFY_CALL_TIMEOUT_S": "120"})
        self.assertLessEqual(result.clock, 4800)

    def test_a_reserve_as_large_as_the_budget_does_no_work(self):
        result = self.go({"polls": [PASSED]}, 75, {"CI_VERIFY_RESERVE_S": "4800"})
        self.assertEqual(result.gh(), [])

    # -- bounds on calls and signals (real time)

    def test_a_hung_gh_call_is_cut_off(self):
        started = time.monotonic()
        result = self.go({"probe": {"hang": True}, "polls": [PASSED]}, 75, {"CI_VERIFY_CALL_TIMEOUT_S": "1"}, fake_time=False)
        self.assertLess(time.monotonic() - started, 20)
        self.assert_cleaned_up(result)

    def test_a_call_ignoring_term_is_killed(self):
        started = time.monotonic()
        result = self.go(
            {"probe": {"hang": "ignore-term"}, "polls": [PASSED]}, 75, {"CI_VERIFY_CALL_TIMEOUT_S": "1"}, fake_time=False
        )
        self.assertLess(time.monotonic() - started, 30)
        self.assert_cleaned_up(result)

    def test_sigterm_during_a_call_ignoring_term_leaves_no_child(self):
        def terminate_once_hanging(proc, stub):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline and not (stub / "hang-pid").exists():
                time.sleep(0.05)
            time.sleep(0.5)
            proc.send_signal(signal.SIGTERM)
            hung.append(int((stub / "hang-pid").read_text()))

        hung = []
        started = time.monotonic()
        try:
            result = self.go(
                {"polls": [NOTHING, {"hang": "ignore-term"}]},
                75,
                {"CI_VERIFY_CALL_TIMEOUT_S": "60", "CI_VERIFY_POLL_S": "1"},
                fake_time=False,
                on_start=terminate_once_hanging,
            )
            self.assertEqual(len(hung), 1)
            with self.assertRaises(ProcessLookupError, msg="the TERM-ignoring gh outlived the script"):
                os.kill(hung[0], 0)
        finally:
            for child in hung:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(child, signal.SIGKILL)
        self.assertLess(time.monotonic() - started, 30)
        self.assert_cleaned_up(result)

    def test_sigterm_deletes_the_branch(self):
        def terminate_once_waiting(proc, stub):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if (stub / "count-poll").exists() and int((stub / "count-poll").read_text()) >= 2:
                    break
                time.sleep(0.05)
            time.sleep(0.3)
            proc.send_signal(signal.SIGTERM)

        started = time.monotonic()
        result = self.go(
            {"polls": [NOTHING, {"hang": True}]},
            75,
            {"CI_VERIFY_CALL_TIMEOUT_S": "60", "CI_VERIFY_POLL_S": "1"},
            fake_time=False,
            on_start=terminate_once_waiting,
        )
        self.assertLess(time.monotonic() - started, 20)
        self.assertEqual(len(result.pushes()), 1)
        self.assert_cleaned_up(result)

    def test_sigterm_while_sleeping_between_polls_exits_at_once(self):
        def terminate_once_pushed(proc, stub):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline and not (stub / "count-push").exists():
                time.sleep(0.05)
            time.sleep(0.5)
            proc.send_signal(signal.SIGTERM)

        started = time.monotonic()
        result = self.go(
            {"polls": [NOTHING, PENDING]},
            75,
            {"CI_VERIFY_CALL_TIMEOUT_S": "60", "CI_VERIFY_POLL_S": "40"},
            fake_time=False,
            on_start=terminate_once_pushed,
        )
        self.assertLess(time.monotonic() - started, 20, "the trap ran without waiting out the poll's sleep")
        self.assert_cleaned_up(result)

class Evaluate(unittest.TestCase):
    def verdict(self, pages, names=("verify",), floor=0, workflows=WORKFLOWS["pages"], sha=SHA):
        return evaluate.evaluate(pages, workflows, sha, floor, list(names))

    def test_newest_by_id_not_by_order(self):
        lines = self.verdict([page(check(300, "failure"), check(200), check(100, "cancelled"))])
        self.assertEqual(lines[:2], ["fail", "300"])

    def test_floor_hides_older_runs(self):
        self.assertEqual(self.verdict([page(check(100))], floor=100)[0], "missing")
        self.assertEqual(self.verdict([page(check(101))], floor=100)[0], "pass")

    def test_order_of_verdicts(self):
        names = ("a", "b")
        self.assertEqual(self.verdict([page(check(1, "failure", name="a"), check(2, "cancelled", name="b"))], names)[0], "fail")
        self.assertEqual(self.verdict([page(check(1, name="a"), check(2, "cancelled", name="b"))], names)[0], "infra")
        self.assertEqual(self.verdict([page(check(2, "cancelled", name="b"))], names)[0], "infra")
        self.assertEqual(self.verdict([page(check(1, status="queued", name="a"))], names)[0], "missing")
        self.assertEqual(
            self.verdict([page(check(1, status="queued", name="a"), check(2, name="b"))], names)[0], "pending"
        )

    def test_only_push_suites_of_this_sha_count(self):
        self.assertEqual(self.verdict([page(check(5, suite=PR_SUITE))])[0], "missing")
        self.assertEqual(self.verdict([page(check(5, suite=9999))])[0], "missing")
        self.assertEqual(self.verdict([page(check(5))], sha="f" * 40)[0], "missing", "a push of another sha")
        self.assertEqual(self.verdict([page(check(5))], workflows=[{"workflow_runs": []}])[0], "missing")
        self.assertEqual(self.verdict([page(check(5, suite=PR_SUITE, conclusion="failure"), check(4))])[:2], ["pass", "4"])

    def test_platform_stops_are_conclusions_only(self):
        failed = check(5, "failure")
        failed["output"] = {"title": "The job was not acquired by Runner of type hosted"}
        self.assertEqual(self.verdict([page(failed)])[0], "fail")
        for conclusion in sorted(evaluate.INFRA_CONCLUSIONS):
            self.assertEqual(self.verdict([page(check(5, conclusion))])[0], "infra", conclusion)

    def test_an_empty_listing_never_passes(self):
        self.assertEqual(self.verdict([page()])[0], "missing")

    def test_unrelated_checks_are_ignored(self):
        self.assertEqual(self.verdict([page(check(5, "failure", name="other"), check(4))])[0], "pass")

    def test_bad_input_is_refused(self):
        good_runs = json.dumps(page(check(1)))
        good_workflows = json.dumps(WORKFLOWS["pages"][0])
        cases = [(text, good_workflows) for text in ("", "{not json", "[]", '{"check_runs": 3}', '{"check_runs": [1]}')]
        cases.append((json.dumps(page({"name": "verify", "app": {"slug": "github-actions"}, "check_suite": {"id": PUSH_SUITE}})), good_workflows))
        cases += [(good_runs, text) for text in ("", "[]", '{"workflow_runs": {}}', '{"workflow_runs": [2]}')]
        for runs, workflows in cases:
            with self.subTest(runs=runs, workflows=workflows), tempfile.TemporaryDirectory() as scratch:
                (Path(scratch) / "runs").write_text(runs)
                (Path(scratch) / "workflows").write_text(workflows)
                argv = ["evaluate.py", f"{scratch}/runs", f"{scratch}/workflows", SHA, "0", "verify"]
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(evaluate.main(argv), 2)

    def test_usage_errors_fail(self):
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(evaluate.main(["evaluate.py"]), 2)
            self.assertEqual(evaluate.main(["evaluate.py", "r", "w", SHA, "0"]), 2)
            self.assertEqual(evaluate.main(["evaluate.py", "r", "w", SHA, "0", "a", "a"]), 2)


if __name__ == "__main__":
    result = unittest.main(exit=False, verbosity=1).result
    sys.exit(0 if result.wasSuccessful() else 1)
