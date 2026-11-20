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
