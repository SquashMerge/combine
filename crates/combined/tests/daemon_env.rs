#![cfg(feature = "test-support")]
//! The spawn helpers clear the environment: a parent with `HC_*` and `XDG_*` set must not
//! reach the daemon. Its own test crate (one process, one test) because it sets variables.

mod common;
mod daemon_support;

use std::fs;

use daemon_support::{DaemonPaths, DaemonProcess, ShortTempDir};

#[test]
fn spawned_daemons_do_not_inherit_hc_or_xdg_variables() {
    let scratch = ShortTempDir::new();
    let poison = scratch.sock("poison-hc.sock");
    let poison_state = scratch.path().join("poison-xdg-state");
    std::env::set_var("HC_SOCKET", &poison);
    std::env::set_var("HC_SESSION", "poison-session");
    std::env::set_var("XDG_STATE_HOME", &poison_state);
    std::env::set_var("XDG_RUNTIME_DIR", &poison_state);

    // No `herdr_core_socket`, so the daemon resolves it from its own environment and names
    // the result when the (absent) socket is unavailable.
    let paths = DaemonPaths::in_scratch(&scratch, "env");
    let dir = scratch.path().join(&paths.name);
    fs::create_dir_all(&dir).unwrap();
    let config = dir.join("daemon.json");
    let text = serde_json::json!({
        "version": 1,
        "backend": "herdr-core",
        "data_dir": paths.data_dir,
        "socket": paths.socket,
    });
    fs::write(&config, serde_json::to_vec(&text).unwrap()).unwrap();

    let mut daemon = DaemonProcess::spawn_with(&scratch, &paths, &["serve", "--config"], &config);
    assert_eq!(daemon.wait_exit().code(), Some(3));
    let log = daemon.log();
    let expected = scratch
        .path()
        .join("home/.local/state/herdr-core/default/default.sock");
    assert!(log.contains(expected.to_str().unwrap()), "{log}");
    for leaked in [
        poison.to_str().unwrap(),
        poison_state.to_str().unwrap(),
        "poison-session",
        "HC_SOCKET",
        "HC_SESSION",
    ] {
        assert!(
            !log.contains(leaked),
            "the daemon inherited {leaked}: {log}"
        );
    }
}
