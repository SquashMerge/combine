# Trellis feature request: enforce the architecture rules Combine needs

Drafted 2026-11-20 from Combine (`SquashMerge/combine`), for filing against Trellis.

## Summary

Combine's `.trellis/rules.toml` records its architecture before any code exists. A review that ran negative fixtures against it found that `trellis rules check` passes (exit 0) on several violations the rules were written to catch. Every gap is documented Trellis behavior, not a bug: the policy language can't yet express these rules. Combine now covers them with a stopgap lexical checker, `tools/arch-guard`. This request asks for six Trellis features, each of which retires one or more of its rules.

| # | Feature | Retires `tools/arch-guard` rule(s) | Priority |
| --- | --- | --- | --- |
| F1 | External package identities for Cargo workspace **members** | `no-listener-crates`, `no-pty-crates`, `no-webview-shell-or-fs`, `js-engine-only-in-daemon` (and makes Combine's existing Rust selectors work) | High |
| F2 | Source-scoped external rules: "only these sources may use package X" | `js-engine-only-in-loader`, `tauri-only-in-ipc` | High |
| F3 | Module- and item-level external selectors (`std::fs`, `tokio::net::TcpListener`) | `core-does-no-io`, `no-listener-no-pty` | High |
| F4 | Non-production layers that every app's production code is forbidden to reach | `no-production-dependency-on-non-production` | Medium |
| F5 | Opt-in failure on `unknown` at protected boundaries (unassigned, `cross_app`) | none directly; closes the "exit 0 on unknown" hole | Medium |
| F6 | Layer source-language policy | `frontend-is-typescript` | Low |

All six follow Trellis's existing principles. Positive evidence is required for `deviates`, absence never proves conformance, uncertain cases stay `unknown`, and a rules file without the new keys behaves exactly as today. Several add closed top-level or table keys, so each needs an amendment to `docs/rules.md`.

## Reproduction

The review fixture is a copy of Combine's `rules.toml` over a four-app checkout: Cargo binaries `combined`, `combine` and `combine-app` in a virtual workspace, and npm app `combine-web`. These probes all exit 0:

| Source | Code | Rule it should break | Trellis result |
| --- | --- | --- | --- |
| `crates/combined/src/rpc.rs` | `use axum::Router;`, with `axum` in `crates/combined/Cargo.toml` | `[external_policy] deny` for `axum` | `unknown` (`rust_export_name_set_unknown`) |
| `crates/combined/src/main.rs` | `use rquickjs::Runtime;` | `rquickjs` assigned to the loader adapter layer | `unknown` |
| `crates/combined/src/core/x.rs` | `std::fs::read_to_string`, `std::process::Command::new`, `std::net::TcpListener::bind` | core does no I/O; no listener | `unknown` (`layer_unassigned`), not a violation |
| `app/web/src/main.ts` | `import { invoke } from '@tauri-apps/api/core'` | only `ipc/` calls Tauri | `conforms` (shell order 1 → ipc order 3 is downward) |
| `app/web/src/main.ts` | `import { helper } from '../../../spike/helper'` | production never depends on the spike | `unknown` (`cross_app`) |
| `app/web/src/renderers/direct.js` | Tauri API and `@tauri-apps/plugin-shell` imports | plugin ban; renderers can't reach Tauri | no destination layer, no ban (selectors were TypeScript only; Combine has since added JavaScript twins) |

Combine's `tools/arch-guard/test_check.py` encodes each probe as a fixture. Those fixtures can become Trellis acceptance tests.

## F1. External package identities for workspace members

**Today** (`docs/rules.md`, external mapping): "External package labels still come only from the root manifest: a workspace member's own registry dependencies, inherited dependencies without a member binding, custom roots and unproved ownership remain unknown." A virtual workspace root has no `[dependencies]`, so for Combine every Rust `external` selector and every Rust `deny` entry is inert.

**Proposal.** Bind Rust external identities per workspace member, from that member's captured `Cargo.toml`, for sites in files the member owns.

- **Ownership:** a file belongs to the member whose root contains it most specifically, using the member resolution Trellis already has (#772). Files no member proves it owns stay `unknown`, as today.
- **Bindings:** `[dependencies]`, `[build-dependencies]` and `[target.'<cfg>'.dependencies]` tables. The Cargo key is the binding name, with hyphen normalization; `package = "<name>"` gives the upstream identity, so `web = { package = "warp" }` binds `web` to `warp`.
- **Inheritance:** `foo = { workspace = true }` resolves through the root's `[workspace.dependencies]` entry, including that entry's `package` rename.
- **Roles:** unchanged from the root-manifest slice. `dev-dependencies` stay declaration evidence, not production, and section and target qualifiers don't prove activation.
- **Conflicts and path dependencies:** a key bound to two different identities across the member's tables stays `unknown`. A path dependency is an in-repo module, never an external package.
- **Evidence:** captured from the same admitted snapshot as source (no live reads, no Cargo invocation), within the existing manifest byte and work bounds.

**Acceptance:**
1. The `axum` probe above reports `$external` `deviates` and exits 1.
2. `rquickjs` used from `crates/combined/src/adapters/loader/` resolves to the external identity and conforms against the loader layer's `external` selector.
3. A workspace-inherited `tauri-plugin-shell = { workspace = true }` in `app/src-tauri/Cargo.toml` deviates against the ban.

## F2. Source-scoped external rules

**Today:** assigning an external package to a layer makes references from that layer, and from every higher layer of the same app, conform. "Only the loader adapter may use the JS engine" and "only `ipc/` imports Tauri" can't be written. The closest form, making the owning layer a peer, also bans legitimate callers above it.

**Proposal:** a new `[[external_rules]]` array of tables. It is a new closed top-level key, so it needs an amendment.

```toml
[[external_rules]]
id = "tauri-only-in-ipc"
select = [
  { language = "typescript", npm_scope = "@tauri-apps", stdlib = false },
  { language = "javascript", npm_scope = "@tauri-apps", stdlib = false },
]
allow_only_in = ["app/web/src/ipc/**"]
reason = "Only ipc/ calls Tauri."

[[external_rules]]
id = "js-engine-only-in-loader"
select = [
  { language = "rust", package = "rquickjs", stdlib = false },
  { language = "rust", package = "deno_core", stdlib = false },
]
allow_only_in = ["crates/combined/src/adapters/loader.rs", "crates/combined/src/adapters/loader/**"]
```

- **`select`** uses the existing external selector shape. `npm_scope` is an optional alternative to `package` that matches every package in one npm scope. It is a literal scope, not a glob.
- **`allow_only_in`** or **`forbid_in`** (exactly one) takes source path globs with the existing pattern rules.
- **Semantics:** each observed production edge from a source to a selected identity is one instance of the rule. It deviates when its source falls outside `allow_only_in`, or inside `forbid_in`, and conforms otherwise. An edge whose identity isn't proved stays `unknown`. Test-only sites are reported in `testOnly` and never fail the production check, as `$external` does today.
- **Reviewed exceptions:** module-to-module and declaration exceptions don't apply. An exception for an external rule needs its own reviewed shape (source path, identity, reason), or none at all, which matches `$external` today.

**Acceptance:**
1. The `main.ts` Tauri probe deviates.
2. `app/web/src/ipc/client.ts` importing `@tauri-apps/api/core` conforms.
3. `use rquickjs::Runtime;` in `crates/combined/src/main.rs` deviates (with F1).

## F3. Module- and item-level external selectors

**Today:** an external identity is a whole package, and `std`/`core`/`alloc` are reviewed root labels. "Core does no filesystem, process or network I/O" can't be written: banning `std` bans everything, and `tokio::fs` is indistinguishable from `tokio::sync`.

**Proposal:** an optional `path` field on Rust external selectors, naming a module or item inside the package:

```toml
[[external_rules]]
id = "core-does-no-io"
select = [
  { language = "rust", package = "std",   stdlib = true,  path = ["fs"] },
  { language = "rust", package = "std",   stdlib = true,  path = ["process"] },
  { language = "rust", package = "std",   stdlib = true,  path = ["net"] },
  { language = "rust", package = "std",   stdlib = true,  path = ["os", "unix", "net"] },
  { language = "rust", package = "tokio", stdlib = false, path = ["fs"] },
  { language = "rust", package = "tokio", stdlib = false, path = ["process"] },
  { language = "rust", package = "tokio", stdlib = false, path = ["net"] },
]
forbid_in = ["crates/combined/src/core/**", "crates/combined/src/ports/**", "crates/combined/src/model/**"]

[[external_rules]]
id = "no-listener"
select = [
  { language = "rust", package = "std",   stdlib = true,  path = ["net", "TcpListener"] },
  { language = "rust", package = "tokio", stdlib = false, path = ["net", "TcpListener"] },
]
forbid_in = ["**"]
```

- **Matching:** a site matches when its resolved path starts with the selector's path, segment by segment. That covers `use` trees with braces and aliases (`use std::{io, fs}`, `use std::process::Command as Cmd`), absolute paths (`::std::fs::read`), and qualified paths in expressions and types.
- **Evidence:** the path must be proved from syntax. A path reached through a re-export, a glob import of a parent (`use std::*; fs::read`), an alias of a crate root (`use std as s;`), or a macro expansion stays `unknown` with a named reason. It is never assumed to match or not match.
- **Scope:** this is name-path evidence, not a resolved API inventory, and it doesn't claim the item exists in a given toolchain version. That keeps the current statement that `std/core/alloc` are labels, not member inventories.

**Acceptance:**
1. Each spelling in Combine's `test_core_io_in_every_spelling` fixture deviates, at the right line.
2. Comments, string literals and `#[cfg(test)]` items are not production sites.
3. `tokio::net::UnixListener` doesn't match `no-listener`.

## F4. Non-production layers, enforced across apps

**Today:** `display = "hidden"` is presentation only. Order is compared only within an app, so a production reference from app A into a hidden layer of app B is `unknown` (`cross_app`) and never fails (ruling 4 of #783, pending #686). Path-based reaches aren't references at all: Cargo `path` dependencies, Rust `#[path = "..."]` and `include_str!`/`include_bytes!`, and JavaScript relative imports into non-module trees.

**Proposal:** a layer attribute that is policy, not presentation:

```toml
[layers.spike]
app = "combined"
display = "hidden"
production = false     # new: no production site in any app may reach this layer
order = 0
paths = ["spike/**"]
```

- **Reserved rule `$non_production`:** any production site, in any app, whose destination is a file in a `production = false` layer deviates. This applies before the `cross_app` rule, which stays as it is for production-to-production pairs.
- **Path-based references count as references for this rule only:** Cargo `path` dependencies (manifest site), `#[path]` attributes, the `include!` macro family with a literal argument, and relative JS/TS import specifiers. Each is resolved against the captured snapshot. A target that can't be resolved stays `unknown`.
- **Interaction with `test_features` (#988):** sites already classified as test are not production and don't count.

**Acceptance:**
1. The `main.ts` → `spike/helper` probe deviates.
2. `m0 = { path = "../../spike/m0" }` in a member's `[dependencies]` deviates; the same under `[dev-dependencies]` doesn't.
3. `include_str!("../../combined/tests/data.txt")` from `crates/combine/src/main.rs` deviates when `crates/*/tests/**` is a `production = false` layer.

## F5. Opt-in failure on `unknown` at protected boundaries

**Today:** `unknown` never fails `trellis rules check`. That keeps checks honest about uncertainty, but a policy can then silently pass. Two cases are common and avoidable:
- a production file that lands in no layer (`layer_unassigned`);
- a production edge into a protected layer that comes back `cross_app` or unresolved.

**Proposal:**
- `[layer_policy] unassigned = "forbid"` (default `"allow"`, today's behavior). A production source file under any app's root that no layer selects fails the check. It is reported per file, with its app.
- `trellis rules check --fail-on <reason>[,<reason>]` (for example `layer_unassigned` or `cross_app`). The selected `unknown` reasons fail the run. The JSON payload is unchanged; only the exit status and a summary line differ.

These are opt-in, so existing checkouts keep today's semantics. Combine would turn on `unassigned = "forbid"` from its first commit, since every production path is planned in advance.

**Acceptance:**
1. A new `crates/combined/src/adapters/mod.rs` that matches no layer fails with `unassigned = "forbid"` and passes without it.

## F6. Layer source-language policy

**Today:** layers select paths, and external selectors carry a language. A `.js` file in a TypeScript frontend gets JavaScript identities that TypeScript-only selectors never see. Combine has worked around this by duplicating every selector for both languages.

**Proposal:** an optional `languages = ["typescript"]` on a layer. A source file the layer selects whose detected language isn't listed is one `$layers` instance that deviates, with reason `language_not_allowed`.

**Acceptance:**
1. `app/web/src/renderers/direct.js` deviates in a layer with `languages = ["typescript"]`.

## Compatibility and wire shape

- New closed keys: `[[external_rules]]` at top level, and `path`/`npm_scope` in external selectors. New layer keys `production` and `languages`, and `layer_policy.unassigned`. Each needs a located `invalid_rules` schema error for bad values, and a rules file without them is unchanged.
- New reserved rule IDs: `$non_production` and one per `[[external_rules]]` entry (its `id`). They use the existing `rules:v2` instance shape, with exact counts, bounded samples and production/test roles.
- All new evidence is read from the admitted snapshot, within the existing work bounds, and fails atomically with `bounded_work` on exhaustion.

## Suggested order

F1 first: it is a mapping change inside an existing contract, and it makes Combine's current Rust selectors and bans work. Then F2 and F3 together, since they share the `[[external_rules]]` table. Then F4 and F5, which close the silent-pass holes. F6 last: the selector duplication workaround is cheap.

When a feature lands, Combine deletes the matching `tools/arch-guard` rule and moves its fixture into a `rules.toml` check.
