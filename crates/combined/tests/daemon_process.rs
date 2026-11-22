#![cfg(feature = "test-support")]
//! The real `combined serve` process: its socket, its locks, its identity and its exit codes.
//! Every daemon runs on explicit paths under the test's scratch dir with a cleared environment.

mod common;
mod daemon_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::Duration;

use combine_protocol::{ClientMessage, DaemonMessage, RequestId, WatchTerminals};
use daemon_support::{
    combined_command, run_to_exit, DaemonPaths, DaemonProcess, ShortTempDir, WireClient,
};

fn code(status: ExitStatus) -> Option<i32> {
    status.code()
}

fn host_id_of(socket: &Path) -> String {
    let (_client, welcome) = WireClient::open(socket);
    welcome.host_id.0
}

fn start(scratch: &ShortTempDir, paths: &DaemonPaths) -> DaemonProcess {
    let mut daemon = DaemonProcess::spawn(scratch, paths, "fake");
    daemon.wait_ready();
    daemon
}

#[test]
fn daemon_process_socket_is_0600() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    let _daemon = start(&scratch, &paths);

    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&paths.socket), 0o600);
    assert_eq!(mode(&paths.data_dir), 0o700);
    assert_eq!(mode(&paths.data_dir.join("host.json")), 0o600);
}

#[test]
fn second_daemon_on_the_same_data_dir_exits_and_the_first_keeps_serving() {
    let scratch = ShortTempDir::new();
    let first_paths = DaemonPaths::in_scratch(&scratch, "a");
    let mut first = start(&scratch, &first_paths);
    let id = host_id_of(&first_paths.socket);

    // A different socket, so only the data directory's lock can stop it.
    let second_paths = DaemonPaths::in_scratch(&scratch, "b")
        .with_data_dir(&scratch, first_paths.data_dir.clone())
        .with_socket(&scratch, scratch.sock("b.sock"));
    let mut second = DaemonProcess::spawn(&scratch, &second_paths, "fake");
    assert_eq!(code(second.wait_exit()), Some(2));
    assert!(
        second
            .log()
            .contains("combined: another combined is using data dir"),
        "{}",
        second.log()
    );
    assert!(!second_paths.socket.exists());

    assert!(first.is_running());
    assert_eq!(host_id_of(&first_paths.socket), id);
}

#[test]
fn second_daemon_with_another_data_dir_cannot_take_the_socket() {
    let scratch = ShortTempDir::new();
    let first_paths = DaemonPaths::in_scratch(&scratch, "a");
    let mut first = start(&scratch, &first_paths);

    let second_paths =
        DaemonPaths::in_scratch(&scratch, "b").with_socket(&scratch, first_paths.socket.clone());
    let mut second = DaemonProcess::spawn(&scratch, &second_paths, "fake");
    assert_eq!(code(second.wait_exit()), Some(2));
    assert!(
        second
            .log()
            .contains("combined: another combined is serving"),
        "{}",
        second.log()
    );

    assert!(first.is_running());
    host_id_of(&first_paths.socket);
}

#[test]
fn daemon_process_reuses_its_host_id_across_restarts() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    let mut first = start(&scratch, &paths);
    let id = host_id_of(&paths.socket);
    first.kill();
    assert!(paths.socket.exists(), "a killed daemon leaves its socket");

    let _second = start(&scratch, &paths);
    assert_eq!(host_id_of(&paths.socket), id);
    assert!(!paths.data_dir.join("state.json").exists());
}

#[test]
fn daemon_process_refuses_a_corrupt_host_json() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    fs::create_dir_all(&paths.data_dir).unwrap();
    let host_json = paths.data_dir.join("host.json");
    fs::write(&host_json, b"{ not json").unwrap();

    let mut daemon = DaemonProcess::spawn(&scratch, &paths, "fake");
    assert_eq!(code(daemon.wait_exit()), Some(2));
    assert!(daemon.log().contains("host.json"), "{}", daemon.log());
    assert_eq!(fs::read(&host_json).unwrap(), b"{ not json");
    assert!(!paths.socket.exists());
}

#[test]
fn herdr_core_backend_with_unreachable_socket_fails_at_start() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    let mut daemon = DaemonProcess::spawn(&scratch, &paths, "herdr-core");
    assert_eq!(code(daemon.wait_exit()), Some(3));
    let log = daemon.log();
    assert!(
        log.contains(paths.herdr_core_socket.to_str().unwrap()),
        "{log}"
    );
    assert!(log.contains("daemon.json"), "{log}");
    assert!(
        !paths.socket.exists(),
        "a failed start must leave no socket"
    );
}

#[test]
fn herdr_core_backend_without_the_adapter_fails_at_start() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    // A stand-in for herdr-core's socket: it is checked and never connected to.
    fs::create_dir_all(paths.herdr_core_socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&paths.herdr_core_socket).unwrap();

    let mut daemon = DaemonProcess::spawn(&scratch, &paths, "herdr-core");
    assert_eq!(code(daemon.wait_exit()), Some(3));
    let log = daemon.log();
    assert!(log.contains("no herdr-core adapter"), "{log}");
    assert!(log.contains("#23"), "{log}");
    assert!(!paths.socket.exists());
    drop(listener);
}

#[test]
fn serve_with_a_bad_config_exits_2() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "bad");
    let dir = scratch.path().join("bad");
    fs::create_dir_all(&dir).unwrap();
    let config = dir.join("daemon.json");

    for (text, needle) in [
        (r#"{"version": 1, "typo": true}"#, "typo"),
        (r#"{"version": 2}"#, "version 2"),
        (r#"{"version": 1, "socket": "relative.sock"}"#, "absolute"),
        ("not json", "invalid"),
    ] {
        fs::write(&config, text).unwrap();
        let mut daemon =
            DaemonProcess::spawn_with(&scratch, &paths, &["serve", "--config"], &config);
        assert_eq!(code(daemon.wait_exit()), Some(2), "{text}");
        assert!(daemon.log().contains(needle), "{text}: {}", daemon.log());
        assert!(daemon.log().starts_with("combined: "), "{}", daemon.log());
    }

    let missing = dir.join("missing.json");
    let mut daemon = DaemonProcess::spawn_with(&scratch, &paths, &["serve", "--config"], &missing);
    assert_eq!(code(daemon.wait_exit()), Some(2));
    assert!(daemon.log().contains("does not exist"), "{}", daemon.log());
}

#[test]
fn daemon_process_opens_no_network_socket() {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "a");
    let daemon = start(&scratch, &paths);

    let (mut client, _) = WireClient::open(&paths.socket);
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    loop {
        if let DaemonMessage::Terminals(_) = client.recv() {
            break;
        }
    }

    let lsof = ["/usr/sbin/lsof", "lsof"]
        .into_iter()
        .find(|name| Command::new(name).arg("-v").output().is_ok());
    let Some(lsof) = lsof else {
        eprintln!("skipped: lsof is not available");
        return;
    };
    let pid = daemon.pid().to_string();
    let list = |kind: &str| {
        let mut command = Command::new(lsof);
        command.args(["-a", "-p", &pid, kind]);
        let output = run_to_exit(command, Duration::from_secs(20));
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    assert_eq!(list("-i"), "", "the daemon has a network socket open");
    let unix = list("-U");
    assert!(unix.contains("combined.sock"), "{unix}");
}

#[test]
fn version_names_the_protocol_and_unknown_commands_exit_2() {
    let scratch = ShortTempDir::new();
    let mut command = combined_command(&scratch);
    command.arg("--version");
    let output = run_to_exit(command, Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "combined {} (protocol {})\n",
            env!("CARGO_PKG_VERSION"),
            combine_protocol::PROTOCOL_VERSION
        )
    );

    let mut command = combined_command(&scratch);
    command.arg("frobnicate");
    let output = run_to_exit(command, Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("combined: unknown command frobnicate"),
        "{stderr}"
    );
    assert!(stderr.contains("usage:"), "{stderr}");
}
