# M2 acceptance (#52)

**Verdict: every M2 done criterion passed on 2026-11-27.**
- A real agent in an isolated demo core created a class diagram through `combine mcp`, with no client attached.
- Its first revision was `failed` with a line and column. The agent repaired the same panel to `validated`.
- The Combine.app built for Intel, attaching afterwards from the MBP, drew it and reported `rendered`.

M2 is done when a real agent, in an isolated demo core with `combine mcp` registered for its project only, does three things with no client attached:
1. creates a class diagram;
2. gets `failed` with a line and column;
3. repairs **the same panel** until it reaches `validated`.

A client that attaches afterwards must see `rendered`. SPEC.md, rev 36, is the source of truth for the behaviour checked here.

## Setup

| Machine | Role | Hardware | Software |
| --- | --- | --- | --- |
| `mini` (`jasons-mac-mini`) | agent host | Apple Silicon, macOS | `combined` and `combine` (release, protocol 7) under launchd |
| personal MBP | client | Intel (x86_64), macOS | `Combine.app`, built on the mini for `x86_64-apple-darwin` |

- **Build.** Everything was built from main `6819626` (PR #103 merged; wire protocol 7, SPEC rev 36).
  - **Daemon and shim:** `heavy-medium.sh cargo build --release -p combined -p combine@0.0.0`, installed to `/opt/homebrew/bin/combined` and `/opt/homebrew/bin/combine`. Then `launchctl kickstart -k` restarted the `com.squashmerge.combined` job. The M1 daemon (protocol 4) is kept as `/opt/homebrew/bin/combined.m1-backup`.
  - **App:** `heavy-medium.sh sh tools/csp-probe/x86_64.sh` built it, with no probe feature and no config override. Checks: `lipo -archs` is `x86_64`, and the bundle has no probe markers. It was copied to the MBP as a `ditto` zip.
- **herdr-core.** The isolated `m1demo` session under `/tmp/m1d` ran herdr-core 0.1.22, core id `f7763bad5dc088a8699a5a336b039333`. Before the agent started, a status reply named that pinned id, and it differs from the default core's. `~/.config/combine/daemon.json` points `combined` at `/tmp/m1d/run/herdr-core/m1demo.sock`. The default herdr-core, which runs the agents working on this repo, was not used.
- **Project.** The demo project `~/m2demo/jasons-mac-mini-diagrams` is a git repository holding only a README and a project `.mcp.json`. The `.mcp.json` registers one server, `combine`: `/opt/homebrew/bin/combine mcp --socket <the daemon's combined.sock>`. Nothing was added to any user-global agent config.
- **Agent.** Claude Code 2.1.332 (`claude-opus-5-5`) ran headless inside a terminal of the demo core:
  - started with `hc run --bg --no-parent --name s9-agent`, with `HC_SESSION=m1demo` and the demo's scratch XDG paths;
  - the terminal got `HC_TERMINAL_ID=t5` and `HC_TERMINAL_SOCKET=/tmp/m1d/run/herdr-core/m1demo.sock`;
  - flags: `-p <prompt> --mcp-config <project>/.mcp.json --strict-mcp-config`;
  - allowed tools: `create_panel`, `update_panel`, `panel_status`, `list_panels` and `show_diagram` from the `combine` server;
  - disallowed tools: `Bash`, `Edit`, `Write`, `NotebookEdit`, `WebFetch`, `WebSearch` and `Task`.

  So the agent could reach Combine only through MCP.
- **No client attached.** The mini's own Combine.app (the M1 build) was quit before the run, and the MBP's app stayed closed until the agent had finished.

## The agent's run: pass

The prompt asked for a Mermaid class diagram titled "Combine daemon": Daemon, Core, TerminalBackend and its two implementations, TerminalList, LayoutSync and Layout. Real agents usually write valid Mermaid first time, so to make the failure path certain the prompt also asked that **the first version contain exactly one deliberate syntax error**. It then asked the agent to repair the same panel with `update_panel` until `validated`.

`combine mcp` resolved the caller through `HC_TERMINAL_ID`, because `HC_TERMINAL_SOCKET` is the daemon's own herdr-core socket. The panel's space is the demo project.

| Step | Call | Panel | Revision | Result |
| --- | --- | --- | --- | --- |
| 1 | `create_panel` | `panel-1048577` | 1048578 | `failed`: `syntax` at **line 29, column 13**, snippet `Daemon *-=- Core` / `------------^`, "Expecting 'AGGREGATION', … got 'MINUS'" |
| 2 | `update_panel` (same ID) | `panel-1048577` | 1048579 | `validated`, with `error: null` |
| 3 | `panel_status` | `panel-1048577` | 1048579 | `validated`, `render_status: []`: no client had drawn it |
| 4 | `list_panels` | n/a | n/a | one panel, no skipped folders, nothing dropped |

The run took 22.4 s over 6 turns and ended without error. No call answered `validating`. The daemon's audit log has one line per mutating call:

```
combined: mcp create_panel space="/Users/jcarreira/m2demo/jasons-mac-mini-diagrams" slot=null panel="panel-1048577" revision=1048578 state=failed
combined: mcp update_panel space="/Users/jcarreira/m2demo/jasons-mac-mini-diagrams" slot=null panel="panel-1048577" revision=1048579 state=validated
```

On disk, `.workspace/panels/panel-1048577/` holds `panel.json`, which has `created_by: { "type": "agent", "slot": null }` and no `placement`, and the repaired 35-line `diagram.mmd`. The daemon placed the panel in the first tile: `layout.json`'s `main` tile lists `.active-terminal` and `panel-1048577`, with the panel active.

An independent check confirmed the state before any client attached. A driver in this checkout ran the real `combine mcp` shim from the demo project's folder, so its caller resolved by cwd, and asked for `panel_status`. The reply was revision 1048579, `validated`, `render_status: []`.

## A client attaching afterwards: pass

The x86_64 `Combine.app` was opened on the MBP. Its `hosts.json` is unchanged from M1.

- **Hosts.** The rail showed `mini` as Connected, with the space `jasons-mac-mini-diagrams`. `work-mbp` showed `protocol version mismatch: client 7, daemon 4`, since that host still runs the M1 daemon. That's expected, and not part of this check.
- **The panel.** Selecting the space showed the `main` tile with the tabs **Active terminal** and **Combine daemon**, the panel active. The diagram was fully drawn:
  - class boxes with their members;
  - `«interface»` on TerminalBackend;
  - filled-diamond composition markers;
  - dashed realization arrows with hollow heads;
  - the labelled `writes` association.

  Pele's styles survived the packaged app's CSP and sanitizing, on the Intel machine the app ships for. That was the S5 step left to S9 (`docs/m2-s5-csp.md`).
- **`rendered`.** The same `panel_status` check through `combine mcp` then returned:

```json
{ "panel": "panel-1048577", "revision": 1048579, "state": "validated", "error": null,
  "render_status": [ { "revision": 1048579, "status": "rendered", "error": null } ] }
```

## Notes

- **The terminal list.** The space's navigator listed the agent's terminal as `sh /Users/jcarreira/m2demo/s9-run-agent.sh`, `exited`, since the headless run had finished.
- **The app's look.** The app is still unstyled: system serif type and plain buttons. The brand restyle is planned as the next slice, after the logo and app-icon change (#108). Panel sets and loader-backed panels are being planned in #107.
- **`work-mbp`.** It wasn't updated for M2. Updating it means installing the protocol 7 `combined` there, as on the mini.
