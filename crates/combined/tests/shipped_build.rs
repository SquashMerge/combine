#![cfg(not(feature = "test-support"))]
//! The binary users install: built without `test-support`, it has no fake backend.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// `/tmp/cs4s-<pid>`: short enough for a socket path, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let path = PathBuf::from(format!("/tmp/cs4s-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Scratch(path)
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
