#!/usr/bin/env python3
"""Tests for the architecture guard: python3 -I tools/arch-guard/test_check.py

Each test builds a small checkout in a temporary directory and runs the guard with the
real tools/arch-guard/guard.toml. The negative cases include every probe from opencode's
review of 2026-11-20 that trellis let through.
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import textwrap
import unittest
from unittest import mock
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONFIG = HERE / "guard.toml"
_spec = importlib.util.spec_from_file_location("arch_guard_check", HERE / "check.py")
check = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = check
_spec.loader.exec_module(check)

WORKSPACE = """\
[workspace]
resolver = "2"
members = ["crates/*", "app/src-tauri"]
exclude = ["spike"]
"""


def package(name: str, deps: str = "") -> str:
    return f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2021"\n\n{deps}'


BASE = {
    "Cargo.toml": WORKSPACE,
    "crates/combined/Cargo.toml": package("combined"),
    "crates/combined/src/main.rs": "fn main() {}\n",
    "crates/combine/Cargo.toml": package("combine"),
    "crates/combine/src/main.rs": "fn main() {}\n",
    "app/src-tauri/Cargo.toml": package("combine-app"),
    "app/src-tauri/src/main.rs": "fn main() {}\n",
    "app/web/package.json": '{"name": "combine-web"}\n',
    "app/web/src/main.ts": "export {};\n",
}


class GuardTest(unittest.TestCase):
    def guard(self, files: dict[str, str]) -> set[tuple[str, str]]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, text in {**BASE, **files}.items():
                path = root / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(textwrap.dedent(text))
            return {(v.rule, v.path) for v in check.run(root, CONFIG)}

    def assertFlags(self, files, *expected):
        self.assertEqual(self.guard(files), set(expected))

    # --- clean baseline

    def test_scaffold_is_clean(self):
        self.assertFlags({})

    # --- Cargo dependencies in workspace members (trellis: unknown, exit 0)

    def test_banned_member_dependency(self):
        self.assertFlags(
            {"crates/combined/Cargo.toml": package("combined", '[dependencies]\naxum = "0.7"\n')},
            ("no-listener-crates", "crates/combined/Cargo.toml"),
        )

    def test_banned_dependency_renamed_and_in_target_table(self):
        deps = """\
        [dependencies]
        web = { package = "warp", version = "0.3" }

        [target.'cfg(unix)'.dependencies]
        portable-pty = "0.8"
        """
        self.assertFlags(
            {"crates/combined/Cargo.toml": package("combined", textwrap.dedent(deps))},
            ("no-listener-crates", "crates/combined/Cargo.toml"),
            ("no-pty-crates", "crates/combined/Cargo.toml"),
        )

    def test_banned_dependency_inherited_from_workspace(self):
        self.assertFlags(
            {
                "Cargo.toml": WORKSPACE + '\n[workspace.dependencies]\ntauri-plugin-shell = "2"\n',
                "app/src-tauri/Cargo.toml": package("combine-app", "[dependencies]\ntauri-plugin-shell = { workspace = true }\n"),
            },
            ("no-webview-shell-or-fs", "app/src-tauri/Cargo.toml"),
        )

    def test_dev_dependency_is_not_production(self):
        self.assertFlags({"crates/combined/Cargo.toml": package("combined", '[dev-dependencies]\nportable-pty = "0.8"\n')})

    def test_js_engine_dependency_outside_daemon(self):
        self.assertFlags(
            {"app/src-tauri/Cargo.toml": package("combine-app", '[dependencies]\nrquickjs = "0.6"\n')},
            ("js-engine-only-in-daemon", "app/src-tauri/Cargo.toml"),
        )

    def test_path_dependency_on_spike(self):
        self.assertFlags(
            {"crates/combined/Cargo.toml": package("combined", '[dependencies]\nm0 = { path = "../../spike/m0" }\n')},
            ("no-production-dependency-on-non-production", "crates/combined/Cargo.toml"),
        )

    # --- Rust source

    def test_core_io_in_every_spelling(self):
        core = """\
        use std::{io, fs};
        use std::process::Command as Cmd;
        use tokio::net::{TcpStream as _};
        pub fn load() { let _ = ::std::fs::read_to_string("x"); }
        """
        self.assertEqual(
            sorted(v.line for v in self._violations({"crates/combined/src/core/space.rs": core})),
            [1, 2, 3, 4],
        )

    def test_ports_and_model_too(self):
        self.assertFlags(
            {
                "crates/combined/src/ports.rs": "pub fn f() { std::net::TcpStream::connect(\"x\"); }\n",
                "crates/combined/src/model/mod.rs": "use tokio::fs;\n",
            },
            ("core-does-no-io", "crates/combined/src/ports.rs"),
            ("core-does-no-io", "crates/combined/src/model/mod.rs"),
        )

    def test_adapters_may_do_io(self):
        self.assertFlags({"crates/combined/src/adapters/fs/store.rs": "use std::fs;\nuse tokio::process::Command;\n"})

    def test_comments_strings_and_tests_are_ignored(self):
        core = """\
        // std::fs is banned here
        /* use std::process; /* nested */ still a comment */
        const DOC: &str = "std::net::TcpListener";
        const RAW: &str = r#"use std::fs;"#;
        pub fn ok<'a>(x: &'a str) -> char { let _ = x; 'f' }

        #[cfg(test)]
        mod tests {
            use std::fs;
            #[test]
            fn t() { std::process::Command::new("ls"); }
        }

        #[cfg(any(test, feature = "test-support"))]
        pub fn helper() { let _ = std::fs::read("x"); }
        """
        self.assertFlags({"crates/combined/src/core/clean.rs": core})

    def test_inner_test_support_gate_skips_file(self):
        src = '#![cfg(feature = "test-support")]\nuse std::fs;\n'
        self.assertFlags({"crates/combined/src/core/fixture.rs": src})

    def test_js_engine_outside_loader_adapter(self):
        self.assertFlags(
            {
                "crates/combined/src/main.rs": "use rquickjs::Runtime;\nfn main() {}\n",
                "crates/combined/src/adapters/loader/engine.rs": "use rquickjs::Runtime;\n",
            },
            ("js-engine-only-in-loader", "crates/combined/src/main.rs"),
        )

    def test_listener_and_pty_anywhere(self):
        self.assertFlags(
            {
                "crates/combined/src/rpc.rs": "use std::net::TcpListener;\nfn f() { TcpListener::bind(\"0.0.0.0:1\"); }\n",
                "app/src-tauri/src/hosts/pty.rs": "extern \"C\" { fn openpty(); }\n",
                "crates/combined/src/adapters/fs/sock.rs": "use tokio::net::UnixListener;\n",
            },
            ("no-listener-no-pty", "crates/combined/src/rpc.rs"),
            ("no-listener-no-pty", "app/src-tauri/src/hosts/pty.rs"),
        )

    def test_rust_path_attribute_into_spike(self):
        self.assertFlags(
            {"crates/combined/src/main.rs": '#[path = "../../../spike/helper.rs"]\nmod helper;\nfn main() {}\n'},
            ("no-production-dependency-on-non-production", "crates/combined/src/main.rs"),
        )

    def test_include_str_of_test_fixture(self):
        self.assertFlags(
            {"crates/combine/src/main.rs": 'const X: &str = include_str!("../../combined/tests/data.txt");\nfn main() {}\n'},
            ("no-production-dependency-on-non-production", "crates/combine/src/main.rs"),
        )

    # --- Frontend

    def test_tauri_outside_ipc(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": "import { invoke } from '@tauri-apps/api/core';\n",
                "app/web/src/panels/host.ts": "const m = await import(\"@tauri-apps/api/event\");\n",
                "app/web/src/ipc/client.ts": "import { invoke } from '@tauri-apps/api/core';\nexport { invoke };\n",
            },
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
            ("tauri-only-in-ipc", "app/web/src/panels/host.ts"),
        )

    def test_javascript_renderer_with_plugin(self):
        src = "import { invoke } from '@tauri-apps/api/core';\nconst sh = require('@tauri-apps/plugin-shell');\n"
        self.assertFlags(
            {"app/web/src/renderers/direct.js": src},
            ("frontend-is-typescript", "app/web/src/renderers/direct.js"),
            ("tauri-only-in-ipc", "app/web/src/renderers/direct.js"),
            ("no-webview-shell-or-fs-plugin", "app/web/src/renderers/direct.js"),
        )

    def test_plugin_banned_even_in_ipc(self):
        self.assertFlags(
            {"app/web/src/ipc/fs.ts": "export * from '@tauri-apps/plugin-fs';\n"},
            ("no-webview-shell-or-fs-plugin", "app/web/src/ipc/fs.ts"),
        )

    def test_cross_app_import_of_spike(self):
        self.assertFlags(
            {"app/web/src/main.ts": "import { helper } from '../../../spike/helper';\n"},
            ("no-production-dependency-on-non-production", "app/web/src/main.ts"),
        )

    def test_commented_import_and_tests_are_ignored(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": "// import '@tauri-apps/api/core';\n/* require('@tauri-apps/plugin-fs') */\nexport {};\n",
                "app/web/test/main.test.ts": "import { invoke } from '@tauri-apps/api/core';\n",
                "spike/m0/client.js": "import '@tauri-apps/plugin-shell';\n",
            },
        )

    # --- Review 2 (opencode, 2026-11-20): Cargo renames, membership and table spellings

    def test_renamed_engine_used_outside_loader(self):
        self.assertFlags(
            {
                "crates/combined/Cargo.toml": package("combined", '[dependencies]\njs = { package = "rquickjs", version = "0.6" }\n'),
                "crates/combined/src/main.rs": "use js::Runtime;\nfn main() { let _ = Runtime::new(); }\n",
                "crates/combined/src/adapters/loader/engine.rs": "use js::Runtime;\n",
            },
            ("js-engine-only-in-loader", "crates/combined/src/main.rs"),
        )

    def test_renamed_engine_inherited_through_target_table(self):
        self.assertFlags(
            {
                "Cargo.toml": WORKSPACE + '\n[workspace.dependencies]\nengine = { package = "deno_core", version = "0.300" }\n',
                "crates/combined/Cargo.toml": package("combined", "[target.'cfg(unix)'.dependencies]\nengine = { workspace = true }\n"),
                "crates/combined/src/main.rs": "fn main() { let _ = engine::JsRuntime::new(Default::default()); }\n",
            },
            ("js-engine-only-in-loader", "crates/combined/src/main.rs"),
        )

    def test_extern_crate_alias_is_followed(self):
        self.assertFlags(
            {"crates/combined/src/main.rs": "extern crate rquickjs as qjs;\nfn main() { let _ = qjs::Runtime::new(); }\n"},
            ("js-engine-only-in-loader", "crates/combined/src/main.rs"),
        )

    def test_implicit_member_through_path_dependency(self):
        self.assertFlags(
            {
                "Cargo.toml": '[workspace]\nresolver = "2"\nmembers = ["crates/combined"]\n',
                "crates/combined/Cargo.toml": package("combined", '[dependencies]\nhelper = { path = "../helper" }\n'),
                "crates/helper/Cargo.toml": package("helper", '[dependencies]\naxum = "0.8"\n'),
                "crates/helper/src/lib.rs": "",
            },
            ("no-listener-crates", "crates/helper/Cargo.toml"),
        )

    def test_excluded_path_dependency_is_not_a_member(self):
        # spike is excluded, so its own dependencies aren't checked; reaching it still is.
        self.assertFlags(
            {
                "crates/combined/Cargo.toml": package("combined", '[dependencies]\nm0 = { path = "../../spike/m0" }\n'),
                "spike/m0/Cargo.toml": package("m0", '[dependencies]\nportable-pty = "0.8"\n'),
            },
            ("no-production-dependency-on-non-production", "crates/combined/Cargo.toml"),
        )

    def test_legacy_build_dependencies_table(self):
        self.assertFlags(
            {"crates/combined/Cargo.toml": package("combined", '[build_dependencies]\naxum = "0.8"\n')},
            ("no-listener-crates", "crates/combined/Cargo.toml"),
        )

    def test_legacy_dev_dependencies_table_is_not_production(self):
        self.assertFlags({"crates/combined/Cargo.toml": package("combined", '[dev_dependencies]\naxum = "0.8"\n')})

    # --- Review 2: Rust test gates and item boundaries

    def test_test_gates_in_every_supported_spelling(self):
        core = """\
        #[cfg(test)]
        fn helper(_: [u8; 1]) {
            let _ = std::fs::read("x");
        }

        #[test]
        fn plain_test() { let _ = std::fs::read("x"); }

        #[tokio::test]
        async fn async_test() { let _ = std::fs::read("x"); }

        #[cfg(/* only in tests */ test)]
        fn commented() { let _ = std::fs::read("x"); }

        #[cfg(any(test, feature = "test-support",))]
        fn trailing_comma() { let _ = std::fs::read("x"); }

        #[cfg(all(unix, test))]
        fn all_with_test() { let _ = std::fs::read("x"); }

        #[cfg(test)]
        const SIZES: [usize; 2] = [1; 2];

        mod inner {
            #![cfg(test)]
            use std::process::Command;
        }
        """
        self.assertFlags({"crates/combined/src/core/gates.rs": core})

    def test_production_cfgs_are_not_test_gates(self):
        core = """\
        #[cfg(not(test))]
        pub fn a() { let _ = std::fs::read("x"); }

        #[cfg(any(test, unix))]
        pub fn b() { let _ = std::fs::read("x"); }

        #[cfg(feature = "metrics")]
        pub fn c() { let _ = std::fs::read("x"); }
        """
        self.assertEqual(
            sorted(v.line for v in self._violations({"crates/combined/src/core/prod.rs": core})),
            [2, 5, 8],
        )

    def test_gated_field_hides_only_that_field(self):
        core = """\
        pub struct S {
            #[cfg(test)]
            probe: std::fs::File,
            pub file: std::fs::File,
        }
        """
        self.assertEqual([v.line for v in self._violations({"crates/combined/src/core/s.rs": core})], [4])

    def test_raw_identifiers_in_paths(self):
        self.assertEqual(
            sorted(v.line for v in self._violations({"crates/combined/src/core.rs": (
                "pub fn a() { let _ = std::r#fs::read(\"x\"); }\n"
                "pub fn b() { let _ = r#std::fs::read(\"x\"); }\n"
                "use r#tokio::r#process;\n")})),
            [1, 2, 3],
        )

    def test_unterminated_rust_is_a_parse_error(self):
        self.assertFlags(
            {"crates/combined/src/core.rs": 'pub const X: &str = "never closed;\n'},
            ("guard-parse", "crates/combined/src/core.rs"),
        )

    # --- Review 2: path sites

    def test_test_only_fixture_includes_are_allowed(self):
        self.assertFlags(
            {
                "crates/combine/src/main.rs": (
                    'fn main() {}\n#[cfg(test)]\nfn helper() { let _ = include_str!("../../combined/tests/data.txt"); }\n'),
                "crates/combine/src/fixture.rs": '#![cfg(test)]\nconst SAMPLE: &str = include_str!("../../combined/tests/data.txt");\n',
            },
        )

    def test_raw_string_path_sites(self):
        self.assertFlags(
            {
                "crates/combined/src/main.rs": 'const X: &str = include_str!(r"../../../spike/data.txt");\nfn main() {}\n',
                "crates/combine/src/main.rs": '#[path = r#"../../../spike/helper.rs"#]\nmod helper;\nfn main() {}\n',
            },
            ("no-production-dependency-on-non-production", "crates/combined/src/main.rs"),
            ("no-production-dependency-on-non-production", "crates/combine/src/main.rs"),
        )

    def test_path_in_doc_string_is_not_a_reach(self):
        src = 'const DOC: &str = "include_str!(\\"../../../spike/x\\")";\n/// #[path = "../../../spike/y.rs"]\nfn main() {}\n'
        self.assertFlags({"crates/combined/src/main.rs": src})

    # --- Review 2: listeners

    def test_socket_builder_and_syscall_crates(self):
        self.assertFlags(
            {
                "crates/combined/src/rpc.rs": (
                    "pub fn f() -> std::io::Result<()> {\n"
                    "    let socket = tokio::net::TcpSocket::new_v4()?;\n"
                    "    socket.bind(\"127.0.0.1:1\".parse().unwrap())?;\n"
                    "    let _l = socket.listen(128)?;\n    Ok(())\n}\n"),
                "crates/combined/src/adapters/fs/raw.rs": "pub fn g(fd: i32) { unsafe { libc::listen(fd, 1); } }\n",
            },
            ("no-listener-no-pty", "crates/combined/src/rpc.rs"),
            ("no-raw-listen-or-pty", "crates/combined/src/adapters/fs/raw.rs"),
        )

    def test_reserved_names_flag_local_types_too(self):
        # Deliberate: the identifier list reserves these names. Rename the local type.
        self.assertFlags(
            {"crates/combined/src/model.rs": "pub struct TcpListener { pub enabled: bool }\n"},
            ("no-listener-no-pty", "crates/combined/src/model.rs"),
        )

    # --- Review 2: skipped directories

    def test_nested_dist_and_target_inside_source_are_read(self):
        self.assertFlags(
            {
                "crates/combined/src/core/dist/mod.rs": "pub fn f() { let _ = std::fs::read(\"x\"); }\n",
                "crates/combined/src/core/target.rs": "use std::process::Command;\n",
            },
            ("core-does-no-io", "crates/combined/src/core/dist/mod.rs"),
            ("core-does-no-io", "crates/combined/src/core/target.rs"),
        )

    def test_skip_glob_inside_source_is_reported(self):
        self.assertFlags(
            {"crates/combined/src/core/node_modules/x.rs": "use std::fs;\n"},
            ("guard-skip", "crates/combined/src/core/node_modules"),
        )

    def test_build_output_roots_are_skipped(self):
        self.assertFlags(
            {
                "target/debug/build/out.rs": "use std::net::TcpListener;\n",
                "app/web/dist/assets/index.js": "import '@tauri-apps/plugin-shell';\n",
            },
        )

    # Slice worktrees and sandbox clones are whole copies of the tree, including the one place a JS
    # engine may live; reading them would flag that file again under a foreign path. Each root has
    # its own witness, so removing either skip fails its own test.

    def test_factory_run_state_is_skipped(self):
        self.assertFlags({".factory/worktrees/46/a1/crates/combined/src/core/x.rs": "use rquickjs::Runtime;\n"})

    def test_factory_sandboxes_are_skipped(self):
        self.assertFlags({".factory-sandboxes/46/crates/combined/src/core/y.rs": "use rquickjs::Runtime;\n"})

    # A skip must not turn a reachable violation into a clean check: production code that compiles
    # or imports a file under a skipped root is reported, for each scratch root and each way in.

    def test_path_module_into_factory_run_state_is_reported(self):
        self.assertFlags(
            {
                "crates/combined/src/main.rs": '#[path = "../../../.factory/payload.rs"]\nmod payload;\nfn main() {}\n',
                ".factory/payload.rs": "pub fn f() { let _ = std::net::TcpListener::bind(\"127.0.0.1:0\"); }\n",
            },
            ("guard-skip", ".factory"),
        )

    def test_path_module_into_factory_sandboxes_is_reported(self):
        self.assertFlags(
            {
                "crates/combined/src/main.rs": '#[path = "../../../.factory-sandboxes/payload.rs"]\nmod payload;\nfn main() {}\n',
                ".factory-sandboxes/payload.rs": "pub fn f() { let _ = std::net::TcpListener::bind(\"127.0.0.1:0\"); }\n",
            },
            ("guard-skip", ".factory-sandboxes"),
        )

    def test_include_into_factory_sandboxes_is_reported(self):
        self.assertFlags(
            {
                "crates/combined/src/main.rs": 'include!("../../../.factory-sandboxes/46/payload.rs");\nfn main() {}\n',
                ".factory-sandboxes/46/payload.rs": "use rquickjs::Runtime;\n",
            },
            ("guard-skip", ".factory-sandboxes"),
        )

    def test_brace_and_bracket_include_into_either_scratch_root_is_reported(self):
        for root in (".factory", ".factory-sandboxes"):
            for call in ('include! {{ "../../../{r}/payload.rs" }}', 'include!["../../../{r}/payload.rs"];'):
                with self.subTest(root=root, call=call):
                    self.assertFlags(
                        {
                            "crates/combined/src/main.rs": call.format(r=root) + "\nfn main() {}\n",
                            f"{root}/payload.rs": "use rquickjs::Runtime;\n",
                        },
                        ("guard-skip", root),
                    )

    def test_brace_and_bracket_include_reach_banned_paths(self):
        # The same delimiters reach a `path_ban` target outside any skip.
        for call in ('include! { "../../../spike/x.rs" }', 'include!["../../../spike/x.rs"];'):
            with self.subTest(call=call):
                self.assertFlags(
                    {"crates/combined/src/main.rs": call + "\nfn main() {}\n", "spike/x.rs": "\n"},
                    ("no-production-dependency-on-non-production", "crates/combined/src/main.rs"),
                )

    def test_frontend_import_into_factory_run_state_is_reported(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": 'import "../../../.factory/46/shell.ts";\n',
                ".factory/46/shell.ts": 'import "@tauri-apps/plugin-shell";\n',
            },
            ("guard-skip", ".factory"),
        )

    def test_production_symlink_into_factory_run_state_is_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, text in {**BASE, ".factory/46/payload.rs": "use rquickjs::Runtime;\n"}.items():
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text(text)
            (root / "crates/combined/src/payload.rs").symlink_to("../../../.factory/46/payload.rs")
            got = {(v.rule, v.path) for v in check.run(root, CONFIG)}
        # The link is read under its own path as well, so its contents are flagged there too.
        self.assertEqual(got, {("guard-skip", ".factory"), ("js-engine-only-in-loader", "crates/combined/src/payload.rs")})

    # --- Review 2: frontend lexing

    def test_imports_inside_strings_templates_and_regexes_are_not_imports(self):
        src = """\
        export const documentation = "import '@tauri-apps/api/core'";
        export const tpl = `require("@tauri-apps/plugin-fs")`;
        export const re = /import "@tauri-apps\\/api"/;
        """
        self.assertFlags({"app/web/src/main.ts": src})

    def test_import_after_regex_literal(self):
        self.assertFlags(
            {"app/web/src/main.ts": 'const slash = /\\//; import { invoke } from "@tauri-apps/api/core";\n'},
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
        )

    def test_escaped_specifiers_are_decoded(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": 'import { invoke } from "\\u0040tauri-apps/api/core";\n',
                "app/web/src/ipc/shell.ts": 'export * from "@tauri-apps/plugin\\u002dshell";\n',
                "app/web/src/panels/host.ts": "const m = require('\\x40tauri-apps/api');\n",
            },
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
            ("no-webview-shell-or-fs-plugin", "app/web/src/ipc/shell.ts"),
            ("tauri-only-in-ipc", "app/web/src/panels/host.ts"),
        )

    def test_import_inside_template_substitution(self):
        self.assertFlags(
            {"app/web/src/main.ts": 'export const s = `${(await import("@tauri-apps/api/core")).invoke}`;\n'},
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
        )

    def test_computed_specifiers_are_reported(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": (
                    "const name = '@tauri-apps/api/core';\n"
                    "export const a = import(name);\n"
                    "export const b = import(`@tauri-apps/${'api'}`);\n"
                    "export const c = import(`./model/types`);\n"),
            },
            ("frontend-imports-are-literal", "app/web/src/main.ts"),
        )
        self.assertEqual(
            sorted(v.line for v in self._violations({"app/web/src/main.ts": "export const a = import(x);\nexport const b = import(`${y}`);\n"})),
            [1, 2],
        )

    def test_not_imports(self):
        src = """\
        export const meta = import.meta.url;
        export const obj = { import: 1, require: 2 };
        export const v = obj.import + obj.require;
        export { meta as m };
        export const x = "from '@tauri-apps/api'";
        """
        self.assertFlags({"app/web/src/main.ts": src})

    def test_directory_root_imports(self):
        self.assertFlags(
            {
                "app/web/test/index.ts": "export const helper = 1;\n",
                "app/web/src/main.ts": 'import { helper } from "../test";\n',
                "app/web/src/shell/boot.ts": 'import "../../../../spike";\n',
            },
            ("no-production-dependency-on-non-production", "app/web/src/main.ts"),
            ("no-production-dependency-on-non-production", "app/web/src/shell/boot.ts"),
        )

    def test_unterminated_frontend_is_a_parse_error(self):
        self.assertFlags(
            {"app/web/src/main.ts": "export const s = 'never closed;\nimport '@tauri-apps/api';\n"},
            ("guard-parse", "app/web/src/main.ts"),
        )

    # --- Review 3 (opencode, 2026-11-20): test attributes and item boundaries

    def test_only_known_test_attributes_gate(self):
        core = """\
        #[helper::test]
        pub fn production() { let _ = std::fs::read("x"); }

        #[tokio::test(flavor = "multi_thread")]
        async fn real_test() { let _ = std::fs::read("x"); }
        """
        self.assertEqual([v.line for v in self._violations({"crates/combined/src/core.rs": core})], [2])

    def test_generic_and_const_generic_boundaries(self):
        core = """\
        pub struct Model {
            #[cfg(test)]
            value: Result<(), std::fs::File>,
            pub size: u8,
        }

        pub struct Marker<const N: usize>;

        #[cfg(test)]
        fn helper() -> Marker<{ 1 }> {
            let _ = std::fs::read("x");
            Marker
        }

        #[cfg(test)]
        struct Defaulted<const N: usize = { 1 }> { file: std::fs::File }

        #[cfg(test)]
        const LIMIT: bool = 1 < 2;
        pub fn after() { let _ = std::process::id(); }
        """
        self.assertEqual([v.line for v in self._violations({"crates/combined/src/core.rs": core})], [20])

    def test_extern_crate_alias_is_scoped(self):
        self.assertFlags(
            {
                "crates/combined/Cargo.toml": package("combined", '[dependencies]\njs = { package = "rquickjs", version = "0.6" }\n'),
                "crates/combined/src/main.rs": "mod nested {\n    extern crate core as js;\n}\nfn main() { let _ = js::Runtime::new(); }\n",
            },
            ("js-engine-only-in-loader", "crates/combined/src/main.rs"),
        )

    def test_library_target_name_of_path_dependency(self):
        self.assertFlags(
            {
                "crates/combined/Cargo.toml": package("combined", '[dependencies]\nrquickjs = { path = "../rquickjs" }\n'),
                "crates/rquickjs/Cargo.toml": package("rquickjs", '[lib]\nname = "engine"\n'),
                "crates/rquickjs/src/lib.rs": "pub fn ping() {}\n",
                "crates/combined/src/main.rs": "fn main() { engine::ping(); }\n",
            },
            ("js-engine-only-in-loader", "crates/combined/src/main.rs"),
        )

    def test_member_package_key_with_workspace_inheritance_uses_workspace_identity(self):
        # Cargo accepts this and ignores the member's `package`; so does the guard.
        self.assertFlags(
            {
                "Cargo.toml": WORKSPACE + '\n[workspace.dependencies]\njs = { package = "rquickjs", version = "0.6" }\n',
                "crates/combined/Cargo.toml": package("combined", '[dependencies]\njs = { workspace = true, package = "plain" }\n'),
                "crates/combined/src/main.rs": "fn main() { let _ = js::Runtime::new(); }\n",
            },
            ("js-engine-only-in-loader", "crates/combined/src/main.rs"),
        )

    # --- Review 3: symlinks and skips

    def test_symlinked_path_dependency_into_spike(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            files = {**BASE,
                     "crates/combined/Cargo.toml": package("combined", '[dependencies]\nm0 = { path = "../../safe" }\n'),
                     "spike/m0/Cargo.toml": package("m0"),
                     "spike/m0/src/lib.rs": "pub fn ping() {}\n"}
            for rel, text in files.items():
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text(textwrap.dedent(text))
            (root / "safe").symlink_to("spike/m0", target_is_directory=True)
            (root / "crates/combined/src/helper.rs").symlink_to("../../../spike/m0/src/lib.rs")
            got = {(v.rule, v.path) for v in check.run(root, CONFIG)}
        self.assertEqual(got, {
            ("no-production-dependency-on-non-production", "crates/combined/Cargo.toml"),
            ("no-production-dependency-on-non-production", "safe"),
            ("no-production-dependency-on-non-production", "crates/combined/src/helper.rs"),
        })

    def test_skip_of_an_ancestor_of_production_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, text in {**BASE, "crates/combined/src/core.rs": "use std::fs;\n"}.items():
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text(text)
            config = root / "guard.toml"
            config.write_text(CONFIG.read_text().replace('skip = [', 'skip = [\n  "crates/**",\n  "app/**",', 1))
            got = {(v.rule, v.path) for v in check.run(root, config)}
        self.assertEqual(got, {("guard-skip", "crates"), ("guard-skip", "app")})

    # --- Review 3: frontend grammar and lexical context

    def test_string_named_bindings(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": 'import { "invoke" as invoke } from "@tauri-apps/api/core";\n',
                "app/web/src/shell/a.ts": 'export { "invoke" as "call" } from "@tauri-apps/api/core";\n',
                "app/web/src/panels/b.ts": 'export * as "api" from "@tauri-apps/api/core";\n',
            },
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
            ("tauri-only-in-ipc", "app/web/src/shell/a.ts"),
            ("tauri-only-in-ipc", "app/web/src/panels/b.ts"),
        )

    def test_import_forms_still_read(self):
        src = """\
        import def, * as ns from "@tauri-apps/api/a";
        import type { T } from "@tauri-apps/api/b";
        import type from "@tauri-apps/api/c";
        import def2, { x as y } from "@tauri-apps/api/d";
        export type { U } from "@tauri-apps/api/e";
        """
        self.assertEqual(sorted(v.line for v in self._violations({"app/web/src/main.ts": src})), [1, 2, 3, 4, 5])

    # --- JSX tags

    def test_jsx_closing_tag_on_one_line(self):
        self.assertFlags({"app/web/src/shell/A.tsx": "export const a = <p>hi</p>;\n"})

    def test_jsx_closing_tag_on_its_own_line(self):
        src = """\
        export const a = (
          <section>
            <p>hi</p>
          </section>
        );
        """
        self.assertFlags({"app/web/src/shell/A.tsx": src})

    def test_jsx_nested_elements_and_fragments(self):
        src = """\
        export const a = (
          <>
            <ul>
              <li>one</li>
              <li>{1 + 2}</li>
            </ul>
          </>
        );
        """
        self.assertFlags({"app/web/src/shell/A.tsx": src})

    def test_jsx_self_closing_tags(self):
        src = """\
        declare const x: object;
        export const a = (
          <div>
            <Foo />
            <Foo a="b" />
            <Foo a={1} />
            <Foo {...x} />
            <br />
          </div>
        );
        """
        self.assertFlags({"app/web/src/shell/A.tsx": src})

    def test_jsx_rule_does_not_apply_to_ts_files(self):
        src = (
            "declare const a: any;\n"
            "export const r = a </[//]/; import { invoke } from '@tauri-apps/api/core';\n"
        )
        self.assertFlags(
            {"app/web/src/shell/probe.ts": src}, ("tauri-only-in-ipc", "app/web/src/shell/probe.ts")
        )

    def test_template_in_regex_after_less_than_does_not_hide_imports_in_ts(self):
        src = (
            "declare const a: any, x: any;\n"
            "export const r = a </`/;\n"
            "import { invoke } from '@tauri-apps/api/core';\n"
            "export const t = /`/;\n"
            "if (x) { } />`/;\n"
        )
        self.assertFlags(
            {"app/web/src/shell/probe.ts": src}, ("tauri-only-in-ipc", "app/web/src/shell/probe.ts")
        )

    def test_tsx_closing_tag_needs_a_tag_name(self):
        src = (
            "declare const a: any;\n"
            "export const r = a </[//]/; import { invoke } from '@tauri-apps/api/core';\n"
        )
        self.assertFlags(
            {"app/web/src/shell/probe.tsx": src}, ("tauri-only-in-ipc", "app/web/src/shell/probe.tsx")
        )

    def test_tsx_regex_starting_with_gt_after_block_is_a_regex(self):
        src = "declare const x: any;\nif (x) { } />`/;\nimport { invoke } from '@tauri-apps/api/core';\n/`/;\n"
        self.assertFlags(
            {"app/web/src/shell/probe.tsx": src}, ("tauri-only-in-ipc", "app/web/src/shell/probe.tsx")
        )

    # --- JSX children are text, never code (opencode review 11)

    def _probe_tsx(self, middle, *expected, head="export const a = <div><span>x</span>\"</div>;"):
        src = f"{head} {middle} export const q = /\"/;\n"
        self.assertFlags({"app/web/src/shell/Probe.tsx": src}, *[(r, "app/web/src/shell/Probe.tsx") for r in expected])

    def test_quote_in_jsx_text_does_not_hide_a_tauri_import(self):
        self._probe_tsx("import { invoke } from '@tauri-apps/api/core';", "tauri-only-in-ipc")

    def test_quote_in_jsx_text_does_not_hide_a_test_path_import(self):
        self._probe_tsx("import '../../test/support/snapshots';", "no-production-dependency-on-non-production")

    def test_quote_in_jsx_text_does_not_hide_a_computed_import(self):
        self._probe_tsx("export const load = import('../ipc/' + 'tauri');", "frontend-imports-are-literal")

    def test_quote_in_text_after_an_opening_tag(self):
        self._probe_tsx(
            "import { invoke } from '@tauri-apps/api/core';",
            "tauri-only-in-ipc",
            head="export const a = <div>say \"hi</div>;",
        )

    def test_backtick_in_jsx_text(self):
        self._probe_tsx(
            "import { invoke } from '@tauri-apps/api/core';",
            "tauri-only-in-ipc",
            head="export const a = <p>use `x</p>;",
        )

    def test_slash_in_jsx_text(self):
        self._probe_tsx(
            "import { invoke } from '@tauri-apps/api/core';",
            "tauri-only-in-ipc",
            head="export const a = <p>and/or // not a comment</p>;",
        )

    def test_apostrophe_in_jsx_text_then_import_on_the_next_line(self):
        src = "export const a = <p>don't</p>;\nimport { invoke } from '@tauri-apps/api/core';\n"
        self.assertFlags(
            {"app/web/src/shell/Probe.tsx": src}, ("tauri-only-in-ipc", "app/web/src/shell/Probe.tsx")
        )

    def test_nested_jsx_inside_an_expression_child(self):
        src = (
            "declare const cond: boolean;\n"
            "export const a = <p>{cond && <b>it's</b>}</p>;\n"
            "import { invoke } from '@tauri-apps/api/core';\n"
        )
        self.assertFlags(
            {"app/web/src/shell/Probe.tsx": src}, ("tauri-only-in-ipc", "app/web/src/shell/Probe.tsx")
        )

    def test_import_inside_a_jsx_expression_is_seen(self):
        src = "export const a = <p>{import('@tauri-apps/api/core')}</p>;\n"
        self.assertFlags(
            {"app/web/src/shell/Probe.tsx": src}, ("tauri-only-in-ipc", "app/web/src/shell/Probe.tsx")
        )

    def test_unbalanced_jsx_fails_closed(self):
        for src in (
            "export const a = <div><span>x</div>;\n",
            "export const a = <div>text\n",
            "export const a = <div x=\"y>text</div>;\n",
            "export const a = <div></span>;\n",
        ):
            with self.subTest(src=src):
                self.assertFlags(
                    {"app/web/src/shell/Probe.tsx": src}, ("guard-parse", "app/web/src/shell/Probe.tsx")
                )

    # --- JSX expression boundaries and the .tsx backstop (opencode review 13)

    PROBE = "app/web/src/shell/Probe.tsx"
    ADJACENT = (
        ("import('@tauri-apps/api/core')", "tauri-only-in-ipc"),
        ("import('../../test/support/snapshots')", "no-production-dependency-on-non-production"),
        ("import('../ipc/' + 'tauri')", "frontend-imports-are-literal"),
    )

    def _tsx(self, src, rule):
        self.assertFlags({self.PROBE: src}, (rule, self.PROBE))

    def test_adjacent_jsx_expressions_do_not_hide_a_load(self):
        for first, rule in self.ADJACENT:
            with self.subTest(first=first):
                self._tsx(f"export const a = <p>{{{first}}}{{1}}</p>; export const q = /x/;\n", rule)

    def test_adjacent_jsx_attributes_do_not_hide_a_load(self):
        for first, rule in self.ADJACENT:
            with self.subTest(first=first):
                self._tsx(f"export const a = <p a={{{first}}} b={{1}} />; export const q = /x/;\n", rule)

    def test_nested_adjacent_expressions_do_not_hide_a_load(self):
        for first, rule in self.ADJACENT:
            with self.subTest(first=first):
                src = f"declare const cond: boolean, x: number;\nexport const a = <p>{{cond && <b>{{{first}}}{{x}}</b>}}{{x}}</p>;\n"
                self._tsx(src, rule)

    def test_template_specifiers_do_not_hide_a_load(self):
        for first, rule in (
            ("import(`@tauri-apps/api/core`)", "tauri-only-in-ipc"),
            ("import(`../ipc/${'tauri'}`)", "frontend-imports-are-literal"),
        ):
            with self.subTest(first=first):
                self._tsx(f"export const a = <p>{{{first}}}{{1}}</p>;\n", rule)
            with self.subTest(first=first, nested="template"):
                self._tsx(f"export const a = <p>{{`${{{first}}}`}}{{1}}</p>;\n", rule)

    def test_backstop_import_spelled_in_jsx_text_fails_closed(self):
        src = "export const a = <p>import('@tauri-apps/api/core')</p>;\n"
        self.assertFlags({self.PROBE: src}, ("guard-parse", self.PROBE))

    def test_backstop_import_spelled_in_a_comment_fails_closed(self):
        for src in (
            "// import x from 'y'\nexport const a = 1;\n",
            "export const a = \"import('x')\";\n",
            "/* export { a } from './a' */\nexport const a = 1;\n",
        ):
            with self.subTest(src=src):
                self.assertFlags({self.PROBE: src}, ("guard-parse", self.PROBE))

    def test_backstop_holds_when_the_lexer_drops_jsx_braces(self):
        real = check.lex_js

        def broken(src, jsx=False):
            toks = real(src, jsx=jsx)
            return [t for t in toks if not t.is_("punct", "}")] if jsx else toks

        for first, rule in self.ADJACENT:
            with self.subTest(first=first), mock.patch.object(check, "lex_js", broken):
                src = f"export const a = <p>{{{first}}}{{1}}</p>; export const q = /x/;\n"
                self.assertFlags({self.PROBE: src}, ("guard-parse", self.PROBE))

    # --- the keyword matrix (opencode review 15)

    @staticmethod
    def _matrix():
        placements = {
            "child": "export const a = <p>{{{b}}}{{1}}</p>;\n",
            "attr": "export const a = <p a={{{b}}} b={{1}} />;\n",
            "plain": "export const a = [{b}, 1];\n",
        }
        specs = (
            ("'@tauri-apps/api/core'", "tauri-only-in-ipc"),
            ("'../../test/support/snapshots'", "no-production-dependency-on-non-production"),
            ("'../ipc/' + 'tauri'", "frontend-imports-are-literal"),
        )
        seps = ("", " ", "/**/", "\n", "// c\n")
        for place, shape in placements.items():
            for form in ("bare", "template", "adjacent"):
                for kw in ("import", "require"):
                    for sep in seps:
                        for spec, rule in specs:
                            call = f"{kw}{sep}({spec})"
                            body = {"bare": call, "template": f"`${{{call}}}`", "adjacent": f"`${{{call}}}${{{{}}}}`"}[form]
                            yield f"{place}/{form}/{kw}/{sep!r}/{rule}", shape.format(b=body), rule

    def _check_matrix(self):
        cases = list(self._matrix())
        for name, src, rule in cases:
            with self.subTest(case=name):
                found = self.guard({self.PROBE: src})
                self.assertTrue(found, "clean")
                self.assertTrue(found <= {(rule, self.PROBE), ("guard-parse", self.PROBE)}, found)
        return len(cases)

    def test_keyword_matrix_is_never_clean(self):
        self.assertEqual(self._check_matrix(), 270)

    def test_keyword_matrix_is_never_clean_with_a_broken_lexer(self):
        real = check.lex_js

        def broken(src, jsx=False):
            toks = real(src, jsx=jsx)
            return [t for t in toks if not (t.is_("punct", "}") or t.kind.startswith("template_"))] if jsx else toks

        with mock.patch.object(check, "lex_js", broken):
            self._check_matrix()

    # --- spread operands and expression forms (opencode review 16)

    SPECS = (
        ("'@tauri-apps/api/core'", "tauri-only-in-ipc"),
        ("'../../test/support/snapshots'", "no-production-dependency-on-non-production"),
        ("'../ipc/'+'tauri'", "frontend-imports-are-literal"),
    )
    SEPS = ("", " ", "/**/", "\n", "//c\n", "/*(){}'`*/")
    SPREADS = (
        ("object", "export const a = ({{...{c}}});", "ts"),
        ("array", "export const a = [...{c}];", "ts"),
        ("call", "declare function f(...a: unknown[]): void;\nf(...{c});", "ts"),
        ("jsx-attr", "export const a = <p {{...{c}}} />;", "tsx"),
        ("jsx-child", "export const a = <p>{{({{...{c}}})}}</p>;", "tsx"),
    )
    FORMS = (
        "CALL", "[CALL]", "({p:CALL})", "({...CALL})", "[...CALL]",
        "`${CALL}${{}}`", "`${{}}${CALL}`", "`${function f(){}}${CALL}`",
        "`${/x/}${CALL}`", "`${()=>{}}${CALL}`", "`${CALL}${()=>{}}`",
        "`${CALL}${({})}`", "`${CALL}${{k:1}}`", "`${CALL}${{k:1}}${CALL}`",
        "`${CALL}${{}}${/x/}`", "String.raw`${CALL}${{}}`",
        "((() => CALL)())", "({m(){return CALL}}).m()",
        "CALL ? /x/ : /y/", "[...await CALL]",
        "`${`${CALL}${{}}`}${{}}`", "`${{v:`${CALL}${{}}`}}${{}}`",
        "`${CALL}${`inner ${1}`}`", "`${1}${`${CALL}${{}}`}`",
    )
    WRAPS = (
        ("export const a = BODY;", "ts"),
        ("export async function f() {return BODY;}", "ts"),
        ("export const a = [BODY, 1];", "ts"),
        ("export const a = <p>{BODY}{1}</p>; export const q = /x/;", "tsx"),
        ("export const a = <P a={BODY} b={1} />;", "tsx"),
    )

    def _expression_cases(self):
        for kw in ("import", "require"):
            for sep in self.SEPS:
                for spec, rule in self.SPECS:
                    call = f"{kw}{sep}({spec})"
                    for name, shape, ext in self.SPREADS:
                        yield f"spread/{name}/{kw}/{sep!r}/{rule}", shape.format(c=call), ext, rule
                    for form in self.FORMS:
                        for wrap, ext in self.WRAPS:
                            src = wrap.replace("BODY", form.replace("CALL", call))
                            yield f"form/{form}/{wrap}/{kw}/{sep!r}/{rule}", src, ext, rule

    def _check_expression_cases(self, tsx_only=False):
        cfg = check.load_config(CONFIG)
        count = 0
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, text in BASE.items():
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text(text)
            for name, src, ext, rule in self._expression_cases():
                if tsx_only and ext != "tsx":
                    continue
                count += 1
                rel = f"app/web/src/shell/Probe.{ext}"
                found = {v.rule for v in check.check_js_file(rel, src, cfg, root)}
                with self.subTest(case=name, ext=ext):
                    self.assertTrue(found, f"clean: {src!r}")
                    self.assertTrue(found <= {rule, "guard-parse"}, (found, src))
        return count

    def test_spread_and_expression_forms_are_never_clean(self):
        self.assertEqual(self._check_expression_cases(), 2 * 6 * 3 * (5 + 24 * 5))

    def test_spread_and_expression_forms_with_the_reader_blind_to_spread(self):
        real = check.lex_js

        def blind(src, jsx=False):
            out = []
            for t in real(src, jsx=jsx):
                if t.is_("punct", "..."):
                    out += [check.Tok("punct", ".", t.start + k) for k in range(3)]
                else:
                    out.append(t)
            return out

        with mock.patch.object(check, "lex_js", blind):
            self._check_expression_cases(tsx_only=True)

    # --- escaped spellings and generic calls (opencode review 17)

    ESCAPED = {
        "require": (
            "require", r"\u0072equire", r"req\u0075ire", r"requir\u0065",
            r"\u0072eq\u0075ir\u0065", r"r\u{65}quire", r"\u{72}\u{65}\u{71}uire",
        ),
        "import": (
            "import", r"\u0069mport", r"im\u0070ort", r"impor\u0074",
            r"\u0069mp\u006frt", r"\u{69}mport", r"\u{69}\u{6d}\u{70}ort",
        ),
    }
    PLACEMENTS = (
        ("child", "export const a = <p>{{{c}}}{{1}}</p>;", "tsx"),
        ("attr", "export const a = <p a={{{c}}} b={{1}} />;", "tsx"),
        ("jsx-spread", "export const a = <p>{{({{...{c}}})}}</p>;", "tsx"),
        ("plain", "export const a = [{c}, 1];", "tsx"),
        ("plain", "export const a = [{c}, 1];", "ts"),
        ("spread", "export const a = ({{...{c}}});", "ts"),
    )

    def _escape_cases(self):
        for kw, spellings in self.ESCAPED.items():
            for spelling in spellings:
                for suffix, tag in (("", "call"), ("<any>", "generic"), ("<Array<string>>", "nested-generic")):
                    for spec, rule in self.SPECS:
                        call = f"{spelling}{suffix}({spec})"
                        for place, shape, ext in self.PLACEMENTS:
                            yield f"{kw}/{spelling}/{tag}/{place}/{rule}", shape.format(c=call), ext, rule

    def _check_escape_cases(self, tsx_only=False):
        cfg = check.load_config(CONFIG)
        count = 0
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, text in BASE.items():
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text(text)
            for name, src, ext, rule in self._escape_cases():
                if tsx_only and ext != "tsx":
                    continue
                count += 1
                found = {v.rule for v in check.check_js_file(f"app/web/src/shell/Probe.{ext}", src, cfg, root)}
                with self.subTest(case=name, ext=ext):
                    self.assertTrue(found, f"clean: {src!r}")
                    self.assertTrue(found <= {rule, "guard-parse"}, (found, src))
        return count

    def test_escaped_and_generic_calls_are_never_clean(self):
        self.assertEqual(self._check_escape_cases(), 2 * 7 * 3 * 3 * 6)

    def test_generic_call_reports_the_load_itself(self):
        for src, rule in (
            ("export const a = <p>{require<any>('@tauri-apps/api/core')}</p>;\n", "tauri-only-in-ipc"),
            ("export const a = <p>{require('@tauri-apps/api/core')}</p>;\n", "tauri-only-in-ipc"),
            ("export const a = <p>{r\\u0065quire<any>('../../test/support/snapshots')}</p>;\n",
             "no-production-dependency-on-non-production"),
        ):
            with self.subTest(src=src):
                self._tsx(src, rule)

    def test_fully_typed_generic_require_is_flagged(self):
        src = """\
        declare function require<T>(id: string): T;
        declare namespace JSX { interface IntrinsicElements { p: object } interface Element {} }
        export const a = <p>{require<any>('@tauri-apps/api/core')}</p>;
        """
        found = self.guard({self.PROBE: src})
        self.assertTrue(found and found <= {("tauri-only-in-ipc", self.PROBE), ("guard-parse", self.PROBE)}, found)

    def test_unbalanced_type_arguments_fail_closed(self):
        for ext in ("ts", "tsx"):
            with self.subTest(ext=ext):
                src = "declare const x: number;\nexport const a = require < x;\n"
                self.assertFlags({f"app/web/src/shell/Probe.{ext}": src}, ("guard-parse", f"app/web/src/shell/Probe.{ext}"))

    def test_ts_keeps_its_non_load_forms(self):
        src = """\
        declare const o: any;
        export const a = o.import, b = o?.require, c = import.meta, d = { import: 1, require: 2 };
        export function require2() {}
        declare function require(id: string): void;
        import fs = require('node:fs');
        export const m = { import(x: string) { return x; } };
        """
        self.assertFlags({"app/web/src/shell/probe.ts": src})

    def test_escaped_and_generic_calls_with_a_blind_reader(self):
        real = check.lex_js

        def blind(src, jsx=False):
            out = []
            for t in real(src, jsx=jsx):
                if t.kind == "ident":
                    raw = check._J_IDENT.match(src, t.start)
                    t = check.Tok("ident", raw.group(0) if raw else t.text, t.start)
                out.append(t)
            return out

        with mock.patch.object(check, "lex_js", blind), mock.patch.object(
            check, "_type_args_end", lambda toks, k: len(toks)
        ):
            self._check_escape_cases(tsx_only=True)

    def test_member_access_named_import_fails_closed_in_tsx(self):
        src = "declare const o: any;\nexport const a = o.import('x');\n"
        self.assertFlags({self.PROBE: src}, ("guard-parse", self.PROBE))

    def test_backstop_keyword_without_a_following_paren(self):
        for src in (
            "export const a = <p>{import/**/('@tauri-apps/api/core')}{{}}</p>;\n",
            "export const o = { import: 1 };\n",
            "export const m = import.meta;\n",
            "export const a = <p>message from \"x\"</p>;\n",
        ):
            with self.subTest(src=src):
                found = self.guard({self.PROBE: src})
                self.assertTrue(found)

    # --- issue #20: one-line closing and self-closing tags, and JSX text before an import

    ONE_LINERS = (
        "export const a = <div><Foo {...p} /></div>;",
        "export const a = <div><Foo/></div>;",
        "export const a = <><Foo /></>;",
        "export const a = <div>{x}<br/>{y}</div>;",
        "export const a = <a href=\"/x/y\">a/b</a>;",
        "export const a = <div>{x}/{y}</div>;",
        "export const a = <Foo a={x / 2} b={/re/.source} />;",
        "export const a = x ? <Foo /> : <Bar />;",
        "export const a = [<Foo key=\"1\" />, <Bar key=\"2\" />];",
        "export const a = <p>x</p>; export const b = <i>y</i>;",
    )
    ONE_LINER_HEAD = "declare const p: object, x: number, y: number;\nfunction Foo() { return null; }\nfunction Bar() { return null; }\n"

    def test_one_line_closing_and_self_closing_tags_are_clean(self):
        for line in self.ONE_LINERS:
            with self.subTest(line=line):
                self.assertFlags({self.PROBE: f"{self.ONE_LINER_HEAD}{line}\n"})

    def test_one_line_self_closing_tags_do_not_hide_an_import(self):
        for line in self.ONE_LINERS:
            for load, rule in (
                ("import { invoke } from '@tauri-apps/api/core';", "tauri-only-in-ipc"),
                ("export const l = import('@tauri-apps/api/core');", "tauri-only-in-ipc"),
                ("import '../../test/support/snapshots';", "no-production-dependency-on-non-production"),
                ("export const l = import('../ipc/' + 'tauri');", "frontend-imports-are-literal"),
            ):
                with self.subTest(line=line, load=load):
                    self._tsx(f"{self.ONE_LINER_HEAD}{line} {load} // '\n", rule)

    def test_self_closing_tag_syntax_in_attributes_and_text_does_not_hide_an_import(self):
        for line in (
            "export const a = <Foo a=\"/>\" />;",
            "export const a = <Foo a='</div>' />;",
            "export const a = <div><Foo /> it's </div>;",
            "export const a = <div><Foo /> a/b </div>;",
        ):
            with self.subTest(line=line):
                self._tsx(
                    f"{self.ONE_LINER_HEAD}{line} import {{ invoke }} from '@tauri-apps/api/core';\n",
                    "tauri-only-in-ipc",
                )

    def test_import_spelled_in_text_after_a_self_closing_tag_fails_closed(self):
        self._tsx(f"{self.ONE_LINER_HEAD}export const a = <div><Foo /> import x from 'y' </div>;\n", "guard-parse")

    def test_issue_20_quote_or_slash_in_jsx_text_does_not_hide_a_later_import(self):
        for text in ("'", "/", "\"", "`", "//"):
            with self.subTest(text=text):
                self._tsx(
                    f"export const a = <div><p>x</p>{text}</div>; import {{ invoke }} from '@tauri-apps/api/core'; // {text}\n",
                    "tauri-only-in-ipc",
                )

    # --- issue #20 follow-up: a load followed by a block is not a method definition

    TS_PROBE = "app/web/src/shell/probe.ts"

    def test_ts_load_before_a_block_is_read_as_a_load(self):
        for load in ("import('@tauri-apps/api/core')", "require('@tauri-apps/api/core')",
                     "import<any>('@tauri-apps/api/core')"):
            body = f"{load}\n{{}}\n"
            for src in (
                body,
                f"{{\n{body}}}\n",
                f"export function f() {{\n{body}}}\n",
                f"export async function f(): Promise<void> {{\n{body}}}\n",
                f"export function f(): string[] {{\n{body}return []; }}\n",
                f"declare const c: boolean;\nif (c) {{ }} else {{\n{body}}}\n",
                f"do {{\n{body}}} while (false);\n",
                f"declare const c: number;\nswitch (c) {{ case 1: {{\n{body}}} }}\n",
                f"label: {{\n{body}}}\n",
                f"export const f = () => {{\n{body}}};\n",
                f"namespace N {{\n{body}}}\n",
                f"export class C {{ m() {{\n{body}}} }}\n",
                f"export class C {{ static {{\n{body}}} }}\n",
                f"export class C<T extends {{ a: 1 }}> {{ m() {{\n{body}}} }}\n",
                f"export const o = {{ m() {{\n{body}}} }};\n",
                f"export const t = `${{(() => {{ {load} {{}} }})()}}`;\n",
            ):
                with self.subTest(src=src):
                    self.assertFlags({self.TS_PROBE: src}, ("tauri-only-in-ipc", self.TS_PROBE))
        self.assertFlags({self.PROBE: "import('@tauri-apps/api/core')\n{}\n"}, ("tauri-only-in-ipc", self.PROBE))

    def test_ts_load_before_a_colon_is_read_as_a_load(self):
        for src in (
            "declare const c: boolean, x: any;\nexport const y = c ? await import('@tauri-apps/api/core') : x;\n",
            "declare const c: boolean, x: any;\nexport const y = c ? import('@tauri-apps/api/core') : x;\n",
            "export class C { x = 1 ? await import('@tauri-apps/api/core') : 2; }\n",
            "export const o = { a: 1 ? import('@tauri-apps/api/core') : 2 };\n",
        ):
            with self.subTest(src=src):
                self.assertFlags({self.TS_PROBE: src}, ("tauri-only-in-ipc", self.TS_PROBE))

    def test_method_definitions_in_classes_and_object_literals_are_not_loads(self):
        for src in (
            "export const o = { import(v: string) { return v; }, require(v: string) { return v; } };\n",
            "export const o = { async import(v: string) { return v; }, *require(v: string) { yield v; } };\n",
            "export const o = { *import(v: string) { yield v; }, async *require(v: string) { yield v; } };\n",
            "export const o = { get import() { return 1; }, set require(v: number) { void v; } };\n",
            "export const o = { a: 1, import<T>(v: T): T { return v; } };\n",
            "export const o = { a: { import(v: string) { return v; } } };\n",
            "export const o = [{ import(v: string) { return v; } }];\n",
            "f({ import(v: string) { return v; } });\nfunction f(x: object) { return x; }\n",
            "export default { import(v: string) { return v; } };\n",
            "export class C { import(v: string) { return v; } require(v: string): string { return v; } }\n",
            "export class C { *import(v: string) { yield v; } static async *require(v: string) { yield v; } }\n",
            "export class C { x = 1; import(v: string) { return v; } }\n",
            "export class C { m() { return 1; } import(v: string): string { return v; } }\n",
            "export class C extends Object { static async import(v: string) { return v; } }\n",
            "export class C implements I { public require(v: string): string { return v; } }\ninterface I { a?: 1 }\n",
            "export const K = class { require(v: string) { return v; } };\n",
            "export class C { m() {} *require(v: string) { yield v; } }\n",
            "export class C { static {} *import(v: string) { yield v; } }\n",
            "export class C { x = {}; *import(v: string) { yield v; } }\n",
            "export class C { x: { a: 1 }\n  *import(v: string) { yield v; } }\n",
            "export class C<T extends { a: 1 }> { import(v: T) { return v; } }\n",
            "export class C extends Array<{ a: 1 }> { *require(v: string) { yield v; } }\n",
            "export type T = { import(v: string): string, require(v: string): void };\n",
        ):
            with self.subTest(src=src):
                self.assertFlags({self.TS_PROBE: src})

    # --- opencode review 76: an exemption only where the member head is unambiguous

    LOAD_SHAPES = (
        ("import('@tauri-apps/api/core')", "tauri-only-in-ipc"),
        ("require('@tauri-apps/api/core')", "tauri-only-in-ipc"),
        ("require?.('@tauri-apps/api/core')", "tauri-only-in-ipc"),
        ("import('../../test/support/snapshots')", "no-production-dependency-on-non-production"),
        ("import('../ipc/' + 'tauri')", "frontend-imports-are-literal"),
    )

    def test_a_word_then_a_line_break_then_a_block_is_not_an_object(self):
        # `of`, `async`, `type` and `get` can be identifiers, and a line break after
        # `return`, `await x`, `yield` or `throw 1` ends the statement: the `{` that
        # follows is a block, so its first call is a load. The `;` forms are controls.
        for ext in (".ts", ".mts", ".cts"):
            path = f"app/web/src/shell/probe{ext}"
            for load, rule in self.LOAD_SHAPES:
                block = f"{{\n  {load}\n  {{}}\n}}\n"
                for src in (
                    f"const of = 1;\nof\n{block}export {{}};\n",
                    f"const of = 1;\nof;\n{block}export {{}};\n",
                    f"const async = 1;\nasync\n{block}export {{}};\n",
                    f"const type = 1;\ntype\n{block}export {{}};\n",
                    f"const get = 1;\nget\n{block}export {{}};\n",
                    f"export function f() {{ return\n{block}}}\n",
                    f"export function f() {{ return;\n{block}}}\n",
                    f"export async function f(x: number) {{ await\nx\n{block}}}\n",
                    f"export function* g() {{ yield\n{block}}}\n",
                    f"export function f() {{ throw 1\n{block}}}\n",
                ):
                    with self.subTest(path=path, src=src):
                        self.assertFlags({path: src}, (rule, path))

    def test_a_call_in_an_initializer_is_not_a_method(self):
        for load, rule in self.LOAD_SHAPES:
            for src in (
                f"declare const c: boolean;\nexport const o = {{\n  x: c ? 2 * {load} : 0\n}};\n",
                f"declare const c: boolean;\nexport class C {{ x = c ? 2 * {load} : 0; }}\n",
                f"declare const c: boolean;\nexport class C {{ static x = c ? 2 * {load} : 0 }}\n",
                f"declare const c: boolean, a: number;\nexport const o = {{ x: c ? a\n* {load} : 0 }};\n",
                f"declare const c: boolean;\nexport const o = {{ x: c ? f(1, {load}) : 0 }};\nfunction f(...a: unknown[]) {{ return a; }}\n",
                f"declare const c: boolean;\nexport class C {{ x = c ? [1, {load}] : 0 }}\n",
                f"declare const c: boolean, get: number;\nexport class C {{ x = c ? get\n* {load} : 0 }}\n",
                f"declare const c: boolean;\nexport const y = c ? 2 * {load} : 0;\n",
            ):
                with self.subTest(src=src):
                    self.assertFlags({self.TS_PROBE: src}, (rule, self.TS_PROBE))

    def test_an_object_or_class_closing_brace_in_an_initializer_is_not_a_member_boundary(self):
        # opencode review 77: `+{} * require(...)` in a class field; the `}` of the
        # object is not the end of a member, so the `*` is multiplication.
        for load, rule in self.LOAD_SHAPES:
            for src in (
                f"declare const c: boolean;\nexport class C {{\n  x = c ? +{{}} * {load} : 0;\n}}\n",
                f"declare const c: boolean;\nexport class C {{\n  x = c ? <any>{{}} * {load} : 0;\n}}\n",
                f"declare const c: boolean;\nexport class C {{\n  x = c ? (+{{}}) * {load} : 0;\n}}\n",
                f"declare const c: boolean;\nexport const o = {{ x: c ? +{{}} * {load} : 0 }};\n",
                f"declare const c: boolean;\nexport class C {{ m() {{}} x = c ? +{{}} * {load} : 0 }}\n",
                f"export class C {{ x = class {{}}\n  * {load}; }}\n",
                f"export class C {{ x = () => {{}}\n  * {load}; }}\n",
                f"export class C {{ x = {{ a: 1 }}\n  * {load}; }}\n",
                f"export class C {{ static {{}} x = +{{}} * {load}; }}\n",
            ):
                with self.subTest(src=src):
                    self.assertFlags({self.TS_PROBE: src}, (rule, self.TS_PROBE))

    def test_ambiguous_member_positions_are_read_as_loads(self):
        # The documented false positives: real methods or signatures named `import`
        # or `require` whose position the reader can't prove is a member head. Each is
        # reported, here as a non-literal load, never skipped.
        for src in (
            "export function f() { return { require(v: string) { return v; } }; }\n",
            "declare const c: boolean;\nexport const o = c ? 1 : { import(v: string) { return v; } };\n",
            "export class C { x = 1\n  import(v: string) { return v; } }\n",
            "export const K = class<T> { import(v: T) { return v; } };\n",
            "declare function dec(...a: unknown[]): any;\nexport class C { @dec\n  import(v: string) { return v; } }\n",
            "export class C { x = 1\n  m() { return 1; }\n  *import(v: string) { yield v; } }\n",
            "export interface I { import(v: string): string }\n",
            "export type T = { import(v: string): string; require(v: string): void };\n",
        ):
            with self.subTest(src=src):
                self.assertFlags({self.TS_PROBE: src}, ("frontend-imports-are-literal", self.TS_PROBE))

    def test_clean_tsx_lexes_clean(self):
        src = """\
        declare const p: object, a: number, b: number, c: number, s: string;
        export const id = <T,>(x: T) => x;
        export const id2 = <T extends object>(x: T) => x;
        export const d = a < b / c;
        export const t = a < b && b > c;
        export function Foo(props: object) {
          return (
            <section aria-label="x" data-q='y'>
              <p>don't "quote" `tick` and/or // fine</p>
              <Foo {...p} />
              <div><Foo {...p} /></div>
              <>
                {s.replace(/"|'/g, "")}
                {a > 1 ? <b>yes</b> : <i>no</i>}
                {/* a comment */}
              </>
              <Foo render={<br />} cb={() => <hr />} />
            </section>
          );
        }
        """
        self.assertFlags({"app/web/src/shell/Probe.tsx": src})

    def test_regex_literals_still_lex_in_tsx(self):
        src = """\
        declare const s: string;
        export const a = <p>{s.replace(/\\.$/, "")}</p>;
        export const re = /<p>/;
        const b = [/x/, /y/].length;
        import { invoke } from "@tauri-apps/api/core";
        """
        self.assertFlags(
            {"app/web/src/shell/A.tsx": src}, ("tauri-only-in-ipc", "app/web/src/shell/A.tsx")
        )

    def test_tauri_import_in_a_tsx_component_is_flagged(self):
        src = """\
        import { invoke } from "@tauri-apps/api/core";
        export function A() {
          return <p onClick={() => invoke("x")}>go</p>;
        }
        """
        self.assertFlags(
            {"app/web/src/shell/A.tsx": src}, ("tauri-only-in-ipc", "app/web/src/shell/A.tsx")
        )

    def test_division_after_postfix_and_non_null(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": 'let n = 1;\nn++ / 2;\nimport("@tauri-apps\\/api\\/core");\nn / 2;\n',
                "app/web/src/shell/x.ts": 'declare const m: number | undefined;\nm! / 2;\nimport("@tauri-apps\\/api\\/core");\nm! / 2;\n',
            },
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
            ("tauri-only-in-ipc", "app/web/src/shell/x.ts"),
        )

    def test_regex_after_control_head_and_block(self):
        src = """\
        declare const flag: boolean, input: string;
        if (flag) /import("@tauri-apps\\/api\\/core")/.test(input);
        { }
        /require("@tauri-apps\\/plugin-fs")/.test(input);
        export const o = { a: 1 } / 2;
        """
        self.assertFlags({"app/web/src/main.ts": src})

    def test_unicode_line_terminators_end_comments(self):
        self.assertFlags(
            {
                "app/web/src/main.ts": '// comment import { invoke } from "@tauri-apps/api/core";\n',
                "app/web/src/shell/x.ts": '// comment import { invoke } from "@tauri-apps/api/core";\n',
            },
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
            ("tauri-only-in-ipc", "app/web/src/shell/x.ts"),
        )

    def test_method_definitions_are_not_loads(self):
        src = """\
        export const service = {
            import(value: string) { return value; },
            require(value: string): string { return value; },
        };
        export class Service {
            require(value: string) { return value; }
            static import(value: string): string { return value; }
        }
        function require(x: string) { return x; }
        """
        self.assertFlags({"app/web/src/main.ts": src})

    def test_optional_require_call(self):
        self.assertFlags(
            {"app/web/src/main.ts": 'const value = require?.("@tauri-apps/api/core");\n'},
            ("tauri-only-in-ipc", "app/web/src/main.ts"),
        )

    def test_unsupported_import_declaration_is_reported(self):
        self.assertFlags(
            {"app/web/src/main.ts": 'import { a } frm "@tauri-apps/api";\n'},
            ("guard-parse", "app/web/src/main.ts"),
        )

    # --- Config

    def test_unknown_config_key_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "guard.toml"
            bad.write_text('version = 1\n[[rust_path]]\nid = "x"\nforbid = []\nwhere = ["**"]\n')
            with self.assertRaises(check.ConfigError):
                check.run(Path(tmp), bad)

    def _violations(self, files):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, text in {**BASE, **files}.items():
                path = root / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(textwrap.dedent(text))
            return check.run(root, CONFIG)


if __name__ == "__main__":
    unittest.main(verbosity=1)
