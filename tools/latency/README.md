# Keystroke-echo latency (S9, M1 acceptance)

SPEC.md's latency budget says keystroke echo should stay under about 30 ms on a local network. `echo_latency.py` measures it through the real `combined` daemon, from the machine you type on, over the same `ssh -T ... combined connect` bridge the app uses.

It needs only the system `python3` and its standard library. Run it with `python3 -I`.

## What it measures

The harness starts the bridge command with piped stdin and stdout and speaks Combine's wire protocol (SPEC.md, "Wire protocol", version 6) over those pipes:

1. It sends `hello` and reads `welcome`.
2. It sends `watch_terminals` and picks the terminal named by `--terminal`, or the one running terminal carrying `--label key=value`.
3. It attaches at the terminal's own size with `observe: false`, then sends `request_ownership` and `focus` and waits until it owns input.
4. It reads and acks until the first paint has gone quiet (`--settle-ms`).
5. For each sample it sends one printable byte as an input frame (kind 3) and times, with `time.perf_counter_ns`, how long until a data frame (kind 2) carrying that byte arrives. The bytes rotate through `a-zA-Z0-9`, so a late echo is never taken for the current one.
6. It acks after each sample and pauses `--interval-ms`, still reading and acking.
7. It sends `detach`, acks the final `closed`, and closes the bridge's stdin.

Timing stops when the data frame is read off the bridge's stdout, before anything is rendered, as in M0. The first `--warmup` keystrokes (default 10) are sent and discarded.

The terminal must run `echo.py`, a raw-mode program that writes each byte back at once.

Every wait has a timeout. The harness stops with `FAILED: ...` and exit code 1 in these cases:

- a `gap`, `lost`, `closed` or `error` frame;
- a malformed frame;
- a bridge that exits or stops reading;
- another client taking input ownership;
- a new epoch on the attachment after the attach (a second `started`, or a frame of another epoch), during a sample or between samples. Its first paint can contain a sampled byte the program hasn't echoed, so it never counts as an echo;
- `--max-consecutive-timeouts` samples in a row (default 10) with no echo.

A sample whose echo takes longer than `--timeout-ms` is counted as timed out and left out of the percentiles.

### The daemon's 16 ms flush

The daemon flushes output at most once per 16 ms. The first output after an idle 16 ms goes at once (SPEC.md, Architecture). With the default `--interval-ms 3`, the gap between keystrokes is shorter than that, so most echoes wait for the next flush. The numbers then show that batching, not the echo of a single keystroke, and the harness prints a note saying so. To measure keystrokes at human typing speed, add a run with `--interval-ms 100`.

## The rule: never the default herdr-core

The harness talks only to the daemon at the end of its bridge, and the daemon decides which herdr-core that is. So every measurement uses an isolated demo core with its own `combined serve`, and the bridge names that daemon's socket with `--socket`. Never point the bridge at the daemon that serves your default herdr-core: that core runs the agents working on this repo. Never stop or kill anything until `hc server status --json` reports a core id other than the default's.

## 1. On the measured host: an isolated core, the echo terminal, a daemon

Run this on the machine that runs the agents, for example `mini`, from a checkout of this repository. It needs `hc` and the `combined` binary you are measuring.

```sh
# A short scratch root (macOS caps socket paths at 103 bytes) and its own session.
R=$(mktemp -d /tmp/c9lat.XXXXXX); chmod 700 "$R"
mkdir -m 700 "$R/state" "$R/config" "$R/run" "$R/config/herdr-core" "$R/space" "$R/daemon"
printf 'headless = "200x120"\nnotes = false\n' >"$R/config/herdr-core/config.toml"
SOCK=$R/run/herdr-core/c9lat.sock
hcl() {
  env -i PATH="$PATH" HOME="$HOME" USER="$USER" LOGNAME="${LOGNAME:-$USER}" TMPDIR=/tmp \
    HC_SESSION=c9lat XDG_STATE_HOME="$R/state" XDG_CONFIG_HOME="$R/config" \
    XDG_RUNTIME_DIR="$R/run" HC_SOCKET="$SOCK" hc "$@"
}
hcl server start --json

# Pin the isolated core's id, and check it isn't the default core's.
DEFAULT_ID=$(hc server status --json | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["core"]["id"])')
CORE_ID=$(hcl server status --json | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["core"]["id"])')
[ -n "$CORE_ID" ] && [ "$CORE_ID" != "$DEFAULT_ID" ] && echo "isolated core $CORE_ID" || echo "STOP: not isolated"

# The echo terminal, labelled so the harness can find it.
cp tools/latency/echo.py "$R/echo.py"
hcl run --bg --no-parent --name echo --label role=echo --cwd "$R/space" /usr/bin/python3 -I "$R/echo.py"

# A daemon of its own on that core, with its own data dir and socket.
cat >"$R/daemon/daemon.json" <<EOF
{"version": 1, "backend": "herdr-core", "herdr_core_socket": "$SOCK",
 "data_dir": "$R/daemon/data", "socket": "$R/daemon/data/combined.sock"}
EOF
env -i PATH=/usr/bin:/bin HOME="$HOME" USER="$USER" TMPDIR=/tmp \
  /opt/homebrew/bin/combined serve --config "$R/daemon/daemon.json" >"$R/daemon/serve.log" 2>&1 &
echo $! >"$R/daemon/serve.pid"
echo "bridge socket: $R/daemon/data/combined.sock"
```

Check it locally before you go to the other machine:

```sh
python3 -I tools/latency/echo_latency.py --label role=echo --samples 50 \
  -- /opt/homebrew/bin/combined connect --socket "$R/daemon/data/combined.sock"
```

## 2. From the other machine: run the harness over ssh

Timing measurements hold the large heavy lock (`heavy.sh`) on the host being measured, so other builds and test suites there can't skew them. The simplest way is to wrap the remote end of the bridge. The lease is then held exactly as long as the connection lasts. Use the whole path to `heavy.sh`, because non-interactive ssh has a minimal `PATH`. `heavy.sh` logs only to stderr, so the bridge's stdout stays protocol frames. The lock may queue, so give the handshake a long `--connect-timeout-ms`.

On the MacBook, from a checkout of this repository (only `tools/latency/echo_latency.py` is needed), with `R` set to the root printed on the mini:

```sh
R=/tmp/c9lat.XXXXXX   # the root from step 1
HEAVY=/Users/jcarreira/projects/squashmerge/trellis-evidence/heavy-lock/heavy.sh
for iv in 3 100; do
  python3 -I tools/latency/echo_latency.py --label role=echo --samples 2000 --interval-ms $iv \
    --connect-timeout-ms 14400000 --json "latency-mini-i$iv.json" \
    -- ssh -T -o BatchMode=yes mini "$HEAVY" /opt/homebrew/bin/combined connect \
       --socket "$R/daemon/data/combined.sock"
done
```

Keep the MacBook quiet during the run as well, with no builds running and other apps idle, and connect it to the network you mean to measure. The report goes to stdout and to the `--json` file. It has these fields:

- `requested`, `completed`, `timed_out`;
- `stats`: `p50_ms`, `p95_ms`, `p99_ms`, `max_ms`, `mean_ms` and `min_ms`, nearest-rank;
- `bridge_argv`, `client_host` (the machine that ran the harness), `daemon_host_id`, `terminal`;
- `timestamp` and `finished` (UTC), `settings`, and `events` (resizes, other data frames and bytes seen during the run).

Add `--raw` to include every sample in nanoseconds.

## 3. Tear down, on the measured host

Stop the daemon with SIGTERM first. Then stop the core, but only after the id check passes again:

```sh
kill -TERM "$(cat "$R/daemon/serve.pid")"
[ "$(hcl server status --json | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["core"]["id"])')" = "$CORE_ID" ] \
  && [ "$CORE_ID" != "$DEFAULT_ID" ] && hcl server stop --json && rm -rf -- "$R"
```

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `--terminal <TermId>` / `--label k=v` | (one is required) | The terminal to measure |
| `--samples N` | 2000 | Measured keystrokes |
| `--interval-ms` | 3 | Pause after each echo before the next keystroke |
| `--timeout-ms` | 1000 | Per sample |
| `--warmup N` | 10 | Keystrokes sent first and discarded |
| `--json <path>` | none | Where the results are written |
| `--connect-timeout-ms` | 20000 | Bridge start and handshake |
| `--io-timeout-ms` | 10000 | Every other reply (list, attach, ownership, detach) |
| `--settle-ms` | 300 | Quiet time after attaching, before the first keystroke |

Run the pure tests with `python3 -I tools/latency/test_echo_latency.py`.
