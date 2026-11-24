# M1 acceptance (#9)

**Verdict: M1 accepted by the owner on 2026-11-23, with the latency p99 as a follow-up.**
- Switching with live terminals and restored layouts: pass.
- The crash path: pass.
- Keystroke echo: within the 30 ms budget at typing speed over OpenSSH on the LAN: p99 28.0 ms, measured 2026-11-24 for #42. Tailscale SSH is over budget (p99 61.4 ms), so Combine's documented bridge is OpenSSH (`docs/daemon.md`, "Which ssh to use for the bridge").

Recorded 2026-11-23. M1 is done when three things hold:
- a personal MBP switches between `mini` and `work-mbp` spaces with live terminals and restored layouts;
- keystroke echo over the local network stays within the 30 ms budget;
- the crash path shows `Inexact` in the panel header.

SPEC.md, rev 21, is the source of truth for the behavior checked here.

## Setup

| Machine | Role | Hardware and OS | Software |
| --- | --- | --- | --- |
| personal MBP (`jason.local`) | client | Intel (x86_64), macOS 27.2 | `Combine.app`, built on the mini for `x86_64-apple-darwin` |
| `mini` (`jasons-mac-mini`) | agent host | Apple Silicon, macOS | `combined` (release) under launchd |
| `work-mbp` (hostname `Mac`) | agent host | Apple Silicon, macOS | `combined` (release) under launchd |

- **Build.** Both hosts ran the same `combined` release build, made from main `ffb4bcb` (PR #39 merged). The app came from the S9 branch, which adds packaging and the CSP fix below.
- **herdr-core.** Each host ran an isolated herdr-core session, `m1demo`, under `/tmp/m1d`, started with `tools/m1demo/m1demo.sh up`. Each core has two demo projects, `<host>-alpha` and `<host>-beta`, holding zsh, `top`, vim and the latency echo program. `daemon.json` points `combined` at that session's socket. The default herdr-core, which runs the agents on these machines, was not used.
- **Client config.** `hosts.json` on the MBP has two entries: `{ "name": "mini", "ssh": "jcarreira@jasons-mac-mini" }` and `{ "name": "work-mbp", "ssh": "work-mbp" }`. On first contact the client recorded both host IDs: mini `e9ac53d8-…`, work-mbp `0bb928f1-…`.
- **Network.** `jasons-mac-mini` resolves through Tailscale MagicDNS to its tailnet address, and the mini has Tailscale SSH on. So the app's ssh to `mini` is answered by `tailscaled`, over WireGuard, not by OpenSSH. Ping between the MBP and the mini is about 0.2 ms.

## Switching and restored layouts: pass

- **Both hosts in the rail.** The rail showed both hosts as Connected, with their spaces: `jasons-mac-mini-alpha` and `jasons-mac-mini-beta`; `mac-alpha` and `mac-beta`.
- **Live terminals.** Selecting a space showed its first live terminal in the focused tile, painted from herdr-core. Typing reached the program, and the header showed `input: this panel` after the first keystroke.
- **Switching both ways.** Switching between `mini` and `work-mbp` spaces kept every terminal live. Earlier output was still there when a space was selected again.
- **Full-screen programs.** `top` updated live and vim rendered.
- **Restored layout.** After **Split right** in a space, the app was quit and reopened, and the space selected again. The split came back. Closing the new tile returned the space to one tile.

## Crash path: pass

1. The test started with `jasons-mac-mini-alpha`'s zsh visible in the panel.
2. `tools/m1demo/m1demo.sh crash` sent SIGKILL to the `m1demo` core's own process. It checked the core id against the default core's first, and never signals the holder. After review, the script was made fail-closed. It pins the demo core id at `up`, and the default core's id must be read successfully. The id and the pid it kills now come from one status reply. It excludes every holder and our own process tree, and it checks the live process is herdr-core (`tools/m1demo/test_m1demo.py`).
3. The holder kept zsh alive, and the next `hc server start` adopted it.
4. The daemon did not restart: the adapter reconnected within its reopen window.
5. The panel header showed **inexact**. The screen repainted from the adopted program, with zsh's partial-line `%` markers visible, which is what `Inexact` warns about. Input went back to `none` for the new epoch.

## Keystroke echo latency: within budget over OpenSSH on the LAN (#42)

`tools/latency/echo_latency.py` runs on the MBP. It speaks Combine's wire protocol through `ssh … /opt/homebrew/bin/combined connect` to the mini, attaches to the echo terminal and takes input ownership. Each sample times one keystroke until its echo arrives in a data frame, so the measurement stops before rendering. A run is 2000 samples. The interval is 3 ms for the stress case, as M0 did, and 100 ms for typing speed.

For a measurement, the remote end runs under the large heavy lock (`heavy.sh`), so no build runs on the mini at the same time.

The daemon flushes output at most once every 16 ms (SPEC, Architecture), and sends at once only after it has been idle for 16 ms. At a 3 ms interval most echoes wait for the next flush, so those numbers measure batching more than transport. The budget applies to typing speed.

### First runs, without the lock, over Tailscale SSH (provisional)

The heavy lock was wedged (see Findings), so no build could run. The mini's load average was about 1.7.

| Interval | requested / completed / timed out | p50 | p95 | p99 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| 3 ms | 2000 / 2000 / 0 | 14.5 ms | 17.4 ms | 24.6 ms | 124.5 ms | 14.7 ms |
| 100 ms | 2000 / 2000 / 0 | 7.5 ms | 12.4 ms | **37.3 ms** | 132.3 ms | 8.9 ms |

At typing speed, p99 is over the 30 ms budget. Running locally on the mini (`ssh -T localhost`, 50 samples, under heavy load), the same daemon gives a p50 of 0.58 ms. So the extra time is on the path between the machines, and Tailscale SSH (user-space ssh in `tailscaled`, over WireGuard) is the prime suspect.

A bridge through a remote pty (`ssh -tt` with `stty raw`, so OpenSSH sets `TCP_NODELAY`) was much slower locally: p50 62 ms. It was dropped.

### Runs under the lock: Tailscale SSH vs OpenSSH over the LAN

These runs were taken under the large lock on 2026-11-24, from the livingroom MBP (`jason.local`, on Wi-Fi) to the mini's `combined connect`. Each has 2000 samples and 0 timeouts. They compare three bridges:
- **Tailscale SSH:** `jasons-mac-mini`, answered by `tailscaled`.
- **OpenSSH on the LAN:** `Jasons-Mac-Mini.local`.
- **macOS `sshd` over the tailnet:** `100.95.124.51`, with Tailscale SSH turned off on the mini, so port 22 on the tailnet reaches `sshd`.

`tailscale ping` showed a direct path between the two machines (via 192.168.1.25:41641), not a DERP relay.

| Bridge | Interval | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| Tailscale SSH | 100 ms | 7.9 ms | 25.9 ms | **61.4 ms** | 241.5 ms |
| macOS `sshd` over the tailnet | 100 ms | 7.6 ms | 13.3 ms | **36.0 ms** | 154.6 ms |
| OpenSSH on the LAN | 100 ms | 7.1 ms | 11.4 ms | **28.0 ms** | 289.2 ms |
| Tailscale SSH | 3 ms | 14.3 ms | 17.7 ms | 26.2 ms | 72.3 ms |
| macOS `sshd` over the tailnet | 3 ms | 14.1 ms | 17.1 ms | 22.3 ms | 56.9 ms |
| OpenSSH on the LAN | 3 ms | 14.2 ms | 17.0 ms | 20.5 ms | 59.6 ms |

**OpenSSH on the LAN meets the budget at typing speed** (p99 28.0 ms).

**Most of the Tailscale cost is Tailscale SSH's own server.** Its p95 is twice the LAN's, and replacing it with `sshd` on the same WireGuard path cuts p99 from 61 to 36 ms. WireGuard itself adds about 2 ms at p95 and 8 ms at p99 over the plain LAN.

At 3 ms every bridge is within budget, because the 16 ms flush dominates.

**Caveats:** these are single runs, taken at different times of day. With 2000 samples, p99 is the 20th-worst sample, so the gap between 28 and 36 ms is suggestive rather than settled.

**Outcome:** Combine's documented bridge is macOS Remote Login (OpenSSH), with the LAN name as `HostName` at home (`docs/daemon.md`, "Which ssh to use for the bridge"). This closes #42.

## Findings and fixes from the session

- **Typing did nothing in the packaged app (fixed in this PR).** The CSP had no `connect-src` for Tauri's IPC.
  - The webview fell back to `postMessage`, which carries only JSON.
  - Typed input and automatic replies are raw-byte commands. They arrived as JSON, were rejected as malformed, and were dropped silently.
  - JSON commands kept working, so "Take input", ack and attach were fine.
  - Fix: `connect-src ipc: http://ipc.localhost`.
  - Tests never caught it, because they run under jsdom or the dev server, where this CSP isn't applied.
- **The app had never been packaged.** `bundle.active` was false, and there was no Tauri CLI. This PR adds `@tauri-apps/cli` 2.12.4 as a pinned dev dependency and enables the `app` bundle. The client MBP is Intel, so the app was cross-built for `x86_64-apple-darwin`.
- **Navigator labels run together.** They show `zshzsh -lrunning`: name, command and state with no separators. Follow-up #43.
- **A terminal with no input owner never refits.** It keeps its last size, letterboxed, until someone takes input. This is SPEC behavior: only the owner resizes. It's an M2 UX question.
- **Heavy lock under the system Python.**
  - Over a non-interactive ssh, `heavy.sh` ran `heavy_run.py` under `/usr/bin/python3` 3.9.6, which crashed in `_acquire` → `_release`.
  - The crash left a lease that wasn't broken after 600 s, and it blocked every heavy job until it was cleared.
  - This was reported to the heavy-lock owner (the Trellis side).
  - The workaround: put mise's Python 3.13 first on `PATH` for the remote `heavy.sh`.
