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
