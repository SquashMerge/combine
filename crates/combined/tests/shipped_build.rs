#![cfg(not(feature = "test-support"))]
//! The binary users install: built without `test-support`, it has no fake backend.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A new directory `<parent>/<prefix>-<8 random hex digits>`, mode 0700. As with `mkdtemp`, a
/// name that exists (a killed run's leftover: pids are reused) is retried, never reused.
/// A copy of `combined::testing::fresh_dir`: this crate is built without `test-support`.
fn fresh_dir(parent: &Path, prefix: &str) -> PathBuf {
    use std::hash::{BuildHasher, Hasher};
    use std::os::unix::fs::DirBuilderExt;
    for _ in 0..16 {
        let suffix = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish() as u32;
        let path = parent.join(format!("{prefix}-{suffix:08x}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            created => {
                created.unwrap_or_else(|error| panic!("creating {}: {error}", path.display()));
                return path;
            }
        }
    }
    panic!("no free {prefix}-* name under {}", parent.display());
}

/// `/tmp/cs4s-<8 random hex digits>`: short enough for a socket path, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        Scratch(fresh_dir(Path::new("/tmp"), "cs4s"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn shipped_daemon_refuses_the_fake_backend() {
    let scratch = Scratch::new();
    let data_dir = scratch.0.join("data");
    let socket = scratch.0.join("combined.sock");
    let config = scratch.0.join("daemon.json");
    let text = serde_json::json!({
        "version": 1,
        "backend": "fake",
        "herdr_core_socket": scratch.0.join("no-hc.sock"),
        "data_dir": data_dir,
        "socket": socket,
    });
    fs::write(&config, serde_json::to_vec(&text).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_combined"))
        .env_clear()
        .env("HOME", scratch.0.join("home"))
        .args(["serve", "--config"])
        .arg(&config)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("combined: backend \"fake\" exists only in test builds"),
        "{stderr}"
    );
    assert!(
        !data_dir.exists(),
        "the refusal must come before the data dir"
    );
    assert!(!socket.exists());
}
