# Working in Combine

Combine is one desktop window over the herdr-core agent terminals on each of your machines, plus panels the agents build next to them. `SPEC.md` is the source of truth for what to build: read the relevant section before starting work, and update it in the same change when a decision changes.

## Before writing code

- **Terminal backend:** herdr-core, the `hc` CLI. Read its API with `hc schema`. Ignore the older `herdr` TUI app and its `herdr api schema`.
- **Architecture rules:** `.trellis/rules.toml` records the layering in SPEC.md, "Repository layout and architecture rules". Put new files where that layout says, and run `trellis rules check` before you finish.
  - Fix any new deviation, or raise it with the owner. Never add an `[[exceptions]]` entry or move a path between layers just to make a check pass.
  - A clean check is not the whole answer: trellis exits 0 on `unknown` and unassigned references. Review new ones on the boundaries in SPEC.md's architecture table, and every production file that lands in no layer.
  - The table's "architecture guard" rows aren't checked by anything yet. Check them by hand in your change.
- **Package names:** `combined`, `combine`, `combine-app` and `combine-web` are the app names `rules.toml` uses; `combine-protocol` is a library, mapped as a shared layer. Don't rename any of them without updating `rules.toml`.
- **Test support:** `test_features = ["test-support"]` marks reference sites as test only where the `cfg(feature = "test-support")` gate is on the code itself. A gate on `mod testing;` doesn't carry into `testing.rs`, so start each support file with `#![cfg(feature = "test-support")]` as well (trellis `docs/rules.md`, Amendment #988).
- **M0** lives in `spike/`, outside the Cargo workspace. Nothing may depend on it.

## Shared heavy-run lock

This machine runs several projects' builds and test suites. Heavy work is serialized through one shared lease, used by Trellis, mimir, Combine and other projects:

    /Users/jcarreira/projects/squashmerge/trellis-evidence/heavy-lock/heavy.sh <command> [args...]

- **Heavy work goes through heavy.sh:**
  - the full test suite (`cargo test --workspace`, `cargo nextest run`);
  - release builds, including Tauri app bundles;
  - whole-workspace `cargo clippy --all-targets`;
  - benchmarks and any timing measurement, so another project's load doesn't skew your numbers. M0's keystroke-latency and output-burst measurements count;
  - anything else that saturates the CPU for more than a minute or two.

  Example: `heavy.sh cargo nextest run --workspace`.
- **Light work runs directly:** `cargo check`, `cargo fmt`, single targeted tests (`cargo test some_test`) and quick builds of one crate.
- **It queues.** The wrapper waits for the lease, then runs the command. The 4-hour timeout covers waiting and running together. Start long runs in the background and wait for them to finish. Don't poll in a tight loop.
- **Never touch the lease directory** `~/.mimir-heavy-locks/heavy.lock.lease`: don't delete, rename or "break" it. A stale lease (a holder that died) is broken automatically after 10 minutes without a heartbeat.
- **No nesting.** Wrap the outermost command only. Inside a wrapped run `MIMIR_HEAVY_LOCK_HELD` is set, and nested calls run directly.
- **Interrupting:** Ctrl-C or SIGTERM on heavy.sh kills the command's process group and releases the lease. Never SIGKILL the wrapper: the child keeps running unlocked.
- Hold the lease for one command at a time. Don't wrap an interactive shell or a long idle wait.
- **Scripts:** run a test or check script whole through heavy.sh (e.g. `heavy.sh ./tools/verify` once it exists). Don't wrap only some of the steps inside it.
- **Build directories:** give each checkout or worktree its own `CARGO_TARGET_DIR` under `/Users/jcarreira/projects/squashmerge/combine-targets/<checkout-name>`, as Trellis does with `trellis-targets/`. Never share one between checkouts. The lock limits CPU use, not disk, so watch free space too.
- **Agents in a container or VM** can only take part if they see the same `~/.mimir-heavy-locks` directory through a bind mount. The protocol allows this, but it needs mounting and setting up.
