#![cfg(feature = "test-support")]
//! The daemon's server in this process, over a real Unix socket and the fake backend: binding,
//! the flush cadence, the first-frame rule, and the terminal list.

mod common;
mod daemon_support;

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use combine_protocol::{
    AttachmentId, ClientMessage, Cursor, DaemonMessage, ErrorCode, Hello, HostId, RequestId,
    TermId, TerminalState, Terminals, WatchTerminals, MAX_FRAME_LEN, PROTOCOL_VERSION,
};
use combined::adapters::fs::data_dir::DataDir;
use combined::model::Millis;
use combined::rpc::connect::relay_input;
use combined::rpc::flush::{FlushAt, FlushSchedule};
use combined::rpc::socket::{bind, BindError};
use combined::rpc::{ServeOptions, ServerHandle, ServerStats};
use combined::testing::{FakeBackend, FakeConfig, FakeControl};
use common::{request_options, spawn_spec};
use daemon_support::{
    connect_and_never_read, fake_server, wait_until, work_dir, DaemonPaths, ShortTempDir,
    WireClient,
};
use futures::executor::block_on;

const HOST: &str = "5b0c1f3e-6a3d-4c52-9d0e-2f7a1b8c4d90";
const WAIT: Duration = Duration::from_secs(5);

// ---- binding ----

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn bind_creates_a_0600_socket_in_a_0700_directory() {
    let scratch = ShortTempDir::new();
    let dir = scratch.path().join("run");
    let path = dir.join("combined.sock");
    let bound = bind(&path).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&path), 0o600);
    assert!(fs::metadata(&path).unwrap().file_type().is_socket());
    assert!(dir.join("combined.sock.lock").is_file());
    UnixStream::connect(&path).unwrap();
    drop(bound);
}

#[test]
fn bind_replaces_a_stale_socket() {
    let scratch = ShortTempDir::new();
    let path = scratch.sock("s.sock");
    drop(bind(&path).unwrap());
    assert!(fs::metadata(&path).unwrap().file_type().is_socket());
    assert!(UnixStream::connect(&path).is_err());

    let again = bind(&path).unwrap();
    UnixStream::connect(&path).unwrap();
    drop(again);
}

#[test]
fn one_socket_path_with_two_data_dirs_refuses_the_second_bind() {
    let scratch = ShortTempDir::new();
    let _a = DataDir::open(&scratch.path().join("a")).unwrap();
    let _b = DataDir::open(&scratch.path().join("b")).unwrap();
    let path = scratch.sock("shared.sock");

    let first = bind(&path).unwrap();
    let second = bind(&path);
    assert!(matches!(second, Err(BindError::InUse { .. })), "{second:?}");

    first.listener.set_nonblocking(false).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    first.listener.accept().unwrap();
    drop(client);
    assert!(fs::metadata(&path).unwrap().file_type().is_socket());
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn bind_refuses_a_second_bind_while_the_lock_is_held_and_nothing_listens() {
    let scratch = ShortTempDir::new();
    let path = scratch.sock("locked.sock");
    let first = bind(&path).unwrap();
    drop(first.listener);
    assert!(UnixStream::connect(&path).is_err());

    let second = bind(&path);
    assert!(matches!(second, Err(BindError::InUse { .. })), "{second:?}");
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
    // `first` still holds its lock here: only the listener was moved out of it.
}

#[test]
fn bind_refuses_a_live_listener_without_the_lock() {
    let scratch = ShortTempDir::new();
    let path = scratch.sock("foreign.sock");
    let foreign = UnixListener::bind(&path).unwrap();

    let result = bind(&path);
    assert!(matches!(result, Err(BindError::InUse { .. })), "{result:?}");
    assert_eq!(result.unwrap_err().exit_code(), 2);
    UnixStream::connect(&path).unwrap();
    drop(foreign);
}

#[test]
fn bind_refuses_a_path_that_is_not_a_socket() {
    let scratch = ShortTempDir::new();
    let path = scratch.sock("plain");
    fs::write(&path, b"keep me").unwrap();

    let result = bind(&path);
    assert!(
        matches!(result, Err(BindError::NotASocket { .. })),
        "{result:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), b"keep me");
}

#[test]
fn bind_refuses_a_directory_open_to_others() {
    let scratch = ShortTempDir::new();
    let dir = scratch.path().join("open");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    let result = bind(&dir.join("s.sock"));
    assert!(
        matches!(result, Err(BindError::DirectoryOpen { .. })),
        "{result:?}"
    );
    assert!(!dir.join("s.sock").exists());
}

#[test]
fn bind_refuses_a_path_over_103_bytes() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("x".repeat(100)).join("s.sock");
    assert!(path.as_os_str().len() > 103);

    let result = bind(&path);
    assert!(
        matches!(result, Err(BindError::TooLong { .. })),
        "{result:?}"
    );
    assert!(!scratch.path().join("x".repeat(100)).exists());
}

// ---- flush schedule ----

#[test]
fn flush_schedule_flushes_at_once_after_an_idle_frame() {
    let mut schedule = FlushSchedule::new(Duration::from_millis(16));
    assert_eq!(schedule.request(Millis(5)), FlushAt::Now);
    schedule.flushed(Millis(5));
    assert_eq!(schedule.request(Millis(21)), FlushAt::Now);
    assert_eq!(schedule.request(Millis(10_000)), FlushAt::Now);
}

#[test]
fn flush_schedule_waits_out_the_interval_while_busy() {
    let mut schedule = FlushSchedule::new(Duration::from_millis(16));
    schedule.flushed(Millis(100));
    assert_eq!(schedule.request(Millis(100)), FlushAt::At(Millis(116)));
    assert_eq!(schedule.request(Millis(105)), FlushAt::At(Millis(116)));
    assert_eq!(schedule.request(Millis(116)), FlushAt::Now);
}

#[test]
fn flush_schedule_refuses_an_early_timer() {
    let mut schedule = FlushSchedule::new(Duration::from_millis(16));
    schedule.flushed(Millis(100));
    assert_eq!(schedule.request(Millis(115)), FlushAt::At(Millis(116)));
    assert_eq!(schedule.request(Millis(116)), FlushAt::Now);
}

#[test]
fn flush_schedule_spaces_continuous_flushes_16ms_apart() {
    let mut schedule = FlushSchedule::new(Duration::from_millis(16));
    let mut now = Millis(0);
    let mut passes = Vec::new();
    while passes.len() < 20 {
        match schedule.request(now) {
            FlushAt::Now => {
                schedule.flushed(now);
                passes.push(now.0);
                now = Millis(now.0 + 1);
            }
            FlushAt::At(at) => now = at,
        }
    }
    for pair in passes.windows(2) {
        assert!(pair[1] - pair[0] >= 16, "{passes:?}");
    }
    assert_eq!(passes[1] - passes[0], 16);
}

// ---- running server ----

struct Env {
    server: ServerHandle,
    backend: Arc<FakeBackend>,
    control: FakeControl,
    socket: PathBuf,
    work: PathBuf,
    scratch: ShortTempDir,
}

impl Env {
    fn new() -> Env {
        Env::with(ServeOptions::default())
    }

    fn with(options: ServeOptions) -> Env {
        let scratch = ShortTempDir::new();
        let (backend, control) = FakeBackend::new(FakeConfig::default());
        let server = fake_server(&scratch, "d", Arc::clone(&backend), HOST, options);
        Env {
            server,
            backend,
            control,
            socket: DaemonPaths::in_scratch(&scratch, "d").socket,
            work: work_dir(&scratch),
            scratch,
        }
    }

    fn stats(&self) -> Arc<ServerStats> {
        self.server.stats()
    }

    fn spawn_terminal(&self) -> TermId {
        let spec = spawn_spec(self.work.to_str().unwrap(), 80, 24);
        block_on(combined::ports::TerminalBackend::spawn(
            &*self.backend,
            spec,
        ))
        .unwrap()
    }

    fn client(&self) -> WireClient {
        WireClient::open(&self.socket).0
    }

    fn wait_connections(&self, count: usize) {
        let stats = self.stats();
        assert!(
            wait_until(WAIT, || stats.connections() == count),
            "connections() is {}, expected {count}",
            stats.connections()
        );
    }
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

fn watch_frame(request: u64) -> Vec<u8> {
    frame(
        1,
        format!(r#"{{"type":"watch_terminals","request":{request}}}"#).as_bytes(),
    )
}

/// Attaches and returns the attachment, with everything up to and including `started` read.
fn attach(client: &mut WireClient, term: &TermId) -> (AttachmentId, Vec<DaemonMessage>) {
    client.send(&ClientMessage::Attach(combine_protocol::Attach {
        request: RequestId(1),
        terminal: term.clone(),
        options: request_options(80, 24, false),
    }));
    let mut seen = Vec::new();
    loop {
        let message = client.recv();
        let done = matches!(message, DaemonMessage::Started(_));
        seen.push(message);
        if done {
            break;
        }
    }
    let attachment = seen
        .iter()
        .find_map(|message| match message {
            DaemonMessage::Attached(attached) => Some(attached.attachment),
            _ => None,
        })
        .unwrap();
    (attachment, seen)
}

/// Reads until the bytes of `attachment`'s data end with `tail`, acking each event.
fn read_until_tail(
    client: &mut WireClient,
    attachment: AttachmentId,
    tail: &[u8],
) -> (Vec<u8>, Vec<Cursor>) {
    let mut bytes = Vec::new();
    let mut cursors = Vec::new();
    while !bytes.ends_with(tail) {
        let message = client.recv();
        if let Some(cursor) = message.cursor().filter(|c| c.attachment == attachment) {
            cursors.push(cursor);
            if let DaemonMessage::Data(data) = &message {
                bytes.extend_from_slice(&data.bytes);
            }
            client.ack(cursor);
        }
    }
    (bytes, cursors)
}

fn next_terminals(client: &WireClient) -> Terminals {
    loop {
        if let DaemonMessage::Terminals(terminals) = client.recv() {
            return terminals;
        }
    }
}

// ---- AC6: ordering and cadence ----

#[test]
fn flushes_are_never_closer_than_16ms_under_continuous_output() {
    let env = Env::new();
    let term = env.spawn_terminal();
    let mut client = env.client();
    let (attachment, _) = attach(&mut client, &term);
    let stats = env.stats();
    let flushes_before = stats.flushes();

    let control = env.control.clone();
    let emit_term = term.clone();
    let start = Instant::now();
    let emitter = thread::spawn(move || {
        for _ in 0..240 {
            control.emit(&emit_term, &[b'x'; 512]);
            thread::sleep(Duration::from_millis(2));
        }
        control.emit(&emit_term, b"END");
    });
    let (bytes, _) = read_until_tail(&mut client, attachment, b"END");
    emitter.join().unwrap();
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap();

    assert!(bytes.len() >= 240 * 512);
    assert!(
        stats.min_flush_gap() >= Some(16),
        "min gap {:?}",
        stats.min_flush_gap()
    );
    let flushes = stats.flushes() - flushes_before;
    assert!(
        flushes >= 2.max(elapsed_ms / 160),
        "{flushes} flushes in {elapsed_ms} ms"
    );
}

#[test]
fn control_messages_precede_stream_messages_in_a_flush() {
    let env = Env::new();
    let term = env.spawn_terminal();
    let mut client = env.client();
    let (_, seen) = attach(&mut client, &term);

    let position = |name: &str| seen.iter().position(|m| m.type_name() == name).unwrap();
    assert!(position("attached") < position("ownership"));
    assert!(position("ownership") < position("started"));
}

#[test]
fn two_attachments_on_one_connection_keep_their_order() {
    let env = Env::new();
    let (first, second) = (env.spawn_terminal(), env.spawn_terminal());
    let mut client = env.client();
    let (a, _) = attach(&mut client, &first);
    let (b, _) = attach(&mut client, &second);

    for n in 0..20 {
        env.control.emit(&first, format!("a{n:02}.").as_bytes());
        env.control.emit(&second, format!("b{n:02}.").as_bytes());
    }
    let expected = |letter: char| {
        (0..20)
            .map(|n| format!("{letter}{n:02}."))
            .collect::<String>()
    };
    let mut seen: [(Vec<u8>, Vec<Cursor>); 2] = Default::default();
    let tails = [expected('a'), expected('b')];
    let deadline = Instant::now() + WAIT;
    while seen
        .iter()
        .zip(&tails)
        .any(|((bytes, _), tail)| !bytes.ends_with(tail.as_bytes()))
    {
        assert!(Instant::now() < deadline, "output did not arrive");
        let Some(message) = client.recv_within(Duration::from_millis(100)) else {
            continue;
        };
        let Some(cursor) = message.cursor() else {
            continue;
        };
        let slot = usize::from(cursor.attachment == b);
        assert!(cursor.attachment == a || cursor.attachment == b);
        seen[slot].1.push(cursor);
        if let DaemonMessage::Data(data) = &message {
            seen[slot].0.extend_from_slice(&data.bytes);
        }
        client.ack(cursor);
    }
    for (_, cursors) in &seen {
        assert!(cursors.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    }
}

// ---- AC11: the first frame and what follows ----

fn expect_error(client: &WireClient, code: ErrorCode) -> String {
    match client.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, code, "{}", error.message);
            error.message
        }
        other => panic!("expected an error, got {other:?}"),
    }
}

fn first_frame_gets_malformed_then_close(bytes: &[u8], half_close: bool) -> String {
    let env = Env::new();
    let mut client = WireClient::connect(&env.socket);
    client.send_bytes(bytes);
    if half_close {
        client.shutdown_write();
    }
    let message = expect_error(&client, ErrorCode::Malformed);
    client.expect_closed();
    env.wait_connections(0);
    message
}

#[test]
fn undecodable_first_frame_gets_malformed_then_close() {
    let message = first_frame_gets_malformed_then_close(&frame(1, b"not json"), false);
    assert!(message.starts_with("malformed frame: "), "{message}");
}

#[test]
fn unknown_kind_first_frame_gets_malformed_then_close() {
    let message = first_frame_gets_malformed_then_close(&frame(9, b"x"), false);
    assert!(message.contains("unknown frame kind 9"), "{message}");
}

#[test]
fn oversized_first_frame_gets_malformed_then_close() {
    let header = (MAX_FRAME_LEN + 1).to_be_bytes();
    let message = first_frame_gets_malformed_then_close(&header, false);
    assert!(message.starts_with("malformed frame: "), "{message}");
}

#[test]
fn truncated_first_frame_gets_malformed_then_close() {
    let mut bytes = 10u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"\x01{\"");
    let message = first_frame_gets_malformed_then_close(&bytes, true);
    assert!(
        message.starts_with("malformed frame: truncated frame"),
        "{message}"
    );
}

#[test]
fn non_hello_first_message_gets_invalid_request_then_close() {
    let env = Env::new();
    let mut client = WireClient::connect(&env.socket);
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    let message = expect_error(&client, ErrorCode::InvalidRequest);
    assert_eq!(message, "expected hello, got watch_terminals");
    client.expect_closed();
    env.wait_connections(0);
}

#[test]
fn undecodable_frame_after_handshake_gets_malformed_then_close() {
    let env = Env::new();
    let mut client = env.client();
    env.wait_connections(1);
    client.send_bytes(&frame(1, b"not json"));
    let message = expect_error(&client, ErrorCode::Malformed);
    assert!(message.starts_with("malformed frame: "), "{message}");
    client.expect_closed();
    env.wait_connections(0);
}

#[test]
fn truncated_frame_after_handshake_gets_malformed_then_close() {
    let env = Env::new();
    let mut client = env.client();
    let mut bytes = 10u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"\x01{\"");
    client.send_bytes(&bytes);
    client.shutdown_write();
    let message = expect_error(&client, ErrorCode::Malformed);
    assert!(
        message.starts_with("malformed frame: truncated frame"),
        "{message}"
    );
    client.expect_closed();
    env.wait_connections(0);
}

#[test]
fn handshake_timeout_closes_a_silent_connection() {
    let env = Env::with(ServeOptions {
        handshake_timeout: Duration::from_millis(200),
        ..ServeOptions::default()
    });
    let client = WireClient::connect(&env.socket);
    env.wait_connections(1);
    client.expect_closed();
    env.wait_connections(0);
}

#[test]
fn handshake_deadline_is_absolute_not_per_read() {
    let env = Env::with(ServeOptions {
        handshake_timeout: Duration::from_millis(500),
        ..ServeOptions::default()
    });
    let mut client = WireClient::connect(&env.socket);
    let hello = combine_protocol::WireMessage::encode(&ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
    }))
    .unwrap();
    let start = Instant::now();
    let mut closed_after = None;
    // One byte every 80 ms never leaves a read idle for 500 ms, yet the whole hello would take
    // longer than the deadline.
    for byte in &hello[..16] {
        if client.try_send_bytes(&[*byte]).is_err() {
            closed_after = Some(start.elapsed());
            break;
        }
        thread::sleep(Duration::from_millis(80));
    }
    let closed_after = closed_after.expect("the daemon kept reading a hello past its deadline");
    assert!(
        closed_after < Duration::from_millis(900),
        "closed after {closed_after:?}"
    );
    client.expect_closed();
    env.wait_connections(0);
}

#[test]
fn client_eof_disconnects_and_keeps_attachments_for_resume() {
    let env = Env::new();
    let term = env.spawn_terminal();
    let mut client = env.client();
    let (attachment, _) = attach(&mut client, &term);
    env.control.emit(&term, b"one");
    let (_, cursors) = read_until_tail(&mut client, attachment, b"one");
    let last = *cursors.last().unwrap();
    drop(client);
    env.wait_connections(0);

    env.control.emit(&term, b"two");
    let mut second = env.client();
    second.send(&ClientMessage::Resume(combine_protocol::Resume {
        request: RequestId(2),
        cursor: last,
    }));
    loop {
        match second.recv() {
            DaemonMessage::Resumed(resumed) => {
                assert_eq!(resumed.attachment, attachment);
                break;
            }
            DaemonMessage::Terminals(_) => {}
            other => panic!("expected resumed, got {other:?}"),
        }
    }
    let (bytes, _) = read_until_tail(&mut second, attachment, b"two");
    assert_eq!(bytes, b"two");
}

#[test]
fn connections_over_the_limit_are_closed() {
    let env = Env::with(ServeOptions {
        max_connections: 2,
        ..ServeOptions::default()
    });
    let mut first = env.client();
    let second = env.client();
    env.wait_connections(2);

    let third = WireClient::connect(&env.socket);
    third.expect_closed();
    assert_eq!(env.stats().connections(), 2);

    first.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    assert_eq!(next_terminals(&first).request, Some(RequestId(1)));
    drop(second);
    env.wait_connections(1);
}

#[test]
fn a_closing_connection_still_counts_toward_the_limit_until_its_writer_is_done() {
    const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
    let env = Env::with(ServeOptions {
        max_connections: 1,
        write_timeout: WRITE_TIMEOUT,
        ..ServeOptions::default()
    });
    let mut spec = spawn_spec(env.work.to_str().unwrap(), 80, 24);
    spec.argv.push("x".repeat(512 * 1024));
    block_on(combined::ports::TerminalBackend::spawn(&*env.backend, spec)).unwrap();

    let mut first = connect_and_never_read(&env.socket);
    env.wait_connections(1);
    std::io::Write::write_all(&mut first, &watch_frame(1)).unwrap();
    thread::sleep(Duration::from_millis(200));
    let malformed_at = Instant::now();
    std::io::Write::write_all(&mut first, &frame(1, b"not json")).unwrap();
    thread::sleep(Duration::from_millis(200));

    // The writer is stuck on the unread snapshot, so the connection is still closing and
    // still holds the only slot: every newcomer is refused without a byte.
    let mut refused = 0;
    while malformed_at.elapsed() < WRITE_TIMEOUT / 2 {
        assert_eq!(env.stats().connections(), 1);
        WireClient::connect(&env.socket).expect_closed();
        refused += 1;
    }
    assert!(refused >= 1);

    env.wait_connections(0);
    let (_admitted, welcome) = WireClient::open(&env.socket);
    assert_eq!(welcome.host_id, HostId(HOST.to_string()));
}

#[test]
fn removed_connection_is_counted_once_and_its_queued_messages_are_dropped() {
    let env = Env::with(ServeOptions {
        flush_interval: Duration::from_secs(10),
        ..ServeOptions::default()
    });
    let term = env.spawn_terminal();
    let mut stream = connect_and_never_read(&env.socket);
    env.wait_connections(1);

    let mut burst: Vec<u8> = (1..=40).flat_map(watch_frame).collect();
    let attach = ClientMessage::Attach(combine_protocol::Attach {
        request: RequestId(99),
        terminal: term.clone(),
        options: request_options(80, 24, false),
    });
    burst.extend(combine_protocol::WireMessage::encode(&attach).unwrap());
    std::io::Write::write_all(&mut stream, &burst).unwrap();

    env.wait_connections(0);
    let (_fresh, welcome) = WireClient::open(&env.socket);
    assert_eq!(welcome.host_id, HostId(HOST.to_string()));
    env.wait_connections(1);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(env.stats().connections(), 1);
    assert!(env.control.attachments(&term).is_empty());
}

// ---- AC12: serving ----

#[test]
fn welcome_carries_the_host_id() {
    let env = Env::new();
    let (_client, welcome) = WireClient::open(&env.socket);
    assert_eq!(welcome.host_id, HostId(HOST.to_string()));
    assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
}

#[test]
fn v1_hello_gets_version_mismatch_then_close() {
    let env = Env::new();
    let mut client = WireClient::connect(&env.socket);
    client.send(&ClientMessage::Hello(Hello {
        protocol_version: 1,
    }));
    let message = expect_error(&client, ErrorCode::VersionMismatch);
    assert_eq!(
        message,
        format!("protocol version mismatch: client 1, daemon {PROTOCOL_VERSION}")
    );
    client.expect_closed();
    env.wait_connections(0);
}

#[test]
fn watch_terminals_answers_with_a_snapshot() {
    let env = Env::new();
    let term = env.spawn_terminal();
    let mut client = env.client();
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(7),
    }));

    let snapshot = next_terminals(&client);
    assert_eq!(snapshot.request, Some(RequestId(7)));
    assert_eq!(snapshot.terminals.len(), 1);
    assert_eq!(snapshot.terminals[0].id, term);
    assert_eq!(snapshot.terminals[0].state, TerminalState::Running);
    assert_eq!(snapshot.spaces.len(), 1);
    assert_eq!(snapshot.spaces[0].id, env.work.to_str().unwrap());
    assert_eq!(snapshot.spaces[0].terminals, vec![term]);
}

#[test]
fn terminal_changes_push_a_new_snapshot() {
    let env = Env::new();
    let first = env.spawn_terminal();
    let mut client = env.client();
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(7),
    }));
    assert_eq!(next_terminals(&client).terminals.len(), 1);

    let until = |client: &WireClient, done: &dyn Fn(&Terminals) -> bool| loop {
        let snapshot = next_terminals(client);
        assert_eq!(snapshot.request, None);
        if done(&snapshot) {
            return snapshot;
        }
    };

    let second = env.spawn_terminal();
    let snapshot = until(&client, &|t| t.terminals.len() == 2);
    assert!(snapshot.terminals.iter().any(|info| info.id == second));

    let moved = env.scratch.path().join("elsewhere");
    env.control.set_cwd(&first, moved.to_str().unwrap());
    let snapshot = until(&client, &|t| {
        t.terminals
            .iter()
            .any(|info| info.id == first && info.cwd == moved.to_str().unwrap())
    });
    assert_eq!(snapshot.terminals.len(), 2);

    env.control.exit(&first, Some(0));
    let snapshot = until(&client, &|t| {
        t.terminals
            .iter()
            .any(|info| info.id == first && info.state == TerminalState::Exited)
    });
    let info = snapshot.terminals.iter().find(|i| i.id == first).unwrap();
    assert_eq!(info.exit_code, Some(0));
}

#[test]
fn unread_watch_answers_over_the_bound_disconnect() {
    let env = Env::new();
    let mut spec = spawn_spec(env.work.to_str().unwrap(), 80, 24);
    spec.argv.push("x".repeat(4096));
    block_on(combined::ports::TerminalBackend::spawn(&*env.backend, spec)).unwrap();

    let mut stream = connect_and_never_read(&env.socket);
    env.wait_connections(1);
    let burst: Vec<u8> = (1..=64).flat_map(watch_frame).collect();
    std::io::Write::write_all(&mut stream, &burst).unwrap();

    env.wait_connections(0);
}

#[test]
fn a_client_that_never_reads_is_closed_by_the_write_timeout() {
    let env = Env::with(ServeOptions {
        write_timeout: Duration::from_millis(200),
        ..ServeOptions::default()
    });
    let mut spec = spawn_spec(env.work.to_str().unwrap(), 80, 24);
    spec.argv.push("x".repeat(200 * 1024));
    block_on(combined::ports::TerminalBackend::spawn(&*env.backend, spec)).unwrap();

    let mut stream = connect_and_never_read(&env.socket);
    env.wait_connections(1);
    // Twelve answers, spread over many flushes: under the bound of 16 unflushed answers, so
    // only the write timeout can end the connection.
    for request in 1..=12 {
        if std::io::Write::write_all(&mut stream, &watch_frame(request)).is_err() {
            break; // the daemon has already closed the connection
        }
        thread::sleep(Duration::from_millis(30));
    }
    env.wait_connections(0);
}

#[test]
fn relaying_input_half_closes_at_eof_and_reports_a_read_error() {
    struct Failing;
    impl std::io::Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("stdin failed"))
        }
    }

    let (mut near, mut far) = UnixStream::pair().unwrap();
    relay_input(&mut &b"frame"[..], &mut near).unwrap();
    let mut received = Vec::new();
    std::io::Read::read_to_end(&mut far, &mut received).unwrap();
    assert_eq!(received, b"frame");

    let (mut near, far) = UnixStream::pair().unwrap();
    let error = relay_input(&mut Failing, &mut near).unwrap_err();
    assert_eq!(error.to_string(), "stdin failed");
    far.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut byte = [0; 1];
    let still_open = std::io::Read::read(&mut &far, &mut byte).unwrap_err();
    assert!(matches!(
        still_open.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
}

#[test]
fn oversized_snapshot_becomes_an_internal_error() {
    let env = Env::new();
    let mut spec = spawn_spec(env.work.to_str().unwrap(), 80, 24);
    spec.argv.push("x".repeat(MAX_FRAME_LEN as usize));
    block_on(combined::ports::TerminalBackend::spawn(&*env.backend, spec)).unwrap();

    let mut client = env.client();
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(3),
    }));
    match client.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, ErrorCode::Internal);
            assert_eq!(error.message, "the terminal list is larger than one frame");
            assert_eq!(error.request, Some(RequestId(3)));
        }
        other => panic!("expected an error, got {other:?}"),
    }
}
