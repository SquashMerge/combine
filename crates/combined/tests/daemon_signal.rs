#![cfg(feature = "test-support")]
//! The real `combined serve` process stops cleanly on SIGTERM and SIGINT. Every daemon runs on
//! explicit paths under the test's scratch dir with a cleared environment and the fake backend.

mod daemon_support;

use std::process::Command;

use daemon_support::{refuse_default_herdr_core, DaemonPaths, DaemonProcess, ShortTempDir};

fn serve_exits_zero_on(signal: &str) {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    refuse_default_herdr_core(&paths.socket, &scratch);
    refuse_default_herdr_core(&paths.herdr_core_socket, &scratch);

    let mut daemon = DaemonProcess::spawn(&scratch, &paths, "fake");
    daemon.wait_ready();
    assert!(paths.socket.exists());

    let sent = Command::new("/bin/kill")
        .arg(signal)
        .arg(daemon.pid().to_string())
        .status()
        .unwrap();
    assert!(sent.success(), "kill {signal} failed");

    let status = daemon.wait_exit();
    assert!(
        status.success(),
        "the daemon exited with {status} after {signal}:\n{}",
        daemon.log()
    );
}

#[test]
fn serve_exits_zero_on_sigterm() {
    serve_exits_zero_on("-TERM");
}

#[test]
fn serve_exits_zero_on_sigint() {
    serve_exits_zero_on("-INT");
}
