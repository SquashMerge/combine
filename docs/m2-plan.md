# M2 plan: agent-built diagrams

This is the working plan for SPEC.md milestone 2 (rev 29). SPEC.md is the source of truth for behavior; this file only orders the work.

M2 covers "MCP server and shim, `create_panel` / `update_panel` / `show_diagram` / `panel_status` with revisions and states, Mermaid with Pele and Markdown renderers, headless validation" (SPEC roadmap). M0 handed it this done criterion: **M2 is done when an agent creates a class diagram panel and fixes its own Mermaid errors with no client attached.**

## What M2 builds on

M1 provides:
- `combine-protocol` v4 (layout, templates, spawn);
- `core::panels`, which reads terminal `panel.json` files only, with an 8 KiB bound;
- `FileStore` with `list_dir`, `watch` and hashes;
- the layout engine;
- the ID reservation (`ids.json`);
- the Tauri host router;
- a `TileTree` that shows non-terminal panel IDs as bare labels.

Not yet in the repo:
- an MCP server (the `combine` shim crate is an empty `main`);
- a loader or JS-engine adapter;
- Pele, DOMPurify or a Markdown renderer.

There is also no layout op that adds or removes a panel ID: `move_panel` refuses an ID that isn't already in the tree.

## Slices

Every slice ends with:
- a clean `trellis rules check`;
- a clean `python3 -I tools/arch-guard/check.py`;
- `./tools/verify` passing;
- an opencode review before any slice that depends on it starts.

| # | Slice | Scope | Layers | Depends on | Done when |
| --- | --- | --- | --- | --- | --- |
| M2-S1 | **Wire protocol v5: panels and the MCP channel** | Version 5: panel messages, the `add_panel` and `remove_panel` layout ops, the `mcp_open` / `mcp` channel, and new error codes (see "Proposed v5 messages") | `protocol` | — | <ul><li>Round-trip, malformed-frame and bounded-allocation tests pass for every new message.</li><li>A version mismatch reads `client 5, daemon 4`.</li><li>A 200 KB Mermaid source either fits the 1 MiB frame cap or fails with a typed `EncodeError`.</li><li>The protocol-neutrality test still passes.</li></ul> |
| M2-S2 | **Headless Mermaid validation in the isolate.** It starts with a spike. | A `DiagramCheck` port with three results: `validated`, `validated_parser_only` and `failed { code, message, line, column, snippet }`. It is implemented in `adapters/loader/` with `rquickjs` and an exact-pinned, vendored Pele. The SPEC limits apply: 200 KB, 10 s, 64 MB. | `daemon-ports`, `adapter-loader` (the only place a JS engine may live), `daemon-entry` | — | <ul><li>A class-diagram fixture returns `validated`.</li><li>Five broken fixtures each return `failed` with a line and column.</li><li>A 200 KB+1 source is refused before the isolate runs.</li><li>The time and memory caps stop a run without killing the daemon.</li><li>The vendored Pele's SHA-256 equals the npm artifact's.</li><li>A `heavy.sh` benchmark records the cold load and the warm p50 and p95 for a 50-class diagram and a 200 KB one. If it misses the 5 s tool wait, fall back to `deno_core` or to parser-only.</li></ul> |
| M2-S3 | **Panels in the daemon: model, revisions, states, storage** | The view-panel spec with renderer `mermaid` or `markdown` and static content, which lives in a file next to `panel.json` (see D9). Daemon states, and revisions that never go backwards. Idempotency keys. Invalid files keep their last good spec. Outside edits are detected by hash. Panel files are watched: this reverses SPEC's "never watches panel files". The scratch area under `<daemon data>/scratch/` expires after 7 days. At most 100 panels per space. Validation runs off the server loop. Everything here works with no client connected. | `daemon-core`, `daemon-ports`, model, `adapter-fs`, `daemon-test-support`, `daemon-entry` | S1, plus S2's port signature | <ul><li>With zero clients connected: create goes to `validating`, then to `validated` or `failed`; update bumps the revision; a late result never overwrites a newer revision; an outside edit triggers a new validation; invalid JSON is kept and never rewritten; the 101st panel is refused; the scratch lifecycle works.</li><li>The M1 spawn and template tests still pass.</li></ul> |
| M2-S4 | **The MCP server and the `combine mcp` shim** | The shim reads `HC_TERMINAL_ID` and `HC_TERMINAL_SOCKET`, with the cwd as a fallback, and connects over `combined.sock`, never ssh. Tools: `create_panel`, `update_panel`, `show_diagram`, `panel_status`, `list_panels`, `remove_panel`. A tool waits about 5 s, then answers `validating`. It records `created_by` and writes the audit log. It implements the MCP subset by hand (D7). A `CallerIdentity` port is implemented by the herdr-core adapter. | `cli`, `daemon-entry` (`mcp_server/`, `rpc/`), `daemon-ports`, `adapter-herdr-core`, `daemon-core` | S1, S3 (S2 for real validation) | End to end, with an in-process daemon and the real `combine mcp` binary over stdio: <ul><li>`tools/list` returns exactly the six tools.</li><li>Broken Mermaid gives `failed` with a line and column within 5 s.</li><li>An update with the same ID gives `validated`.</li><li>Idempotency holds.</li><li>Caller resolution and its fallback work.</li><li>A slow validator gives `validating`, and `panel_status` later gives `validated`.</li><li>Terminal output keeps flowing during the wait.</li></ul> |
| M2-S5 | **Panels reach the client** | `rpc` handlers. Tauri routing and IPC. A `ViewPanel` host that fetches the visible revision and reports `rendered` or `render_failed` for each revision. Tab titles. Dangling IDs show as missing. | `daemon-entry`, `daemon-core`, `app-entry`, `app-hosts`, `web-ipc`, `web-model`, `web-panels`, `web-shell` | S1, S3 | <ul><li>An app integration test sees a panel created through core.</li><li>Webview tests cover titles, the missing state and revision ordering.</li><li>`render_failed` is visible through `panel_status`.</li><li>The packaged `Combine.app` shows a Mermaid and a Markdown panel with no CSP violations (moved from S6).</li></ul> |
| M2-S6 | **Mermaid and Markdown renderers** | Pele `render()` themed with the `--pele-*` variables. Markdown with `markdown-it` (`html: false`) and highlighted code blocks. All output goes through DOMPurify. Images are `data:` only, and links go through `open_url`, a minimal host command (see D17). Renderers load lazily with a literal `import()`. | `web-renderers`, `web-ipc`, `app-entry` | — | <ul><li>A class diagram renders.</li><li>Hostile fixtures are neutralized.</li><li>Remote images are removed.</li><li>`open_url` refuses everything except `https:` and `http:`.</li><li>Every fixture's sanitized output passes the CSP policy check; the packaged `Combine.app` check moved to S5 (#48).</li></ul> |
| M2-S7 | **Placement, scratch, close and pin** | With no client watching, the daemon loads the layout and adds the agent's panel as a tab in the hinted tile, or else the first tile. It never creates a tile. Closing a scratch panel deletes it and removes its ID. Pin moves it into `.workspace/`. The palette gains panel entries. `list_panels` reports dropped changes. | `daemon-core` (`layout_sync`), `daemon-entry`, `web-shell`, `web-model` | S3, S4, S5 | <ul><li>`show_diagram` with no client writes `layout.json`, and a client that connects later sees the tab.</li><li>Closing and pinning work.</li><li>The conflict policy applies to overlapping edits.</li></ul> |
| M2-S8 | **Slot-based terminal panels and terminal tabs** | `SpaceBinding` slots in `state.json`. `follow_active`. An exited terminal offers respawn or close, and respawn reuses the template hash for approval. Terminal tabs in tiles. The header dropdown and backend label. Deciding whether a terminal with no input owner should refit. Not part of the done criterion. | model, `daemon-core`, `adapter-fs`, `daemon-entry`, `protocol`, `app-*`, `web-*` | S5, S7 | <ul><li>Two slot-pinned panels can sit side by side.</li><li>`follow_active` follows.</li><li>A restart restores the bindings.</li><li>Respawn sends the template's hash.</li><li>Nothing spawns automatically.</li></ul> If it's too big for one slice, split it into S8a (daemon) and S8b (client). |
| M2-S9 | **M2 acceptance** | The done criterion | — | S1–S7 | <ul><li>A real agent runs in an **isolated** demo core, with `combine mcp` registered per project only.</li><li>With no client attached, it creates a class diagram, gets `failed` with a line and column, and repairs **the same panel** until it reaches `validated`.</li><li>A client that attaches afterwards sees `rendered`.</li><li>The results are recorded in `docs/m2-acceptance.md`.</li></ul> |

**Proposed v5 messages.** SPEC names none, so all of these are inferred:
- `panels { request, space, panels, skipped, error }`, pushed while a client watches a space's layout;
- `get_panel` and its reply `panel { … revision, renderer, source, state, error }`;
- `report_render`, which has no reply;
- `close_panel` and `pin_panel`;
- the layout ops `add_panel` and `remove_panel`;
- `mcp_open { caller }`, answered by `mcp_opened { space }`, after which `mcp { message }` carries one JSON-RPC message per frame;
- the error codes `unknown_panel`, `unknown_caller` and `panel_limit`.

S1 fixed them in SPEC rev 23 (Wire protocol, Version 5 messages), which is now their source.

**Order:**
1. S1, S2 (spike first) and S6 run in parallel.
2. S3 after S1.
3. S4 and S5 in parallel.
4. S7.
5. S8 and S9 in parallel.

The shortest path to the done criterion is S1 → S2 → S3 → S4 → S9.

## How the work runs

- **One GitHub issue per slice**, in milestone M2, each run through `/feature-factory:feature <issue>`. S1 is the pilot. The issues: S1 #44, S2 #45, S3 #46, S4 #47, S5 #48, S6 #49, S7 #50, S8 #51, S9 #52.
- **One worktree and one `CARGO_TARGET_DIR` per slice.** S2's benchmark runs through `heavy.sh`.
- **Real cores are isolated.** S9's agent runs in an isolated core. Never register `combine mcp` in the user-global agent config: every agent on the machine, including those on the default core, would reach the daemon. Use a project `.mcp.json` or `--mcp-config`.

## Decisions

The owner decided these on 2026-11-23. They take the plan's recommendations unless noted otherwise.

- **D1. Where validation runs.** In the daemon, inside the isolate in `adapters/loader/`, behind a `DiagramCheck` port. SPEC requires validation with no client attached, and the JS-engine rule (arch-guard `js-engine-only-in-loader`) puts the engine in this adapter. The client only reports render status.
- **D2. Which JS engine.** `rquickjs`, pinned exactly. S2's benchmark confirms it; if Pele misses the 5 s wait on a 200 KB diagram, fall back to `deno_core`. M3's loaders use the same engine. This closes SPEC's open question on `rquickjs` versus `deno_core` once S2 confirms. S2 outcome: confirmed, rquickjs 0.14.0 (docs/m2-s2-spike.md, docs/m2-s2-bench.md).
- **D3. Pele and Mermaid versions.** Pin the upstream npm `pele` exactly, at 1.10.0 or later. Vendor the same artifact, with its LICENSE and SHA-256, under `adapters/loader/`; a test checks that the two hashes match. Don't use Trellis's patched build. **Spec gap:** SPEC says "Mermaid 12.1.0 syntax", and Pele doesn't name a Mermaid version. S2 confirms the syntax version and fixes SPEC and the tool descriptions. S2 outcome: Pele 1.13.0, whose README targets Mermaid 12.1.0, so SPEC stands.
- **D4. Render or parse only.** Full `render()` is expected, and S2 proves it. Pele measures text without a DOM and reports `PeleError { code, line, column, snippet }`. If `render()` can't run headless, `parse()` gives `validated (parser only)`. This closes the open question. S2 outcome: full render() runs headless.
- **D5. How the MCP channel connects.** Over `combined.sock` (wire v5, `mcp_open` then `mcp` frames), on the same host, never over ssh. The shim duplicates the daemon's socket lookup, and a test pins that the two agree. SPEC should say "the daemon's Unix socket".
- **D6. Connection cap.** MCP connections get their own cap, separate from the 64 client connections. SPEC changes in S4. S4 outcome: 16 MCP connections besides the 64 client connections (SPEC rev 27).
- **D7. MCP library.** `rmcp`, pinned exactly, if its transport fits our frames. Otherwise implement only `initialize`, `tools/list`, `tools/call` and `ping`. S4 outcome: a hand-written subset in `mcp_server/`; `rmcp`'s transports carry byte streams, not whole-message frames, and its parsing has no bound.
- **D8. Caller identity.** A `CallerIdentity` port, implemented by the herdr-core adapter. `HC_TERMINAL_ID` is accepted only when `HC_TERMINAL_SOCKET` equals the daemon's herdr-core socket. Otherwise the daemon falls back to matching the caller's cwd.
- **D9. Where panel content lives.** In a file next to `panel.json` (`diagram.mmd` or `content.md`). `panel.json` stays small, under its 8 KiB bound. Agents edit diagrams with ordinary file tools. SPEC's Persistence section lists the file.
- **D10. Markdown limits.** 200 KB, as for Mermaid. For Markdown, "validated" means within that size and valid UTF-8.
- **D11. Revisions and idempotency.**
  - Revisions come from the never-repeating `ids.json` counter.
  - An idempotency key is stored in the panel's own `panel.json` (`idempotency_key`), scoped to its space. A repeated `create_panel` with the same key finds that panel and returns it. That is what SPEC promises: "A repeated call with the same idempotency key returns the same panel".
    - The key survives daemon restarts, because it is in the file.
    - There is nothing to evict: the lookup scans the space's panels, at most 100.
    - The guarantee lasts as long as the panel. Once a panel is removed, or a scratch panel expires, a new call with its key creates a new panel. S4 states this in SPEC and in the tool description.
  - For an older revision, `panel_status` returns the current revision marked `superseded`.
  - The decision was revised on 2026-11-23 after review (opencode-39): the in-memory map capped at 1,024 keys would have weakened SPEC's guarantee across restarts.
- **D12. `created_by`.** `{ "type": "agent", "slot": <slot or null> }`. Repo files never hold a TermId.
- **D13. Placement.** A tab in the tile the hint names, or else the first tile in tree order. No new tiles in M2. Scratch IDs leave `layout.json` when the panel is deleted. S4 stores a create's hint as `placement` in `panel.json`; S7 applies it.
- **D14. Terminal tabs.** They are in M2 as S8, but not part of the done criterion. S8 also decides whether a terminal with no input owner should refit.
- **D15. Which tools M2 exposes.** `create_panel`, `update_panel`, `show_diagram`, `panel_status`, `list_panels`, `remove_panel`. Later tools stay hidden.
- **D16. What `create_panel` makes.** Mermaid or Markdown with static content only. `kind`, `loader` and live sources are refused with an error naming the milestone that adds them. Terminals can't be created through MCP.
- **D17. Links.** A minimal `open_url` host command. Rust checks the scheme (`https:` or `http:` only), and it opens on a click only, through `/usr/bin/open` with no shell.
- **D18. Markdown libraries.** `markdown-it` with `html: false`, then DOMPurify, plus `highlight.js` core with a few languages. All are pinned exactly, declared in `package.json`, and added to the `web-renderers` external list in `rules.toml`.
- **D19. Render status.** It is kept per connection, in memory, and dropped on disconnect. S5 outcome: kept per connection in `core/render_status.rs`, at most 1,024 panels per connection, the least recently reported dropped first; reports for a newer-than-current revision, or an unknown or removed panel, are ignored (SPEC rev 28).
- **D20. How the panel list reaches a client.** It rides on `watch_layout`: no new subscription. S5 outcome: `panels` follows the `layout` answer and is pushed after any change of the watched space's panels, trimmed to one frame (SPEC rev 28).
- **D21. Trust for static panels.** M2 never produces `untrusted`, because static Mermaid and Markdown run no code. Repo-origin static panels render sanitized, without approval. State this in SPEC.

**SPEC fixes that come with these slices:**
- The state list gains `removed` and `validated (parser only)` (S3).
- The scratch path uses the SHA-256 form, like fallback layouts (S3).
- "The daemon never watches panel files" is reversed (S3).
- `active_terminal` becomes a slot (S8).

## Risks

- **Pele under QuickJS.** Compatibility, the cold parse of a ~712 KB bundle, and staying within the 10 s and 64 MB caps and the 5 s tool wait are all unproven. S2 measures them first.
- **The JS-engine boundary.** Validator code outside `adapters/loader/` fails arch-guard.
- **Drift between the two copies of Pele.** The daemon's vendored copy and the web's npm copy must be the same artifact. The hash test guards this.
- **Bundle size.** Pele is about 265 KB gzipped. Mitigate with lazy literal imports and a short list of highlighted languages.
- **The CSP.** DOMPurify's SVG profile may strip Pele's styles. jsdom doesn't apply the CSP; that is how M1 shipped a CSP bug. S5 and S9 check in the packaged app. Outcome: S5's probe build and the x86_64 bundle check (`docs/m2-s5-csp.md`).
- **Undeclared npm imports.** trellis rates them `unknown`, which passes. Declare every renderer package.
- **Agent file edits racing MCP writes.** Check the hash before every panel and layout write (the #34 finding).
- **The shim's lifetime and the connection cap.** A shim must survive a daemon restart, or fail clearly.
- **Daemon-made layout writes with no watcher.** These are new. The debounce and shutdown flushes must cover them.
- **A panel reply too big for one frame.** `write_one` (`crates/combined/src/rpc/connection.rs:169-185`) replaces a message it can't frame with an internal error carrying `request: null`. So a panel reply whose 204,800-byte source escapes past the 1 MiB frame (control characters at 6 bytes each) would leave its `get_panel` unanswered. S3 refuses a source whose escaped form can't fit one panel frame, or S5 gives that error the reply's `request`. S4 handles an `mcp` reply too big for one frame the same way. Outcome: S3 refuses such a source; S5's `write_one` also names the request of a `panel` or `panels` that can't fit, and the client keeps at most 4 `get_panel` in flight per host.
- **Memory and depth when parsing `mcp.message`.** The codec keeps `mcp.message` as text and accepts any nesting depth. So S4 bounds memory and depth when it parses the message into JSON-RPC types, because a tree of a full frame can hold over 100 MiB.

## Open issues

| Issue | Where it goes |
| --- | --- |
| #34 (S7 follow-ups) | **S3:** the hash check before writing, and re-arming the macOS watcher. **S5:** the watch races and the writer items. **S7:** the palette and missing-panel items. The rest stays separate. |
| #37 (S8 follow-ups) | **S8:** the auto-open loop guard and the approval-line display. The rest stays separate. |
| #38 (S6 follow-ups) | **S8:** the terminal dropdown, the backend label and the stream-open failure state. **S7:** the disconnected-tree items. The rest stays separate. |
| #43 (navigator labels) | S8, or an early one-line PR. |
| #27, #41, #42 | Separate from M2. |
