#!/bin/sh
# M1 acceptance demo (#9): an isolated herdr-core session, "m1demo", for one host, with a few
# live terminals in two demo projects. combined is pointed at it through daemon.json.
#
# It never touches the default herdr-core.
# - Every hc call made here runs with HC_SESSION=m1demo and scratch XDG paths under /tmp/m1d.
# - It also clears the HC_* variables a terminal inherits.
# - `up` pins the demo core's id in /tmp/m1d/core-id (written once, mode 0600), after checking
#   that it differs from the default core's id.
# - `crash` and `down` fail closed. Immediately before acting, one fresh status reply from the
#   demo socket must name the pinned id, and a successful lookup of the default core's id must
#   name another. `crash` signals that same reply's pid only if it is a plain integer above 1;
#   not a holder (`holder` or any of `holders`); not this shell, its parent, its group or a
#   child; and a live process whose command is herdr-core's (`hc` or `herdr-core`).
#   /tmp/m1d and its run directories must be real directories owned by you, and the socket a
#   real socket, never symlinks. The rules follow the herdr-core lane's (tools/herdr-core-lane.sh,
#   crates/combined/tests/contract/lane_safety.rs); tools/m1demo/test_m1demo.py tests them.
#
# Usage: sh tools/m1demo/m1demo.sh up|status|daemon-json|crash|down
#   HC=/path/to/hc overrides how hc is found (non-interactive ssh shells often lack it on PATH).
#   M1DEMO_TEST_ROOT=/tmp/m1t.<suffix> replaces /tmp/m1d, for the tests only.

set -eu

ROOT=/tmp/m1d
if [ -n "${M1DEMO_TEST_ROOT:-}" ]; then
  case ${M1DEMO_TEST_ROOT#/private} in
    /tmp/m1t.?*) ;;
    *) echo "m1demo: refusing: M1DEMO_TEST_ROOT must be /tmp/m1t.<suffix>" >&2; exit 1 ;;
  esac
  case ${M1DEMO_TEST_ROOT#*/m1t.} in
    *[!A-Za-z0-9_]*) echo "m1demo: refusing: bad M1DEMO_TEST_ROOT suffix" >&2; exit 1 ;;
  esac
  ROOT=$M1DEMO_TEST_ROOT
fi
SESSION=m1demo
DEMO="$HOME/m1demo"

find_hc() {
  if [ -n "${HC:-}" ]; then printf '%s' "$HC"; return; fi
  for c in "$(command -v hc 2>/dev/null || true)" "$HOME/.local/bin/hc" /opt/homebrew/bin/hc /usr/local/bin/hc; do
    if [ -n "$c" ] && [ -x "$c" ]; then printf '%s' "$c"; return; fi
  done
  echo "m1demo: can't find hc; set HC=/path/to/hc" >&2
  exit 1
}
HC_BIN=$(find_hc)

socket_path() { printf '%s' "$ROOT/run/herdr-core/$SESSION.sock"; }

# The demo core: its own session and state. Never the default.
hcd() {
  env -u HC_TERMINAL_ID -u HC_TERMINAL_SOCKET -u HC_SCOPE -u HC_SOCKET \
    HC_SESSION="$SESSION" XDG_STATE_HOME="$ROOT/state" XDG_CONFIG_HOME="$ROOT/config" \
    XDG_RUNTIME_DIR="$ROOT/run" "$HC_BIN" "$@"
}

# The same, with the demo socket named, for status and stop.
hcd_socket() {
  env -u HC_TERMINAL_ID -u HC_TERMINAL_SOCKET -u HC_SCOPE \
    HC_SESSION="$SESSION" XDG_STATE_HOME="$ROOT/state" XDG_CONFIG_HOME="$ROOT/config" \
    XDG_RUNTIME_DIR="$ROOT/run" HC_SOCKET="$(socket_path)" "$HC_BIN" "$@"
}

field() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

demo_status() { hcd_socket server status --json; }

# safety pin|identity|target: the checks above, on one fresh status reply. Prints the pinned
# id (pin, identity) or the pid to signal (target); on any doubt says why and exits 1.
# Embedded rather than a separate file so the checks travel with the script however it is run.
safety() {
  python3 -I - "$1" "$ROOT" "$SESSION" "$HC_BIN" "$$" "$PPID" <<'PY'
import json, os, re, stat, subprocess, sys

mode, root, session, hc, shell, shell_parent = sys.argv[1:7]
CORE_COMMANDS = ("herdr-core", "hc")
ID = re.compile(r"[A-Za-z0-9_-]{1,128}")
uid = os.geteuid()


def refuse(why):
    print("m1demo: refusing: " + why, file=sys.stderr)
    sys.exit(1)


def check_kind(path, kind):
    """PATH is a directory or socket of ours, itself and never a symlink to one."""
    try:
        st = os.lstat(path)
    except OSError as error:
        refuse("%s: %s" % (path, error.strerror))
    if stat.S_ISLNK(st.st_mode):
        refuse("%s is a symlink" % path)
    if not (stat.S_ISDIR if kind == "dir" else stat.S_ISSOCK)(st.st_mode):
        refuse("%s is not a %s" % (path, "directory" if kind == "dir" else "socket"))
    if st.st_uid != uid:
        refuse("%s is not owned by you" % path)
    return st


socket = os.path.join(root, "run", "herdr-core", session + ".sock")
if check_kind(root, "dir").st_mode & 0o077:
    refuse("%s is open to other users" % root)
for sub in ("state", "config", "run", os.path.join("run", "herdr-core")):
    check_kind(os.path.join(root, sub), "dir")
check_kind(socket, "socket")

base = {k: v for k, v in os.environ.items() if not k.startswith("HC_")}
demo_env = dict(base, HC_SESSION=session, XDG_STATE_HOME=os.path.join(root, "state"),
                XDG_CONFIG_HOME=os.path.join(root, "config"),
                XDG_RUNTIME_DIR=os.path.join(root, "run"), HC_SOCKET=socket)


def status(env, which):
    """One `hc server status --json` reply and its core id; refuses on any failure."""
    try:
        out = subprocess.run([hc, "server", "status", "--json"], env=env,
                             stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired) as error:
        refuse("the %s core's status failed: %s" % (which, error))
    if out.returncode != 0:
        refuse("the %s core's status failed (exit %d)" % (which, out.returncode))
    try:
        reply = json.loads(out.stdout)
        core_id = reply["core"]["id"]
    except (ValueError, KeyError, TypeError):
        refuse("the %s core's status has no core id" % which)
    if not isinstance(core_id, str) or not ID.fullmatch(core_id):
        refuse("the %s core's id %r is not an id" % (which, core_id))
    return reply, core_id


pin_path = os.path.join(root, "core-id")


def read_pin():
    try:
        fd = os.open(pin_path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        refuse("no pinned demo core id in %s; run `up` first" % pin_path)
    except OSError as error:
        refuse("%s: %s" % (pin_path, error.strerror))
    with os.fdopen(fd) as f:
        st = os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode) or st.st_uid != uid or stat.S_IMODE(st.st_mode) != 0o600:
            refuse("%s is not a file of yours with mode 0600" % pin_path)
        text = f.read(256)
    if not text.endswith("\n") or not ID.fullmatch(text[:-1]):
        refuse("%s does not hold one core id" % pin_path)
    return text[:-1]


# The default core's id, as hc in this user's shell sees it without HC_* variables. Mandatory.
_, default_id = status(base, "default")

if mode == "pin":
    _, demo_id = status(demo_env, "demo")
    if demo_id == default_id:
        refuse("the demo core's id %s is the default core's" % demo_id)
    try:
        fd = os.open(pin_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except FileExistsError:
        if read_pin() != demo_id:
            refuse("the demo core's id %s is not the one pinned in %s" % (demo_id, pin_path))
    except OSError as error:
        refuse("%s: %s" % (pin_path, error.strerror))
    else:
        with os.fdopen(fd, "w") as f:
            os.fchmod(f.fileno(), 0o600)
            f.write(demo_id + "\n")
    print(demo_id)
    sys.exit(0)

pinned = read_pin()
if pinned == default_id:
    refuse("the pinned id %s is the default core's" % pinned)
# One snapshot, taken now: its id and its pid are what the caller acts on.
reply, demo_id = status(demo_env, "demo")
if demo_id != pinned:
    refuse("the demo socket's core %s is not the pinned demo core %s" % (demo_id, pinned))
if mode == "identity":
    print(demo_id)
    sys.exit(0)
if mode != "target":
    refuse("unknown check %r" % mode)

pid = reply.get("pid")
if type(pid) is not int or not 1 < pid < 2**31:
    refuse("the core pid %r is not a plain integer above 1" % (pid,))
holders = reply.get("holders")
holder = reply.get("holder")
if not isinstance(holders, list) or not (holder is None or isinstance(holder, dict)):
    refuse("the status's holders are unreadable")
holder_pids = set()
for entry in holders + ([holder] if holder else []):
    hp = entry.get("pid") if isinstance(entry, dict) else None
    if type(hp) is not int:
        refuse("a holder's pid %r is unreadable" % (hp,))
    holder_pids.add(hp)
if pid in holder_pids:
    refuse("pid %d is a holder" % pid)


def ps(target, fields):
    """The one line `ps -o FIELDS -p TARGET` prints, or None."""
    out = subprocess.run(["/bin/ps", "-o", fields, "-p", str(target)], stdin=subprocess.DEVNULL,
                         capture_output=True, text=True, env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"})
    lines = [line for line in out.stdout.splitlines() if line.strip()]
    return lines[0] if out.returncode == 0 and len(lines) == 1 else None


def group_of(target):
    line = ps(target, "pgid=")
    if line is None or not line.strip().isdigit():
        refuse("ps gives no process group for %s" % target)
    return int(line)


shell, shell_parent = int(shell), int(shell_parent)
groups = {group_of(shell), os.getpgrp()}
if pid in {shell, shell_parent, os.getpid(), os.getppid()} | groups:
    refuse("pid %d is this shell, its parent or its process group" % pid)
line = ps(pid, "ppid=,pgid=,comm=")
parts = line.split(None, 2) if line else []
if len(parts) != 3 or not parts[0].isdigit() or not parts[1].isdigit():
    refuse("pid %d is not a live process" % pid)
ppid, pgid, comm = int(parts[0]), int(parts[1]), parts[2]
if comm.rsplit("/", 1)[-1] not in CORE_COMMANDS:
    refuse("pid %d runs %s, not herdr-core" % (pid, comm))
if pgid in groups:
    refuse("pid %d shares our process group %d" % (pid, pgid))
if ppid in (shell, os.getpid()):
    refuse("pid %d is our child" % pid)
print(pid)
PY
}

write_echo() {
  cat > "$ROOT/echo.py" <<'PY'
# Raw-mode echo for the latency harness: writes each byte back at once.
import os, sys, termios, tty
fd = sys.stdin.fileno()
if os.isatty(fd):
    tty.setraw(fd)
while True:
    b = os.read(fd, 1)
    if not b:
        break
    os.write(sys.stdout.fileno(), b)
PY
}

project() {
  dir="$DEMO/$1"
  mkdir -p "$dir"
  if [ ! -d "$dir/.git" ]; then git -C "$dir" init -q; fi
  if [ ! -f "$dir/README.md" ]; then printf '# %s\n\nM1 demo project on %s.\n' "$1" "$(hostname -s)" > "$dir/README.md"; fi
}

run_in() {
  dir=$1; shift
  (cd "$dir" && hcd run --bg "$@")
}

up() {
  for d in "$ROOT" "$ROOT/state" "$ROOT/config" "$ROOT/run"; do
    if [ -L "$d" ] || { [ -e "$d" ] && [ ! -d "$d" ]; }; then
      echo "m1demo: refusing: $d is a symlink or not a directory" >&2
      exit 1
    fi
  done
  mkdir -p "$ROOT/state" "$ROOT/config" "$ROOT/run"
  chmod 700 "$ROOT" "$ROOT/state" "$ROOT/config" "$ROOT/run"
  hcd server start --json >/dev/null
  id=$(safety pin) || exit 1
  host=$(hostname -s | tr 'A-Z' 'a-z')
  project "$host-alpha"
  project "$host-beta"
  write_echo
  if [ "$(hcd ls --json | field 'len(d.get("terminals", d) if isinstance(d, dict) else d)')" != 0 ]; then
    echo "m1demo: terminals already running; not adding more"
  else
    run_in "$DEMO/$host-alpha" zsh -l
    if command -v htop >/dev/null 2>&1; then run_in "$DEMO/$host-alpha" htop; else run_in "$DEMO/$host-alpha" top; fi
    run_in "$DEMO/$host-beta" vim README.md
    run_in "$DEMO/$host-beta" python3 -I "$ROOT/echo.py"
  fi
  echo "m1demo: core $id up (pinned in $ROOT/core-id); socket $(socket_path)"
  hcd ls
}

daemon_json() {
  printf '{\n  "version": 1,\n  "backend": "herdr-core",\n  "herdr_core_socket": "%s"\n}\n' "$(socket_path)"
}

# The crash path: SIGKILL the demo core's own process, never a holder, then start it again.
# The holder keeps the programs; the new core adopts them, so attachments see an Inexact epoch.
crash() {
  pid=$(safety target) || exit 1
  case $pid in '' | *[!0-9]* | 0 | 1) echo "m1demo: refusing: bad core pid '$pid'" >&2; exit 1 ;; esac
  echo "m1demo: SIGKILL demo core pid $pid (its holders keep the programs)"
  kill -KILL "$pid"
  hcd server start --json >/dev/null
  id=$(safety identity) || exit 1
  echo "m1demo: demo core back: $id"
}

down() {
  safety identity >/dev/null || exit 1
  hcd_socket server stop --json >/dev/null
  echo "m1demo: demo core stopped. Terminals and $DEMO are kept; rm -rf $ROOT to clear its state."
}

case "${1:-}" in
  up) up ;;
  status) demo_status ;;
  daemon-json) daemon_json ;;
  crash) crash ;;
  down) down ;;
  *) echo "usage: $0 up|status|daemon-json|crash|down" >&2; exit 2 ;;
esac
