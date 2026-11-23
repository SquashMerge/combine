# Running the Combine daemon

`combined` is the daemon: one per agent host, running as you. `SPEC.md`, "Hosts and connections" and "Persistence", is the design; this page is how to install and check it. Combine never runs the commands below, and never writes `~/Library/LaunchAgents` itself. You run them.

## Paths

| Name | Path | Holds |
| --- | --- | --- |
| `<daemon config>` | `~/.config/combine/` | `daemon.json`, the start-up settings |
| `<daemon data>` | `~/Library/Application Support/com.squashmerge.combined/`, mode 0700 | `host.json`, `ids.json`, `combined.lock`, `combined.sock`, `combined.sock.lock` |
| `<daemon cache>` | `~/Library/Caches/com.squashmerge.combined/` | cached source data (later milestones) |
| log | `~/Library/Logs/combined.log` | stdout and stderr when run by launchd |

The daemon's socket is `<daemon data>/combined.sock`, mode 0600. A `hosts.json` entry for this machine uses that path as its `socket`.

## `daemon.json`

Optional. With no file at `~/.config/combine/daemon.json`, every default applies. Every field is optional, paths must be absolute, and an unknown field, another `version`, bad JSON or a relative path stops the start with exit 2.

```json
{
  "version": 1,
  "backend": "herdr-core",
  "herdr_core_socket": "/Users/me/.local/state/herdr-core/default/default.sock",
  "data_dir": "/Users/me/Library/Application Support/com.squashmerge.combined",
  "socket": "/Users/me/Library/Application Support/com.squashmerge.combined/combined.sock"
}
```

| Field | Meaning | Default |
| --- | --- | --- |
| `backend` | `herdr-core`. `fake` exists only in builds with the `test-support` feature | `herdr-core` |
| `herdr_core_socket` | herdr-core's socket | `HC_SOCKET`, else `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`, else `${XDG_STATE_HOME:-$HOME/.local/state}/herdr-core/<session>/<session>.sock`, with `<session>` from `HC_SESSION` or `default` |
| `data_dir` | the `<daemon data>` directory | see Paths |
| `socket` | the daemon's socket | `<data_dir>/combined.sock` |

`combined serve --config <path>` reads another file; a missing file named that way stops the start. A socket path is at most 103 bytes.

## Before you install

Until #23 wires the herdr-core adapter, a release `combined` can't serve: `backend = "herdr-core"` exits 3, and the release build refuses `"backend": "fake"`. launchd with `KeepAlive` would restart that exit every 10 s, so don't install the release build yet. The checks below need an interim build, `cargo build -p combined --features test-support`, run by its absolute path (`target/debug/combined`) with `"backend": "fake"` in `daemon.json`.

## Install

From the repository root:

```sh
cargo build --release -p combined
install -m 0755 target/release/combined /opt/homebrew/bin/combined
mkdir -p ~/Library/Logs
sed "s#__HOME__#$HOME#g" crates/combined/launchd/com.squashmerge.combined.plist \
  > ~/Library/LaunchAgents/com.squashmerge.combined.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.squashmerge.combined.plist
launchctl print gui/$(id -u)/com.squashmerge.combined
```

The plist runs `/opt/homebrew/bin/combined serve` at load and keeps it running (`RunAtLoad`, `KeepAlive`, `ThrottleInterval` 10). Change the path in the installed copy if you put the binary elsewhere.

## Uninstall

```sh
launchctl bootout gui/$(id -u)/com.squashmerge.combined
rm ~/Library/LaunchAgents/com.squashmerge.combined.plist
```

## Fail-fast and restarts

The daemon doesn't wait for herdr-core. If its socket is missing at start, or the backend's watch ends while running, `combined` exits 3 and, because of `KeepAlive`, launchd starts it again every 10 s. Clients reconnect and get `gap` for attachments the new process doesn't know.

A stop that isn't SIGTERM or SIGINT (SIGKILL, a crash, power loss) is handled like a crash: the OS releases both locks, the next start replaces the stale socket, and a layout change still inside its debounce is lost. SIGTERM and SIGINT are a clean shutdown (see Shutdown).

## Layouts

The daemon owns each space's layout. Clients watch a space and change it with `mutate_layout` (`SPEC.md`, "Wire protocol" and "Persistence").

- **File:** `<project>/.workspace/layout.json`, where `<project>` is the space's folder. The first write creates `.workspace/`, 500 ms to 2 s after the first accepted change, and a write that falls back never creates it. The daemon never overwrites a `layout.json` it can't parse; an invalid file shows an error in the client and the last good layout stays on screen.
- **Writes:** a change is written 500 ms after the latest one and at most 2 s after the first. A space switch, an unwatch, a client disconnect and a clean shutdown write at once, unless the file is invalid. A crash or SIGKILL loses a change still waiting.
- **Outside edits:** edit `layout.json` by hand or with `git` while the daemon runs and it reloads the file. If you edit a field the daemon has an unsaved change to, your edit wins and the daemon drops its change, and the client shows a notice.
- **Read-only projects:** if `.workspace/` can't be written, the layout is kept in `<daemon data>/spaces/<sha256 of the space's folder path>/layout.json` and the client marks the space read-only. That file then takes priority: delete it to go back to the project's `layout.json`. While it exists, edits to the project's file are ignored.
- **Symlinks:** the daemon refuses a symlink at `.workspace`, under it, or at `layout.json` or a `panel.json`; a template's `cwd` follows symlinks but must stay inside the project (SPEC.md, Terminal templates).

## Shutdown

SIGTERM and SIGINT stop the daemon cleanly. It writes every pending layout change, closes its connections, releases both locks and exits 0. Both handlers are registered before the socket is bound. launchd's `bootout` sends SIGTERM, so an uninstall or a restart through `launchctl` loses no layout change that can be written. Changes held while `layout.json` is invalid, and writes that fail, aren't written. Terminals keep running in herdr-core; clients reconnect to the next daemon and get `gap` for attachments it doesn't know.

## Check the bridge over ssh

Until #23 lands this needs the interim `test-support` build and a daemon on `"backend": "fake"`, started by that build's `combined serve`. Keep its `daemon.json` at `~/.config/combine/daemon.json`, or pass `--socket <path>` to `combined connect`, and use the build's absolute path below. With Remote Login on, this sends a `hello` for protocol version 4 through `combined connect` and prints the reply:

```sh
printf '\000\000\000\046\001{"type":"hello","protocol_version":4}' \
  | ssh -T -o BatchMode=yes localhost /opt/homebrew/bin/combined connect | xxd | head
```

Use the absolute path of `combined` on that machine. The frame is a 4-byte big-endian length (38), the kind byte (1, control) and a 37-byte JSON payload `{"type":"hello","protocol_version":4}`. The reply is a control frame holding `{"type":"welcome","protocol_version":4,"host_id":"..."}`, and the `host_id` is this host's ID, the one in `host.json`.

## Check that no network port is open

Until #23 lands, run this against the interim `test-support` build serving the `fake` backend.

```sh
lsof -a -p "$(pgrep -x combined)" -i    # prints nothing
lsof -a -p "$(pgrep -x combined)" -U    # lists combined.sock
```

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | `combined connect` ended cleanly: the socket reached EOF or stdout closed (EPIPE). Stdin EOF alone only half-closes the socket. `combined serve` stopped on SIGTERM or SIGINT |
| 1 | a runtime failure (for example a failed write to `ids.json`), or `combined connect` couldn't reach a daemon, couldn't read stdin, or hit another I/O error |
| 2 | bad usage, a bad `daemon.json`, a bad data directory or file in it, or another daemon already serving the socket or data directory |
| 3 | herdr-core is unavailable: its socket is missing at start, or its watch ended while running |

## Troubleshooting

- **`host.json`:** the daemon writes it once, on first start, and never rewrites it. If it is unreadable, not valid JSON, another version, or holds something that isn't a lowercase UUID, the daemon exits 2 and names the file. Fix or remove it by hand; removing it gives the host a new ID, and clients that stored the old one refuse the host.
- **`ids.json`:** it reserves attachment IDs so they never repeat across restarts. An invalid one stops the start (exit 2) and is never reset. Deleting it by hand resets the ID counter, so an old client cursor is answered with `gap` unless an ID happens to collide.
- **herdr-core socket:** an exit 3 names the path, where it came from (`daemon.json`, `HC_SOCKET`, `HC_SESSION` with `XDG_RUNTIME_DIR` or `XDG_STATE_HOME`, or the default) and a hint. launchd starts with a minimal environment, so set `herdr_core_socket` in `daemon.json` if herdr-core's session isn't the default. Check with `ls -l <path>`, and with `hc server status --json` against that session. Never point a test at your default herdr-core.
- **Another daemon:** exit 2 with "another combined is serving ..." (the socket's lock) or "another combined is using data dir ..." (the data directory's lock) means another daemon holds that lock. Stop it first.
