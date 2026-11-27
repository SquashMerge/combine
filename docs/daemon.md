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

- The release `combined` serves herdr-core, which is the default backend.
- `"backend": "fake"` exists only in `test-support` builds, and the release build refuses it.
- herdr-core must be running for your user; otherwise the daemon exits 3 and launchd retries every 10 s.

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

The daemon doesn't wait for herdr-core. If its socket is missing, or herdr-core doesn't answer `hello` (2 s to connect, 5 s for the answer), at start, or the backend's watch ends while running, `combined` exits 3 and, because of `KeepAlive`, launchd starts it again every 10 s. Clients reconnect and get `gap` for attachments the new process doesn't know.

A stop that isn't SIGTERM or SIGINT (SIGKILL, a crash, power loss) is handled like a crash: the OS releases both locks, the next start replaces the stale socket, and a layout change still inside its debounce is lost. SIGTERM and SIGINT are a clean shutdown (see Shutdown).

## Layouts

The daemon owns each space's layout. Clients watch a space and change it with `mutate_layout` (`SPEC.md`, "Wire protocol" and "Persistence").

- **File:** `<project>/.workspace/layout.json`, where `<project>` is the space's folder. The file is `"version": 2`; the daemon also reads version 1 and migrates it (see Migration to version 2). The first write creates `.workspace/`, 500 ms to 2 s after the first accepted change, and a write that falls back never creates it. The daemon never overwrites a `layout.json` it can't parse; an invalid file shows an error in the client and the last good layout stays on screen.
- **Writes:** a change is written 500 ms after the latest one and at most 2 s after the first. A space switch, an unwatch, a client disconnect and a clean shutdown write at once, unless the file is invalid. A crash or SIGKILL loses a change still waiting.
- **Outside edits:** edit `layout.json` by hand or with `git` while the daemon runs and it reloads the file. If you edit a field the daemon has an unsaved change to, your edit wins and the daemon drops its change, and the client shows a notice.
- **Read-only projects:** if `.workspace/` can't be written, the layout is kept in `<daemon data>/spaces/<sha256 of the space's folder path>/layout.json` and the client marks the space read-only. That file then takes priority: delete it to go back to the project's `layout.json`. While it exists, edits to the project's file are ignored.
- **Migration to version 2:** when the daemon reads a version 1 `layout.json` it changes it once to version 2. If no tab ID in it starts with `.`, it adds the follow-active tab `.active-terminal` at the end of the first tile (shown only if that tile showed nothing), then sets the version to 2. The change is written like any other, and only to the project's own `layout.json`. If the layout came from the fallback copy, or the project refuses the write, the migration stays in memory: no error, no read-only mark and no retry, so viewing a read-only project writes nothing. The next real change writes it, to the fallback copy when the project can't be written. A version 2 file is never migrated again, so a closed `.active-terminal` stays closed. An older daemon reads version 2 as invalid and never overwrites it.
- **Symlinks:** the daemon refuses a symlink at `.workspace`, under it, or at `layout.json` or a `panel.json`; a template's `cwd` follows symlinks but must stay inside the project (SPEC.md, Terminal templates).

## Open-file limit and watching

macOS refuses to watch more than a twelfth of the soft open-file limit in paths per process, and launchd starts the daemon with a soft limit of 256. The daemon watches two paths per space (the project folder and its `.workspace/`), so it raises its soft limit at start-up, before it watches anything, to the lesser of the hard limit and 10,240 (macOS refuses more). It never lowers a higher soft limit, and it logs the result once: `combined: open-file limit 10240 (hard unlimited), raised from 256`. If the raise fails, it logs why and carries on with the limit it has. The launchd plist sets no limit of its own.

If a space still can't be watched (the cap is reached, say), the daemon logs `combined: cannot watch <space>, so edits made outside Combine to its layout and panels are not seen until it can be: <error>`, once per distinct error. It tries again every 30 s, and when it succeeds it logs `watching <space> again` and reads that space's layout and panels again. The space still works through Combine meanwhile; only outside edits go unseen. Clients aren't told: the log is the only place it shows. A `.workspace/` created after its space was watched that can't be watched is retried the same way.

## Shutdown

SIGTERM and SIGINT stop the daemon cleanly. It writes every pending layout change, closes its connections, releases both locks and exits 0. Both handlers are registered before the daemon connects to herdr-core, and start-up is cancellable: a SIGTERM or SIGINT while it is connecting to herdr-core, reading the first terminal watch or listing terminals for the first time stops it at once with exit 0, before the socket is bound, so it leaves no socket behind. The ID block it reserved is spent. launchd's `bootout` sends SIGTERM, so an uninstall or a restart through `launchctl` loses no layout change that can be written. Changes held while `layout.json` is invalid, and writes that fail, aren't written. Terminals keep running in herdr-core; clients reconnect to the next daemon and get `gap` for attachments it doesn't know.

## Check the bridge over ssh

This is a manual check. It is not part of the automated pass criteria: no automated test runs ssh or reads `~/.ssh`. `sh tools/verify-herdr-core` runs `combined connect` as a subprocess with piped stdio, which is what `ssh -T` runs on the far side. The general form is `ssh -T -o BatchMode=yes localhost <path>/combined connect`. Keep the daemon's `daemon.json` at `~/.config/combine/daemon.json`, or pass `--socket <path>` to `combined connect`. With Remote Login on, this sends a `hello` for protocol version 5 through `combined connect` and prints the reply:

```sh
printf '\000\000\000\046\001{"type":"hello","protocol_version":5}' \
  | ssh -T -o BatchMode=yes localhost /opt/homebrew/bin/combined connect | xxd | head
```

Use the absolute path of `combined` on that machine in place of `<path>`. The frame is a 4-byte big-endian length (38), the kind byte (1, control) and a 37-byte JSON payload `{"type":"hello","protocol_version":5}`. The reply is a control frame holding `{"type":"welcome","protocol_version":5,"host_id":"..."}`, and the `host_id` is this host's ID, the one in `host.json`.

## Which ssh to use for the bridge

The app reaches each host's daemon with `ssh -T <host> <path>/combined connect`. Use macOS Remote Login (OpenSSH) for this, not Tailscale SSH.

Keystroke echo at typing speed, measured for #42 (`docs/m1-acceptance.md`), against the 30 ms budget:

| Bridge | p95 | p99 | Within budget |
| --- | --- | --- | --- |
| OpenSSH, LAN name (`Jasons-Mac-Mini.local`) | 11.4 ms | 28.0 ms | yes |
| macOS `sshd` over the tailnet (tailnet IP, Tailscale SSH off) | 13.3 ms | 36.0 ms | close, no |
| Tailscale SSH (MagicDNS name, `RunSSH` on) | 25.9 ms | 61.4 ms | no |

Tailscale SSH answers in `tailscaled`, not in `sshd`, and adds most of the tail. WireGuard itself adds only a few milliseconds.

**At home:** point the host at its LAN name, in `~/.ssh/config` on the client:

```
Host mini
  HostName Jasons-Mac-Mini.local
  User jcarreira
```

**Away from home:** use the tailnet, but reach `sshd` rather than Tailscale SSH.
- Tailscale SSH is a setting of the host, not of the client. While it is on, port 22 on the host's tailnet address goes to `tailscaled`.
- To use `sshd` over the tailnet, turn Tailscale SSH off on that host (`sudo tailscale set --ssh=false`), and give the tailnet name or address as `HostName`.
- This was measured at p99 36 ms: better than Tailscale SSH, but just over budget.

**Host keys:** keep host-key checking on. The first time you connect to a new name or address, accept the host key interactively, after checking its fingerprint against `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the host. The app's bridge uses `BatchMode=yes`, so it can't answer that prompt.

## Check that no network port is open

```sh
lsof -a -p "$(pgrep -x combined)" -i    # prints nothing
lsof -a -p "$(pgrep -x combined)" -U    # lists combined.sock
```

The first prints nothing, because `lsof` shows no network socket for `combined`. The second lists `combined.sock` and the daemon's other Unix sockets (its connections to herdr-core, its clients' connections and the runtime's internal socket pair), none of them a network socket.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | `combined connect` ended cleanly: the socket reached EOF or stdout closed (EPIPE). Stdin EOF alone only half-closes the socket. `combined serve` stopped on SIGTERM or SIGINT |
| 1 | a runtime failure (for example a failed write to `ids.json`), or `combined connect` couldn't reach a daemon, couldn't read stdin, or hit another I/O error |
| 2 | bad usage, a bad `daemon.json`, a bad data directory or file in it, or another daemon already serving the socket or data directory |
| 3 | herdr-core is unavailable: its socket is missing or doesn't answer at start, or its watch ended while running |

## Troubleshooting

- **`host.json`:** the daemon writes it once, on first start, and never rewrites it. If it is unreadable, not valid JSON, another version, or holds something that isn't a lowercase UUID, the daemon exits 2 and names the file. Fix or remove it by hand; removing it gives the host a new ID, and clients that stored the old one refuse the host.
- **`ids.json`:** it reserves attachment IDs so they never repeat across restarts. An invalid one stops the start (exit 2) and is never reset. Deleting it by hand resets the ID counter, so an old client cursor is answered with `gap` unless an ID happens to collide.
- **herdr-core socket:** an exit 3 names the path, where it came from (`daemon.json`, `HC_SOCKET`, `HC_SESSION` with `XDG_RUNTIME_DIR` or `XDG_STATE_HOME`, or the default) and a hint. launchd starts with a minimal environment, so set `herdr_core_socket` in `daemon.json` if herdr-core's session isn't the default. Check with `ls -l <path>`, and with `hc server status --json` against that session. Never point a test at your default herdr-core.
- **Another daemon:** exit 2 with "another combined is serving ..." (the socket's lock) or "another combined is using data dir ..." (the data directory's lock) means another daemon holds that lock. Stop it first.
