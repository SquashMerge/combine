#![cfg(feature = "test-support")]
//! The real `combined serve` process stops cleanly on SIGTERM and SIGINT. Every daemon runs on
//! explicit paths under the test's scratch dir with a cleared environment and the fake backend.

mod daemon_support;

use std::fs;
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::time::Duration;

use daemon_support::{
    refuse_default_herdr_core, wait_until, DaemonPaths, DaemonProcess, ShortTempDir,
};

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

#[test]
fn serve_exits_zero_on_sigterm_while_herdr_core_never_answers_hello() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    refuse_default_herdr_core(&paths.herdr_core_socket, &scratch);
    // A stand-in for herdr-core's socket that accepts the connection and never says hello.
    fs::create_dir_all(paths.herdr_core_socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&paths.herdr_core_socket).unwrap();
    listener.set_nonblocking(true).unwrap();

    let mut daemon = DaemonProcess::spawn(&scratch, &paths, "herdr-core");
    let mut accepted = None;
    assert!(
        wait_until(Duration::from_secs(10), || {
            accepted = listener.accept().ok();
            accepted.is_some()
        }),
        "the daemon never connected to herdr-core's socket:\n{}",
        daemon.log()
    );
    daemon.terminate();

    let status = daemon.wait_exit();
    assert_eq!(status.code(), Some(0), "{}", daemon.log());
    let log = daemon.log();
    assert!(
        log.contains("combined: stopped by a signal during start-up"),
        "{log}"
    );
    assert!(!log.contains("cannot start"), "{log}");
    assert!(!paths.socket.exists());
    let mut lock = paths.socket.clone().into_os_string();
    lock.push(".lock");
    assert!(!std::path::Path::new(&lock).exists());
    drop(accepted);
}
