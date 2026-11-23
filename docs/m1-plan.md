# M1 plan: daemon, streaming terminals, hosts

Working plan for SPEC.md milestone 1 (rev 20). SPEC.md is the source of truth for behavior; this file only orders the work. **M1 is done when** a personal MBP switches between `mini` and `work-mbp` spaces with live terminals and restored layouts, and keystroke echo over the local network stays within the 30 ms budget.

Every slice ends with a clean `trellis rules check`, a clean `python3 -I tools/arch-guard/check.py`, its tests passing (the full suite through `heavy-medium.sh`, which `./tools/verify` takes itself), and a review by opencode before the next slice that depends on it starts.

## Slices

| # | Issue | Slice | Layers (rules.toml) | Depends on | Done when |
| --- | --- | --- | --- | --- | --- |
| S1 | [#1](https://github.com/SquashMerge/combine/issues/1) | **Wire protocol.** Length-prefixed frames (JSON control, binary terminal data, 1 MB cap), handshake (protocol version, host ID), attachment cursors `(attachment, epoch, seq)`, message types for attach/resume/ack/Gap/input/resize/focus/display | `protocol` | — | round-trip and malformed-frame tests; version mismatch names both versions |
| S2 | [#2](https://github.com/SquashMerge/combine/issues/2) | **Daemon core and the fake backend.** Ports (`TerminalBackend`, `Attachment`, `FileStore`), model (`TermInfo`, spaces), spaces from the git root of each terminal's starting cwd, the per-attachment event log (ordered, applied-event acks, replay, `Gap`, bounded bytes and events), ownership generations; the in-memory fake backend; the contract test suite running against it | `daemon-core`, `daemon-ports`, `daemon-test-support`, `daemon-tests` | S1 | every contract test that doesn't need a PTY passes against the fake, including the mutation checks |
| S3 | [#3](https://github.com/SquashMerge/combine/issues/3) | **herdr-core adapter.** Framed client, paired viewers (observe display plus active input, supervised together, re-created from the latest requested size and display), `events.subscribe` with `missed` handling, list/spawn/kill, remote-terminal filtering, restart versus crash, protocol-generation check | `adapter-herdr-core` | S2 | the contract suite passes against an isolated herdr-core (`HC_SESSION`, scratch XDG paths), including restart, crash and the delayed-old-output resize case |
| S4 | [#4](https://github.com/SquashMerge/combine/issues/4) | **Daemon entry.** Unix socket server (0600, no network listener), `combined connect` bridge, `host.json` host ID, daemon config (herdr-core socket path), launchd agent plist, round-robin output batching about every 16 ms | `daemon-entry`, `adapter-fs` (host.json, config) | S1, S2 | a client over `ssh -T -o BatchMode=yes localhost <path>/combined connect` attaches, replays and resumes against the fake and the real adapter |
| S5 | [#5](https://github.com/SquashMerge/combine/issues/5) | **Client shell.** Tauri 2 app, host list (`hosts.json`), SSH bridge and local socket, reconnect with backoff, host ID check, failure messages for host keys and auth, the space rail across hosts, terminal navigator | `app-entry`, `app-hosts`, `app-state`, `web-shell`, `web-ipc`, `web-model` | S1 (S4 for end-to-end) | the rail shows spaces from `mini` and from a second host; a dropped host dims and reconnects |
| S6 | [#6](https://github.com/SquashMerge/combine/issues/6) | **Terminal panels.** xterm.js with WebGL and Unicode 11 widths, attach on show and detach on hide, replay and applied-event acks, a new xterm per epoch or `Gap`, letterboxing at the PTY size, ownership requests, display facts, DECXCPR replies only | `web-panels`, `web-renderers` | S4, S5 | two panels on one terminal behave as SPEC says, across a daemon restart and a herdr-core `server.restart`; visual check of vim, htop and an agent TUI |
| S7 | [#7](https://github.com/SquashMerge/combine/issues/7) | **Layouts and client state.** `.workspace/layout.json` (split tree, tile stacks), safe writes, watcher with self-write detection, the conflict policy, read-only projects, per-client `ClientSpaceState`, the command palette | `daemon-core`, `adapter-fs`, `app-state`, `web-shell` | S2, S5 | a restart of daemon or client restores layout and per-client state; an outside edit to `layout.json` follows the conflict policy |
| S8 | [#8](https://github.com/SquashMerge/combine/issues/8) | **Add terminal.** Auto-open the first running terminal; Add terminal spawns a plain shell or a terminal template from `.workspace/panels/*/panel.json` on the user's click, which is the approval; nothing from a repo spawns on its own. | `protocol`, `daemon-ports`, `daemon-core`, `adapter-fs`, `daemon-entry`, `app-entry`, `app-hosts`, `web-shell`, `web-ipc`, `web-model` | S7 | nothing spawns without an explicit request (fake-backend and client call-log tests); a template's cwd can't escape the project; invalid panel files are skipped with a reason; the menu shows exactly what runs |
| S9 | [#9](https://github.com/SquashMerge/combine/issues/9) | **M1 acceptance.** Local-network latency from a MacBook (p50/p95/p99, through `heavy.sh`), the MBP switching between `mini` and `work-mbp`, the crash path (`Inexact` shown in the header) | — | all | the M1 done criterion above |

**Order:** S1, then S2 and S5 in parallel (S5 can start on the shell before the daemon exists), then S3 and S4, then S6 and S7, then S8 and S9.

## How the work runs

- Each slice is one GitHub issue in milestone M1, run through the feature factory (`/feature-factory:feature <issue>`), one PR per slice. `.factory.json` resolves `#N` to the issue, and `./tools/verify` is its verify command. S1 is the pilot.

- Slices that can run in parallel are built in separate git worktrees, each with its own `CARGO_TARGET_DIR` under `combine-targets/<worktree>`, as AGENTS.md requires.
- Tests against a real herdr-core always use an isolated core (`HC_SESSION`, scratch XDG paths under a short `/tmp` path), never the default core. The contract suite refuses the default socket.
- herdr-core requests (`docs/herdr-core-requests.md`) don't block M1. When they land, the saved-cursor limitation is dropped and the adapter emits `Resynced`.

## Decisions

- **Frontend framework:** React, as mimir and trellis use (decided 2026-11-20).
