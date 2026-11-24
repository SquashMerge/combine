#!/usr/bin/env python3
"""Core-free refusal tests for m1demo.sh's `crash`, `down` and `up` (PR #53 review, P1).

No core and no signal. Each case gets a scratch namespace /tmp/m1t.XXXX (M1DEMO_TEST_ROOT), a
mock `hc` that replays scripted status replies and records every call, and a shell function
that overrides `kill` and records its arguments instead of signalling. The "core" a baseline
case aims at is a `cat` started through a symlink named `hc`, so `ps` shows a core command;
it ends when its stdin closes. Run with `python3 -I tools/m1demo/test_m1demo.py`.
"""
import json
import os
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "m1demo.sh")
DEMO_ID = "d" * 32
DEFAULT_ID = "f" * 32
CORE = object()  # status(): the pid of the case's harmless "core"

MOCK_HC = r'''
import json, os, sys
scenario = json.load(open(os.environ["M1T_SCENARIO"]))
call = {"argv": sys.argv[1:], "session": os.environ.get("HC_SESSION"),
        "socket": os.environ.get("HC_SOCKET"), "runtime": os.environ.get("XDG_RUNTIME_DIR")}
with open(os.environ["M1T_CALLS"], "a") as out:
    out.write(json.dumps(call) + "\n")
demo = os.environ.get("HC_SESSION") == "m1demo"
args = sys.argv[1:]
if args[:2] in (["server", "start"], ["server", "stop"]):
    print("{}")
    sys.exit(0)
if args[:2] == ["ls", "--json"]:
    print(json.dumps({"terminals": [{"id": "hc:t1"}]}))
    sys.exit(0)
if args == ["ls"]:
    print("hc:t1")
    sys.exit(0)
if args[:2] != ["server", "status"]:
    sys.exit("unexpected mock hc call %r" % args)
if not demo:
    reply = scenario["default"]
else:
    count = os.environ["M1T_COUNT"]
    n = int(open(count).read()) if os.path.exists(count) else 0
    open(count, "w").write(str(n + 1))
    replies = scenario["demo"]
    reply = replies[min(n, len(replies) - 1)]
if reply is None:
    sys.exit("simulated status failure")
if isinstance(reply, dict) and reply.get("pid") == "SHELL":
    reply["pid"] = int(open(os.environ["M1T_SHELL"]).read())
print(json.dumps(reply))
'''

# Records `kill` instead of signalling, then sources the script, as demo_safety.py does.
WRAPPER = 'echo $$ > "$M1T_SHELL"; kill() { printf "%s\\n" "$*" >> "$M1T_KILLS"; }; . "$M1T_SCRIPT"'


def read(path):
    with open(path) as f:
        return f.read()


class Demo(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="m1t.", dir="/tmp")
        self.addCleanup(shutil.rmtree, self.root, True)
        self.work = os.path.join(self.root, "work")
        os.mkdir(self.work)
        for sub in ("state", "config", "run", "run/herdr-core"):
            os.mkdir(os.path.join(self.root, sub), 0o700)
        os.chmod(self.root, 0o700)
        self.sock = os.path.join(self.root, "run", "herdr-core", "m1demo.sock")
        self.make_socket(self.sock)
        self.hc = os.path.join(self.work, "mock-hc")
        with open(self.hc, "w") as f:
            f.write("#!%s -I\n%s" % (sys.executable, MOCK_HC))
        os.chmod(self.hc, 0o700)
        self.core = self.spawn("hc")
        self.paths = {name: os.path.join(self.work, name)
                      for name in ("scenario", "calls", "kills", "count", "shell")}

    @staticmethod
    def make_socket(path):
        # Bound only, never listening: the file is all the checks look at.
        s = socket.socket(socket.AF_UNIX)
        s.bind(path)
        s.close()

    def spawn(self, name, new_session=True):
        """A harmless process whose `ps` command ends in NAME; it ends when stdin closes."""
        link = os.path.join(self.work, name)
        if not os.path.exists(link):
            os.symlink("/bin/cat", link)
        proc = subprocess.Popen([link], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                start_new_session=new_session)
        self.addCleanup(proc.wait, 10)
        self.addCleanup(proc.stdin.close)
        return proc

    def pin(self, value=DEMO_ID, mode=0o600):
        path = os.path.join(self.root, "core-id")
        with open(path, "w") as f:
            f.write(value + "\n")
        os.chmod(path, mode)

    def status(self, pid=CORE, core_id=DEMO_ID, holders=(44444,), holder=44444):
        reply = {"core": {"id": core_id}, "pid": self.core.pid if pid is CORE else pid,
                 "holders": [{"pid": p, "current": p == holder} for p in holders],
                 "holder": None if holder is None else {"pid": holder}}
        return reply

    def run_demo(self, command, demo=None, default=None, root=None, pin=True):
        """Runs one command; the records start empty, the replies count from the first."""
        for name in ("calls", "kills", "count", "shell"):
            if os.path.exists(self.paths[name]):
                os.remove(self.paths[name])
        if pin:
            self.pin()
        scenario = {"default": {"core": {"id": DEFAULT_ID}, "pid": 11111}
                    if default is None else default,
                    "demo": [self.status()] if demo is None else demo}
        if default == "fail":
            scenario["default"] = None
        with open(self.paths["scenario"], "w") as f:
            json.dump(scenario, f)
        env = {
            "PATH": "/usr/bin:/bin:" + os.path.dirname(sys.executable),
            "HOME": os.path.join(self.work, "home"),
            "HC": self.hc,
            "M1DEMO_TEST_ROOT": self.root if root is None else root,
            "M1T_SCRIPT": SCRIPT,
            "M1T_SCENARIO": self.paths["scenario"],
            "M1T_CALLS": self.paths["calls"],
            "M1T_KILLS": self.paths["kills"],
            "M1T_COUNT": self.paths["count"],
            "M1T_SHELL": self.paths["shell"],
            "HC_SOCKET": "/nonexistent/default.sock",  # must never reach hc
        }
        self.result = subprocess.run(["/bin/sh", "-c", WRAPPER, "m1demo-test", command],
                                     env=env, capture_output=True, text=True, timeout=60)
        return self.result

    def read_lines(self, name):
        path = self.paths[name]
        return read(path).splitlines() if os.path.exists(path) else []

    def kills(self):
        return self.read_lines("kills")

    def calls(self):
        return [json.loads(line) for line in self.read_lines("calls")]

    def assert_refused(self, why, command="crash"):
        result = self.result
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("refusing", result.stderr)
        self.assertIn(why, result.stderr)
        self.assertEqual(self.kills(), [], "no signal may be recorded")
        argvs = [call["argv"][:2] for call in self.calls()]
        self.assertNotIn(["server", "stop"], argvs)
        if command == "crash":
            self.assertNotIn(["server", "start"], argvs)

    # ---- crash ----

    def test_baseline_signals_the_snapshot_pid_then_restarts(self):
        result = self.run_demo("crash")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.kills(), ["-KILL %d" % self.core.pid])
        calls = self.calls()
        self.assertEqual([c["argv"][:2] for c in calls],
                         [["server", "status"], ["server", "status"], ["server", "start"],
                          ["server", "status"], ["server", "status"]])
        default, demo = calls[0], calls[1]
        self.assertIsNone(default["session"])
        self.assertIsNone(default["socket"])
        self.assertEqual(demo["session"], "m1demo")
        self.assertEqual(demo["socket"], self.sock)
        self.assertIn("demo core back: " + DEMO_ID, result.stdout)

    def test_failed_default_lookup_refuses(self):
        self.run_demo("crash", default="fail")
        self.assert_refused("the default core's status failed")

    def test_default_lookup_without_an_id_refuses(self):
        for reply in ({"core": {}}, {"core": {"id": ""}}, {}, ["x"]):
            with self.subTest(reply=reply):
                self.run_demo("crash", default=reply)
                self.assert_refused("default core's")

    def test_missing_pin_refuses(self):
        self.run_demo("crash", pin=False)
        self.assert_refused("no pinned demo core id")

    def test_pin_with_loose_mode_or_bad_content_refuses(self):
        for value, mode in ((DEMO_ID, 0o644), ("two ids", 0o600), ("", 0o600)):
            with self.subTest(value=value, mode=oct(mode)):
                self.pin(value, mode)
                self.run_demo("crash", pin=False)
                self.assert_refused("core-id")

    def test_symlinked_pin_refuses(self):
        target = os.path.join(self.work, "elsewhere")
        with open(target, "w") as f:
            f.write(DEMO_ID + "\n")
        os.chmod(target, 0o600)
        os.symlink(target, os.path.join(self.root, "core-id"))
        self.run_demo("crash", pin=False)
        self.assert_refused("core-id")

    def test_pin_equal_to_the_default_refuses(self):
        self.run_demo("crash", default={"core": {"id": DEMO_ID}})
        self.assert_refused("is the default core's")

    def test_snapshot_of_another_core_refuses(self):
        # The review's identity_changed row: the reply that supplies the pid names another core.
        self.run_demo("crash", demo=[self.status(core_id=DEFAULT_ID, pid=11111)])
        self.assert_refused("is not the pinned demo core")

    def test_snapshot_id_mismatch_with_pin_refuses(self):
        self.pin("e" * 32)
        self.run_demo("crash", pin=False)
        self.assert_refused("is not the pinned demo core")

    def test_one_snapshot_supplies_both_id_and_pid(self):
        # A second reply naming another core is never consulted before the signal.
        other = self.spawn("hc")
        result = self.run_demo("crash", demo=[self.status(),
                                              self.status(core_id=DEFAULT_ID, pid=other.pid)])
        self.assertEqual(self.kills(), ["-KILL %d" % self.core.pid])
        # The restarted core's check reads the second reply and refuses it.
        self.assertEqual(result.returncode, 1)
        self.assertIn("is not the pinned demo core", result.stderr)

    def test_current_holder_is_excluded(self):
        self.run_demo("crash", demo=[self.status(holders=(self.core.pid,), holder=self.core.pid)])
        self.assert_refused("is a holder")

    def test_older_holder_in_holders_is_excluded(self):
        # The review's old_holder row: the pid is only in `holders`, not in `holder`.
        self.run_demo("crash", demo=[self.status(holders=(44444, self.core.pid), holder=44444)])
        self.assert_refused("is a holder")

    def test_unreadable_holders_refuse(self):
        for holders in (None, "x", [{"pid": "5"}], [7]):
            with self.subTest(holders=holders):
                reply = self.status()
                reply["holders"] = holders
                self.run_demo("crash", demo=[reply])
                self.assert_refused("holder")

    def test_this_shell_is_excluded(self):
        self.run_demo("crash", demo=[self.status(pid="SHELL")])
        self.assert_refused("this shell, its parent or its process group")

    def test_the_shells_parent_is_excluded(self):
        self.run_demo("crash", demo=[self.status(pid=os.getpid())])
        self.assert_refused("this shell, its parent or its process group")

    def test_a_process_in_our_group_is_excluded(self):
        same_group = self.spawn("hc", new_session=False)
        self.run_demo("crash", demo=[self.status(pid=same_group.pid)])
        self.assert_refused("shares our process group")

    def test_our_group_leader_pid_is_excluded(self):
        self.run_demo("crash", demo=[self.status(pid=os.getpgrp())])
        self.assert_refused("this shell, its parent or its process group")

    def test_pid_must_be_a_plain_integer_above_one(self):
        for pid in (0, 1, -5, "4242", 4242.0, True, None, 2**31, [4242]):
            with self.subTest(pid=pid):
                self.run_demo("crash", demo=[self.status(pid=pid)])
                self.assert_refused("is not a plain integer above 1")

    def test_a_process_that_is_not_herdr_core_is_refused(self):
        other = self.spawn("not-a-core")
        self.run_demo("crash", demo=[self.status(pid=other.pid)])
        self.assert_refused("not herdr-core")

    def test_a_dead_pid_is_refused(self):
        gone = self.spawn("hc")
        gone.stdin.close()
        gone.wait(10)
        self.run_demo("crash", demo=[self.status(pid=gone.pid)])
        self.assert_refused("is not a live process")

    def test_herdr_core_command_is_accepted(self):
        self.core = self.spawn("herdr-core")
        result = self.run_demo("crash")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.kills(), ["-KILL %d" % self.core.pid])

    def test_failed_demo_status_refuses(self):
        self.run_demo("crash", demo=[None])
        self.assert_refused("the demo core's status failed")

    # ---- the namespace ----

    def test_symlinked_root_refuses(self):
        link = self.root + "_link"
        os.symlink(self.root, link)
        self.addCleanup(os.remove, link)
        self.run_demo("crash", root=link)
        self.assert_refused("is a symlink")

    def test_root_open_to_others_refuses(self):
        os.chmod(self.root, 0o755)
        self.run_demo("crash")
        self.assert_refused("open to other users")

    def test_symlinked_run_directory_refuses(self):
        run = os.path.join(self.root, "run")
        os.rename(run, os.path.join(self.root, "real-run"))
        os.symlink(os.path.join(self.root, "real-run"), run)
        self.run_demo("crash")
        self.assert_refused("is a symlink")

    def test_symlinked_socket_refuses(self):
        real = os.path.join(self.root, "real.sock")
        self.make_socket(real)
        os.remove(self.sock)
        os.symlink(real, self.sock)
        self.run_demo("crash")
        self.assert_refused("m1demo.sock is a symlink")

    def test_socket_that_is_not_a_socket_refuses(self):
        os.remove(self.sock)
        open(self.sock, "w").close()
        self.run_demo("crash")
        self.assert_refused("is not a socket")

    def test_missing_socket_refuses(self):
        os.remove(self.sock)
        self.run_demo("crash")
        self.assert_refused("m1demo.sock")

    def test_test_root_outside_its_pattern_refuses(self):
        for root in ("/tmp/m1d", "/tmp/other", "/tmp/m1t.", "/tmp/m1t.a/b", "/tmp/m1t.a b"):
            with self.subTest(root=root):
                self.run_demo("crash", root=root, pin=False)
                self.assertEqual(self.result.returncode, 1)
                self.assertIn("M1DEMO_TEST_ROOT", self.result.stderr)
                self.assertEqual(self.calls(), [])

    # ---- down ----

    def test_down_stops_through_the_demo_socket(self):
        result = self.run_demo("down")
        self.assertEqual(result.returncode, 0, result.stderr)
        stop = [c for c in self.calls() if c["argv"][:2] == ["server", "stop"]]
        self.assertEqual(len(stop), 1)
        self.assertEqual((stop[0]["session"], stop[0]["socket"]), ("m1demo", self.sock))
        self.assertEqual(self.kills(), [])

    def test_down_refuses_like_crash(self):
        cases = (
            dict(default="fail", why="the default core's status failed"),
            dict(pin=False, why="no pinned demo core id"),
            dict(demo=[self.status(core_id=DEFAULT_ID)], why="is not the pinned demo core"),
            dict(default={"core": {"id": DEMO_ID}}, why="is the default core's"),
        )
        for case in cases:
            why = case.pop("why")
            with self.subTest(why=why):
                path = os.path.join(self.root, "core-id")
                if os.path.exists(path):
                    os.remove(path)
                self.run_demo("down", **case)
                self.assert_refused(why, command="down")

    # ---- up ----

    def test_up_pins_the_demo_id_once_with_mode_0600(self):
        result = self.run_demo("up", pin=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        path = os.path.join(self.root, "core-id")
        st = os.lstat(path)
        self.assertTrue(stat.S_ISREG(st.st_mode))
        self.assertEqual(stat.S_IMODE(st.st_mode), 0o600)
        self.assertEqual(read(path), DEMO_ID + "\n")
        # Again, with the same core: the pin stays as written.
        result = self.run_demo("up", pin=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read(path), DEMO_ID + "\n")

    def test_up_refuses_a_core_that_is_not_the_pinned_one(self):
        self.pin("e" * 32)
        result = self.run_demo("up", pin=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("is not the one pinned", result.stderr)
        self.assertEqual(read(os.path.join(self.root, "core-id")), "e" * 32 + "\n")

    def test_up_without_a_default_id_pins_nothing(self):
        result = self.run_demo("up", pin=False, default="fail")
        self.assertEqual(result.returncode, 1)
        self.assertIn("the default core's status failed", result.stderr)
        self.assertFalse(os.path.lexists(os.path.join(self.root, "core-id")))

    def test_up_refuses_the_default_core(self):
        result = self.run_demo("up", pin=False, demo=[self.status(core_id=DEFAULT_ID)])
        self.assertEqual(result.returncode, 1)
        self.assertIn("is the default core's", result.stderr)
        self.assertFalse(os.path.lexists(os.path.join(self.root, "core-id")))

    def test_up_refuses_a_symlinked_root(self):
        link = self.root + "_link"
        os.symlink(self.root, link)
        self.addCleanup(os.remove, link)
        result = self.run_demo("up", pin=False, root=link)
        self.assertEqual(result.returncode, 1)
        self.assertIn("is a symlink", result.stderr)
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
