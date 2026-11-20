# M0 terminal-fidelity spike

Disposable code. The findings are in [FINDINGS.md](FINDINGS.md). Nothing outside `spike/` may depend on it.

Everything runs against an **isolated herdr-core** (session `m0`, state under `/tmp/cm0`), never the user's
default core. `env.sh` sets that up; `hcm_safe` refuses to continue if the isolated session resolves to the
default core (id `4bcff4a5...`). `m0d` and the Python tools also refuse the default socket path, and `m0d`
refuses a core whose `hello` reports the default core id.

## Layout

| Path | What |
| --- | --- |
| `env.sh` | `source` it: `hcm` (isolated `hc`), `hcm_safe`, `M0_SOCK`, `CARGO_TARGET_DIR`, `HEAVY` |
| `m0d/` | Rust relay daemon (`m0d serve`, `m0d connect`), its own Cargo workspace |
| `client/` | Node + pinned `@xterm/headless` 6.0.0 / `@xterm/addon-unicode11` 0.9.0: client library and tests |
| `client/lib/m0client.mjs` | daemon protocol client, `Viewer` (one attachment -> one xterm, acks applied events) |
| `client/lib/state.mjs` | full xterm state snapshot (text, cells, cursor, modes, pen, saved cursor, ...) and diff |
| `client/tests/gonogo.mjs` | the go/no-go oracle: cases a/b/c for one fixture |
| `client/tests/relay.mjs` | relay contract: replay, overflow, resize, two attachments, backpressure, gap, hc attach |
| `client/tests/measure.mjs` | keystroke latency and output burst (run through heavy.sh) |
| `fixtures/` | `modes.py`, `modes2.py` (state fixtures with an oracle trace), `flood.py`, `queries.py`, `qloop.py`, `echo.py`, `tee_pty.py` (traces vim/less), `sample.txt` |
| `tools/hcframe.py` | minimal Python herdr-core framed client (protocol probes) |
| `tools/proxy.py`, `tools/pty_attach.py` | logging socket proxy and a pty driver for `hc attach` (how the frame layout was found) |
| `tools/probes/exp_*.py` | the protocol probes behind FINDINGS section 1: credit (`exp_credit`), stall and resync (`exp_stall MB SECS NAME`), paint framing (`exp_paint`), resize notifications (`exp_resize`, `exp_obs2`), observers (`exp_obs`), queries (`exp_query viewer|noviewer`) |
| `results/` | JSON output of the last runs (`*-ssh*` = over real ssh) |
| `results/captures/` | `hc attach` wire captures (proxy hex logs), first-paint dumps per fixture and case, query results |

## Setup

```sh
cd spike/m0 && source env.sh
mkdir -p $M0/config/herdr-core
printf 'headless = "80x24"\nnotes = false\n' > $M0/config/herdr-core/config.toml   # stable size for the oracle
hcm server start --json && hcm_safe
(cd m0d && cargo build --release)            # -> $CARGO_TARGET_DIR/release/m0d
(cd client && npm ci)
```

`headless = "80x24"` matters: with the default (200x120) herdr-core resizes the PTY back to 200x120 whenever
the last active viewer leaves, and the reference emulator would need the same resizes.

## Rerun each test

All from `spike/m0/client` with `env.sh` sourced. Each test starts and stops its own `m0d` on
`/tmp/cm0/m0d.sock` and creates and kills its own terminals.

| Test | Command |
| --- | --- |
| go/no-go, state fixture (alt screen) | `node tests/gonogo.mjs modes` |
| go/no-go, main-screen modes fixture | `node tests/gonogo.mjs modes2` |
| go/no-go, vim / less | `node tests/gonogo.mjs vim` / `node tests/gonogo.mjs less` |
| go/no-go without the server restart | add `--no-restart` |
| emulator widths as xterm.js defaults them | prefix `M0_UNICODE=6` |
| relay, all | `node tests/relay.mjs` |
| relay, one | `node tests/relay.mjs replay` (also `overflow resize two backpressure gap hcattach`) |
| keystroke latency | `$HEAVY node tests/measure.mjs latency 2000` |
| 50 MB burst | `$HEAVY node tests/measure.mjs burst 50` |

Protocol probes (Python, from `spike/m0`, `env.sh` sourced):

```sh
# watch hc attach on the wire
python3 -I tools/proxy.py $M0/px.sock $M0_SOCK $M0/cap/attach.log &
env -u HC_TERMINAL_ID -u HC_TERMINAL_SOCKET -u HC_SCOPE HC_SESSION=m0 XDG_STATE_HOME=$M0/state \
  XDG_CONFIG_HOME=$M0/config XDG_RUNTIME_DIR=$M0/run HC_SOCKET=$M0/px.sock \
  python3 -I tools/pty_attach.py <terminal> $M0/cap/attach.out 'send:k' 'size:100x30' detach
```

`tools/hcframe.py` is importable for ad-hoc probes (`Conn().attach(...)`, `.credit()`, `.resize()`).

## Transports

Tests reach the daemon over its socket by default. `M0_VIA=bridge` pipes through a local `m0d connect`;
`M0_VIA=ssh` runs `ssh -T -o BatchMode=yes ${M0_SSH_HOST:-localhost} env HC_SESSION=m0 XDG_...=/tmp/cm0/... <abs m0d> connect`.
`M0D_SOCK=/tmp/cm0/other.sock` runs a test against its own daemon, so it can't collide with a queued
heavy run on the default `/tmp/cm0/m0d.sock`. Example: `M0_VIA=ssh node tests/relay.mjs replay`.

## The bridge

`m0d connect --listen <sock>` copies stdin to the daemon socket and the socket to stdout: what
`ssh -T host .../m0d connect` runs. Tests use it with `DaemonConn.connect({ via: 'bridge' })`. From another
machine (not tested here; see FINDINGS.md for the exact command):

```sh
ssh -T -o BatchMode=yes <mini> /path/to/m0d connect --listen /tmp/cm0/m0d.sock
```

## Cleanup

```sh
hcm_safe && for t in $(hcm ls --all --json | python3 -I -c 'import json,sys;print(" ".join(t["address"] for t in json.load(sys.stdin)["terminals"]))'); do hcm kill $t; hcm rm $t; done
hcm server stop --json && rm -rf /tmp/cm0
```
