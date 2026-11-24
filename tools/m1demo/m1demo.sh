#!/bin/sh
# M1 acceptance demo (#9): an isolated herdr-core session, "m1demo", for one host, with a few
# live terminals in two demo projects. combined is pointed at it through daemon.json.
#
# It never touches the default herdr-core.
# - Every hc call made here runs with HC_SESSION=m1demo and scratch XDG paths under /tmp/m1d.
# - It also clears the HC_* variables a terminal inherits.
# - Before `crash` or `down` it stops, or kills, anything, it checks that the demo core's id differs
#   from the default core's id, and that the pid is the demo core's own process, not its holder.
#
# Usage: sh tools/m1demo/m1demo.sh up|status|daemon-json|crash|down
#   HC=/path/to/hc overrides how hc is found (non-interactive ssh shells often lack it on PATH).

set -eu

ROOT=/tmp/m1d
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

# The demo core: its own session and state. Never the default.
hcd() {
  env -u HC_TERMINAL_ID -u HC_TERMINAL_SOCKET -u HC_SCOPE -u HC_SOCKET \
    HC_SESSION="$SESSION" XDG_STATE_HOME="$ROOT/state" XDG_CONFIG_HOME="$ROOT/config" \
    XDG_RUNTIME_DIR="$ROOT/run" "$HC_BIN" "$@"
}

# Read-only: the default core's id, so the demo core can be told apart from it.
default_core_id() {
  env -u HC_TERMINAL_ID -u HC_TERMINAL_SOCKET -u HC_SCOPE -u HC_SOCKET -u HC_SESSION \
    "$HC_BIN" server status --json 2>/dev/null |
    python3 -I -c 'import json,sys
try: print(json.load(sys.stdin)["core"]["id"])
except Exception: print("")'
}

field() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

demo_status() { hcd server status --json; }

# Refuses unless the demo core is up and is not the default core.
check_demo() {
  demo_id=$(demo_status | field 'd["core"]["id"]')
  default_id=$(default_core_id)
  if [ -z "$demo_id" ] || [ "$demo_id" = "$default_id" ]; then
    echo "m1demo: refusing: demo core id '$demo_id' is empty or equals the default core's" >&2
    exit 1
  fi
}

socket_path() { printf '%s' "$ROOT/run/herdr-core/$SESSION.sock"; }

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
  mkdir -p "$ROOT/state" "$ROOT/config" "$ROOT/run"
  chmod 700 "$ROOT" "$ROOT/state" "$ROOT/config" "$ROOT/run"
  hcd server start --json >/dev/null
  check_demo
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
  echo "m1demo: core $(demo_status | field 'd["core"]["id"]') up; socket $(socket_path)"
  hcd ls
}

daemon_json() {
  printf '{\n  "version": 1,\n  "backend": "herdr-core",\n  "herdr_core_socket": "%s"\n}\n' "$(socket_path)"
}

# The crash path: SIGKILL the demo core's own process, never its holder, then start it again.
# The holder keeps the programs; the new core adopts them, so attachments see an Inexact epoch.
crash() {
  check_demo
  status=$(demo_status)
  pid=$(printf '%s' "$status" | field 'd["pid"]')
  holder=$(printf '%s' "$status" | field '(d.get("holder") or {}).get("pid", "")')
  case $pid in '' | *[!0-9]* | 0 | 1) echo "m1demo: refusing: bad core pid '$pid'" >&2; exit 1 ;; esac
  if [ "$pid" = "$holder" ] || [ "$pid" = "$$" ]; then echo "m1demo: refusing: pid $pid is the holder or this shell" >&2; exit 1; fi
  echo "m1demo: SIGKILL demo core pid $pid (holder $holder keeps the programs)"
  kill -KILL "$pid"
  hcd server start --json >/dev/null
  check_demo
  echo "m1demo: demo core back: $(demo_status | field 'd["core"]["id"]')"
}

down() {
  check_demo
  hcd server stop --json >/dev/null
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
