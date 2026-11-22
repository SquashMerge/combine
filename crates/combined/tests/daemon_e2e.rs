#![cfg(feature = "test-support")]
//! The real `combined connect` bridge in front of a daemon in this process, on the fake
//! backend, over real Unix sockets and pipes.

mod common;
mod daemon_support;

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Stdio;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use combine_protocol::{
    AttachmentId, ClientMessage, CloseReason, Cursor, DaemonMessage, Detach, ErrorCode, HostId,
    RequestId, Resume, TermId, WatchTerminals, PROTOCOL_VERSION,
};
use combined::rpc::ServeOptions;
use combined::testing::{FakeBackend, FakeConfig};
use common::{request_options, spawn_spec};
use daemon_support::{
    combined_command, fake_server, run_to_exit, wait_until, work_dir, BridgeClient, DaemonPaths,
    ShortTempDir,
};
use futures::executor::block_on;

const HOST: &str = "5b0c1f3e-6a3d-4c52-9d0e-2f7a1b8c4d90";

/// Attaches through the bridge, reading up to and including `started`.
fn attach(bridge: &mut BridgeClient, term: &TermId) -> AttachmentId {
    bridge.send(&ClientMessage::Attach(combine_protocol::Attach {
        request: RequestId(2),
        terminal: term.clone(),
        options: request_options(80, 24, false),
    }));
    let mut attachment = None;
    loop {
        match bridge.recv() {
            DaemonMessage::Attached(attached) => attachment = Some(attached.attachment),
            DaemonMessage::Started(_) => return attachment.expect("started before attached"),
            _ => {}
        }
    }
}

/// Reads the attachment's events until its data ends with `tail`, acking each when `ack`.
/// Returns the data and every event cursor.
fn read_until_tail(bridge: &mut BridgeClient, tail: &[u8], ack: bool) -> (Vec<u8>, Vec<Cursor>) {
    let mut bytes = Vec::new();
    let mut cursors = Vec::new();
    while !bytes.ends_with(tail) {
        let message = bridge.recv();
        if let Some(cursor) = message.cursor() {
            cursors.push(cursor);
            if let DaemonMessage::Data(data) = &message {
                bytes.extend_from_slice(&data.bytes);
            }
            if ack {
                bridge.ack(cursor);
            }
        }
    }
    (bytes, cursors)
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(payload.len() + 1)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.push(kind);
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn attach_replay_and_resume_through_combined_connect() {
    let scratch = ShortTempDir::new();
    let (backend, control) = FakeBackend::new(FakeConfig::default());
    let work = work_dir(&scratch);
    let term = block_on(combined::ports::TerminalBackend::spawn(
        &*backend,
        spawn_spec(work.to_str().unwrap(), 80, 24),
    ))
    .unwrap();
    let server = fake_server(
        &scratch,
        "d",
        Arc::clone(&backend),
        HOST,
        ServeOptions::default(),
    );
    let stats = server.stats();
    let socket = DaemonPaths::in_scratch(&scratch, "d").socket;

    let (mut first, welcome) = BridgeClient::open(&scratch, &socket);
    assert_eq!(welcome.host_id, HostId(HOST.to_string()));
    first.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    let listed = loop {
        if let DaemonMessage::Terminals(terminals) = first.recv() {
            break terminals;
        }
    };
    assert_eq!(listed.request, Some(RequestId(1)));
    assert_eq!(listed.terminals[0].id, term);

    let attachment = attach(&mut first, &listed.terminals[0].id);
    control.emit(&term, b"one");
    let (_, cursors) = read_until_tail(&mut first, b"one", true);
    let c1 = *cursors.last().unwrap();
    control.emit(&term, b"two");
    read_until_tail(&mut first, b"two", false);

    first.kill();
    assert!(
        wait_until(Duration::from_secs(5), || stats.connections() == 0),
        "the daemon kept the connection of the killed bridge"
    );
    control.emit(&term, b"three");

    let (mut second, _) = BridgeClient::open(&scratch, &socket);
    second.send(&ClientMessage::Resume(Resume {
        request: RequestId(3),
        cursor: c1,
    }));
    let replay_through = loop {
        match second.recv() {
            DaemonMessage::Resumed(resumed) => {
                assert_eq!(resumed.attachment, attachment);
                break resumed.replay_through.expect("something was replayed");
            }
            DaemonMessage::Terminals(_) => {}
            other => panic!("expected resumed, got {other:?}"),
        }
    };
    let (replayed, cursors) = read_until_tail(&mut second, b"twothree", true);
    assert_eq!(replayed, b"twothree");
    assert!(cursors.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert!(cursors[0].seq > c1.seq);
    assert_eq!(*cursors.last().unwrap(), replay_through);

    control.emit(&term, b"four");
    let (live, cursors) = read_until_tail(&mut second, b"four", true);
    assert_eq!(live, b"four");
    assert_eq!(cursors[0].seq.0, replay_through.seq.0 + 1);

    second.send(&ClientMessage::Detach(Detach { attachment }));
    loop {
        match second.recv() {
            DaemonMessage::Closed(closed) => {
                assert_eq!(closed.reason, CloseReason::Detached);
                break;
            }
            DaemonMessage::Data(_) => {}
            other => panic!("expected closed, got {other:?}"),
        }
    }

    second.close_stdin();
    assert_eq!(second.wait_exit().code(), Some(0));
    assert_eq!(second.stderr(), "");
}

#[test]
fn combined_connect_relays_only_frames_and_exits_0_when_the_daemon_closes() {
    let scratch = ShortTempDir::new();
    let (backend, _control) = FakeBackend::new(FakeConfig::default());
    let _server = fake_server(&scratch, "d", backend, HOST, ServeOptions::default());
    let socket = DaemonPaths::in_scratch(&scratch, "d").socket;

    let (mut bridge, welcome) = BridgeClient::open(&scratch, &socket);
    assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
    // An undecodable frame makes the daemon answer and close; stdin stays open.
    bridge.send_bytes(&frame(1, b"not json"));
    match bridge.recv() {
        DaemonMessage::Error(error) => assert_eq!(error.code, ErrorCode::Malformed),
        other => panic!("expected an error, got {other:?}"),
    }
    assert_eq!(bridge.wait_exit().code(), Some(0));
    assert_eq!(bridge.stderr(), "");
}

#[test]
fn combined_connect_without_a_daemon_fails_on_stderr_only() {
    let scratch = ShortTempDir::new();
    let socket = scratch.sock("none.sock");
    let mut command = combined_command(&scratch);
    command
        .args(["connect", "--socket"])
        .arg(&socket)
        .stdin(Stdio::null());
    let output = run_to_exit(command, Duration::from_secs(10));

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("combined: "), "{stderr}");
    assert!(stderr.contains(socket.to_str().unwrap()), "{stderr}");
    assert!(!socket.exists(), "connect must not create the socket");
}

#[test]
fn v1_client_through_combined_connect_gets_version_mismatch() {
    let scratch = ShortTempDir::new();
    let (backend, _control) = FakeBackend::new(FakeConfig::default());
    let _server = fake_server(&scratch, "d", backend, HOST, ServeOptions::default());
    let socket = DaemonPaths::in_scratch(&scratch, "d").socket;

    let mut bridge = BridgeClient::spawn(&scratch, &socket);
    bridge.send(&ClientMessage::Hello(combine_protocol::Hello {
        protocol_version: 1,
    }));
    match bridge.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, ErrorCode::VersionMismatch);
            assert_eq!(
                error.message,
                format!("protocol version mismatch: client 1, daemon {PROTOCOL_VERSION}")
            );
        }
        other => panic!("expected an error, got {other:?}"),
    }
    assert_eq!(bridge.wait_exit().code(), Some(0));
}

#[test]
fn combined_connect_exits_1_when_stdin_cannot_be_read() {
    let scratch = ShortTempDir::new();
    let socket = scratch.sock("stdin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    // Reading a directory is an error on every platform Combine runs on.
    let unreadable = File::open(scratch.path()).unwrap();
    let mut command = combined_command(&scratch);
    command
        .args(["connect", "--socket"])
        .arg(&socket)
        .stdin(Stdio::from(unreadable));
    let runner = thread::spawn(move || run_to_exit(command, Duration::from_secs(10)));
    let (_daemon_side, _) = listener.accept().unwrap();
    let output = runner.join().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

/// Fills the bridge's way to stdout from the daemon side, then drops the daemon side.
///
/// The bridge stops reading the socket once its stdout pipe (which nobody reads) is full, so it
/// cannot see the daemon's close until the test drains the pipe. The fill stops once writes
/// have stalled for 300 ms *and* at least 64 KiB went through (a blocked bridge takes about 80 KiB
/// here: its stdout pipe plus the chunk in its hand and the socket buffer), so the bridge really
/// is blocked on its stdout. A bridge that
/// was starved before it got that far keeps the loop going until it is scheduled again. A
/// 30 s deadline bounds a bridge that never reads, and the function panics if it was the
/// deadline that ended the fill: the precondition then did not hold.
fn fill_then_close(daemon_side: UnixStream) {
    daemon_side.set_nonblocking(true).unwrap();
    let chunk = vec![b'x'; 64 * 1024];
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut written = 0usize;
    let mut last_progress = Instant::now();
    while Instant::now() < deadline
        && (written < 64 * 1024 || last_progress.elapsed() < Duration::from_millis(300))
    {
        match (&daemon_side).write(&chunk) {
            Ok(n) => {
                written += n;
                last_progress = Instant::now();
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{error}"),
        }
    }
    assert!(
        written >= 64 * 1024 && last_progress.elapsed() >= Duration::from_millis(300),
        "the bridge's stdout never stopped making progress ({written} bytes written)"
    );
    drop(daemon_side);
}

/// Where a bridge started by [`traced_bridge`] records the branch its input thread took.
fn trace_file(scratch: &ShortTempDir) -> std::path::PathBuf {
    scratch.path().join("connect-trace")
}

fn trace_has(scratch: &ShortTempDir, wanted: &str) -> bool {
    fs::read_to_string(trace_file(scratch)).is_ok_and(|trace| trace.contains(wanted))
}

/// Drains the bridge's stdout and stderr and waits for it to exit.
fn drain(child: &mut std::process::Child) -> (Vec<u8>, String, std::process::ExitStatus) {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut relayed = Vec::new();
    stdout.read_to_end(&mut relayed).unwrap();
    let mut errors = String::new();
    stderr.read_to_string(&mut errors).unwrap();
    (relayed, errors, child.wait().unwrap())
}

/// The daemon has closed while the bridge is still stuck writing to a stdout nobody reads, and
/// then input reaches the dead socket: the write fails with a broken pipe. That means the
/// daemon closed, so the bridge still exits 0 with nothing on stderr.
///
/// The test fails unless the fill really blocked the bridge and the bridge's own trace
/// (`COMBINED_CONNECT_TRACE`, test-support builds) shows its input write hit the closed socket.
#[test]
fn combined_connect_exits_0_when_input_reaches_a_socket_the_daemon_already_closed() {
    let scratch = ShortTempDir::new();
    let socket = scratch.sock("closed.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = combined_command(&scratch)
        .args(["connect", "--socket"])
        .arg(&socket)
        .env("COMBINED_CONNECT_TRACE", trace_file(&scratch))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (daemon_side, _) = listener.accept().unwrap();
    fill_then_close(daemon_side);
    assert!(
        child.try_wait().unwrap().is_none(),
        "the bridge exited before the daemon closed"
    );

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"after the close").unwrap();
    drop(stdin);
    assert!(
        wait_until(Duration::from_secs(10), || trace_has(
            &scratch,
            "input write failed"
        )),
        "the input thread never wrote to the closed socket"
    );

    let (relayed, errors, status) = drain(&mut child);
    assert!(relayed.iter().all(|byte| *byte == b'x'));
    assert_eq!(errors, "");
    assert_eq!(status.code(), Some(0));
    assert!(fs::metadata(&socket).is_ok());
}

/// The same race, but stdin ends without a byte, so the first thing the input thread does
/// after the daemon closed is the half-close. On platforms where that fails with `ENOTCONN`
/// (macOS), the bridge must still exit 0 and say nothing on stderr, and the test fails unless
/// the trace shows that failure happened. Elsewhere the half-close succeeds and nothing
/// remains to observe.
#[test]
fn combined_connect_exits_0_when_the_half_close_reaches_a_socket_the_daemon_already_closed() {
    let scratch = ShortTempDir::new();
    let socket = scratch.sock("halfclose.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = combined_command(&scratch)
        .args(["connect", "--socket"])
        .arg(&socket)
        .env("COMBINED_CONNECT_TRACE", trace_file(&scratch))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (daemon_side, _) = listener.accept().unwrap();
    fill_then_close(daemon_side);
    assert!(
        child.try_wait().unwrap().is_none(),
        "the bridge exited before the daemon closed"
    );

    drop(child.stdin.take());
    if cfg!(target_os = "macos") {
        assert!(
            wait_until(Duration::from_secs(10), || trace_has(
                &scratch,
                "half-close failed"
            )),
            "the half-close never failed on the closed socket"
        );
    }

    let (relayed, errors, status) = drain(&mut child);
    assert!(relayed.iter().all(|byte| *byte == b'x'));
    assert_eq!(errors, "");
    assert_eq!(status.code(), Some(0));
}
