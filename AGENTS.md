# Working in Combine

Combine is one desktop window over the herdr-core agent terminals on each of your machines, plus panels the agents build next to them. `SPEC.md` is the source of truth for what to build: read the relevant section before starting work, and update it in the same change when a decision changes.

## Before writing code

- **Terminal backend:** herdr-core, the `hc` CLI. Read its API with `hc schema`. Ignore the older `herdr` TUI app and its `herdr api schema`.
- **Architecture rules:** `.trellis/rules.toml` records the layering in SPEC.md, "Repository layout and architecture rules". Put new files where that layout says, and run `trellis rules check` before you finish.
  - Fix any new deviation, or raise it with the owner. Never add an `[[exceptions]]` entry or move a path between layers just to make a check pass.
  - A clean check is not the whole answer: trellis exits 0 on `unknown` and unassigned references. Review new ones on the boundaries in SPEC.md's architecture table, and every production file that lands in no layer.
- **Architecture guard:** `python3 -I tools/arch-guard/check.py` checks what trellis can't: member-crate dependency bans, I/O in core, JS-engine and Tauri imports outside their one home, listeners and PTYs, frontend source language, and production code reaching `spike/` or test paths. Run it before you finish; it must be clean. Its rules are in `tools/arch-guard/guard.toml`; change them only as part of a reviewed change to SPEC.md's architecture table. If you change `check.py`, run `python3 -I tools/arch-guard/test_check.py`.
- **npm dependencies:** declare every package the frontend imports in `app/web/package.json`. trellis proves an npm identity only when the nearest `package.json` declares it; an undeclared import is `unknown` and passes.
- **Retiring guard rules:** when a trellis issue from `docs/trellis-feature-request.md` lands, delete the guard rule it replaces in the same change that adds the `rules.toml` check, and move the fixture over.
- **Package names:** `combined`, `combine`, `combine-app` and `combine-web` are the app names `rules.toml` uses; `combine-protocol` is a library, mapped as a shared layer. Don't rename any of them without updating `rules.toml`.
- **Test support:** `test_features = ["test-support"]` marks reference sites as test only where the `cfg(feature = "test-support")` gate is on the code itself. A gate on `mod testing;` doesn't carry into `testing.rs`, so start each support file with `#![cfg(feature = "test-support")]` as well (trellis `docs/rules.md`, Amendment #988).
- **M0** lives in `spike/`, outside the Cargo workspace. Nothing may depend on it. Its findings are in `spike/m0/FINDINGS.md`; SPEC.md rev 9 records the decisions taken from them.
- **Never test against the default herdr-core.** It runs the agents working on this repo, including you. Coordinating with other agents through it is fine and expected (`hc send`, `hc read`, `hc ls`, `hc get`, `hc wait`). Never restart, stop, kill, rm or attach to its terminals, and never point Combine code or tests at it. Every test, probe or tool that talks to a real herdr-core uses an isolated core: its own `HC_SESSION` and scratch XDG paths under a short `/tmp` path (macOS caps socket paths at 103 bytes). `spike/m0/env.sh` shows the setup. Before any restart, stop, kill or rm, check that `hc server status --json` reports a core id other than the default's. Code that connects to herdr-core takes the socket path explicitly, and tests refuse the default socket. herdr-core's own CONTRIBUTING.md asks the same: "Tests use isolated sessions; don't point them at a running core."

## Shared heavy-run lock

This machine runs several projects' builds and test suites. Heavy work is serialized through one shared lease with two slots, used by Trellis, mimir, Combine and other projects. The protocol is in `trellis-evidence/heavy-lock/README-v2.md`. There are two wrappers:

    /Users/jcarreira/projects/squashmerge/trellis-evidence/heavy-lock/heavy-medium.sh <command> [args...]
    /Users/jcarreira/projects/squashmerge/trellis-evidence/heavy-lock/heavy.sh <command> [args...]

- **Medium (`heavy-medium.sh`)** takes one slot, so two medium jobs can run at once. Use it for builds and checks:
  - `./tools/verify` (it takes the medium tier itself);
  - the full test suite (`cargo test --workspace`, `cargo nextest run`);
  - whole-workspace `cargo clippy --all-targets`;
  - release builds, including Tauri app bundles;
  - anything else that saturates the CPU for more than a minute or two.

  Example: `heavy-medium.sh cargo nextest run --workspace`.
- **Large (`heavy.sh`)** takes both slots and runs alone. Use it for benchmarks and any timing or memory measurement, so another job's load doesn't skew your numbers. M0's keystroke-latency and output-burst measurements and M1's latency test count. A job that names no tier counts as large.
- **Light work runs directly:** `cargo check`, `cargo fmt`, single targeted tests (`cargo test some_test`) and quick builds of one crate.
- **It queues.** The wrapper waits for a slot, then runs the command. A waiting large job stops new medium jobs from starting, so measurements aren't starved. The 4-hour timeout covers waiting and running together. Start long runs in the background and wait for them to finish. Don't poll in a tight loop.
- **Never touch the lease directories** under `~/.mimir-heavy-locks/` (`heavy.lock.lease`, `heavy.lock.lease2`, `heavy.lock.intent`): don't delete, rename or "break" them. A stale lease (a holder that died) is broken automatically after 10 minutes without a heartbeat.
- **No nesting.** Wrap the outermost command only. Inside a wrapped run `MIMIR_HEAVY_LOCK_HELD` is set, and nested calls run directly. A factory driver that wraps `factory observe` in `heavy-medium.sh` therefore runs `./tools/verify` inside that lease, and the lock wait doesn't count against the verify timeout. Nested wrappers don't upgrade the tier: a `heavy.sh` call inside a medium lease still holds one slot. If any step measures timing or memory, wrap the outermost command in `heavy.sh`, or run the measurement separately after the medium lease is released. Never unset `MIMIR_HEAVY_LOCK_HELD` to force an upgrade.
- **Interrupting:** Ctrl-C or SIGTERM on the wrapper kills the command's process group and releases the lease. Never SIGKILL the wrapper: the child keeps running unlocked.
- Hold the lease for one command at a time. Don't wrap an interactive shell or a long idle wait.
- **Scripts:** run a test or check script whole through the wrapper. Don't wrap only some of the steps inside it.
- **Build directories:** give each checkout or worktree its own `CARGO_TARGET_DIR` under `/Users/jcarreira/projects/squashmerge/combine-targets/<checkout-name>`, as Trellis does with `trellis-targets/`. Never share one between checkouts. The lock limits CPU use, not disk, so watch free space too.
- **Agents in a container or VM** can only take part if they see the same `~/.mimir-heavy-locks` directory through a bind mount. The protocol allows this, but it needs mounting and setting up.
