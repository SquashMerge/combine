#![cfg(test)]
//! Test doubles for the host layer: a protocol daemon over real frames, a scripted connector,
//! a fake clock and recording sinks. Nothing here starts a real `ssh`, opens a network
//! listener or talks to a herdr-core.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use combine_protocol::{
    daemon_handshake, read_message, write_message, ClientMessage, DaemonMessage, ErrorCode,
    ErrorMessage, HostId, LayoutSnapshot, RequestId, Welcome, WireMessage,
};

use super::config::StoreError;
use super::identity::HostIdStore;
use super::link::{Closer, ConnectError, Connector, ExitInfo, ExitReport, FailureText, Link};
use super::model::{ConnectionState, HostsSnapshot, SpaceView};
use super::service::SnapshotSink;
use super::session::LayoutSink;
use super::supervisor::{Clock, HostEvents, StopSignal, Wake};

pub const HOST_A: &str = "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c";
pub const HOST_B: &str = "0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c";

/// How long a test waits for something that should already be happening.
pub const WAIT: Duration = Duration::from_secs(5);

pub fn host_id(s: &str) -> HostId {
    HostId(s.to_string())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A scratch directory `/tmp/cs5-<pid>-<n>`, short enough for a Unix socket path. Removed on drop.
pub struct ShortTempDir {
    path: PathBuf,
}

impl ShortTempDir {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = PathBuf::from(format!("/tmp/cs5-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("create the scratch directory");
        ShortTempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ShortTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Writes `#!/bin/sh` plus `body`, closes the file, then makes it executable.
pub fn write_fake_ssh(dir: &Path, body: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = dir.join(format!("fake-ssh-{n}"));
    {
        let mut file = fs::File::create(&path).expect("create the fake ssh");
        file.write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
            .expect("write the fake ssh");
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod the fake ssh");
    path
}

/// Writes one encoded `welcome` frame, for a fake ssh to `cat`.
pub fn write_welcome_file(dir: &Path, id: &HostId) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = dir.join(format!("welcome-{n}.bin"));
    let welcome = DaemonMessage::Welcome(Welcome {
        protocol_version: combine_protocol::PROTOCOL_VERSION,
        host_id: id.clone(),
    });
    fs::write(&path, welcome.encode().expect("encode welcome")).expect("write the frame");
    path
}

/// Receives the next value or fails the test after [`WAIT`].
pub fn recv<T>(rx: &Receiver<T>) -> T {
    rx.recv_timeout(WAIT).expect("nothing arrived within 5 s")
}

/// True when the thread ends within `limit`. Nothing sleeps: a helper thread joins and signals.
pub fn joins_within(handle: JoinHandle<()>, limit: Duration) -> bool {
    let (done, joined) = mpsc::channel();
    thread::spawn(move || {
        let _ = handle.join();
        let _ = done.send(());
    });
    joined.recv_timeout(limit).is_ok()
}

enum Script {
    Welcome(HostId),
    WelcomeVersion(u32, HostId),
    WelcomeThenFrame(HostId, Vec<u8>),
    Refuse(ErrorMessage),
    CloseAfterHello,
}

/// A daemon built on real `combine_protocol` frames.
pub struct Double {
    script: Script,
    probe: Probe,
}

impl Double {
    fn with(script: Script) -> Double {
        Double {
            script,
            probe: Probe::default(),
        }
    }

    /// Completes the handshake, then holds the link open until it is dropped or closed.
    pub fn welcome(id: HostId) -> Double {
        Double::with(Script::Welcome(id))
    }

    /// Completes the handshake, writes `frame` as it is, then holds the link open.
    pub fn welcome_then_frame(id: HostId, frame: Vec<u8>) -> Double {
        Double::with(Script::WelcomeThenFrame(id, frame))
    }

    /// Answers the hello with an `error` frame and closes.
    pub fn refuse(error: ErrorMessage) -> Double {
        Double::with(Script::Refuse(error))
    }

    /// Answers the hello with a `welcome` that names protocol version `version`.
    pub fn welcome_version(version: u32, id: HostId) -> Double {
        Double::with(Script::WelcomeVersion(version, id))
    }

    /// Reads the hello and closes.
    pub fn close_after_hello() -> Double {
        Double::with(Script::CloseAfterHello)
    }

    /// A handle that outlives the double's move into a [`ScriptedConnector`].
    pub fn probe(&self) -> Probe {
        self.probe.clone()
    }

    /// Serves one connection over `UnixStream::pair()`. The thread ends when the client closes
    /// the link, so joining it proves the client did.
    pub fn pair(self) -> (Link, DropHandle, JoinHandle<()>) {
        let (client, server) = UnixStream::pair().expect("socket pair");
        let drop_handle = DropHandle {
            stream: server.try_clone().expect("clone the server end"),
        };
        let reader = client.try_clone().expect("clone the client end");
        let shutter = client.try_clone().expect("clone the client end");
        let closer: Closer = Arc::new(move || {
            let _ = shutter.shutdown(Shutdown::Both);
        });
        let link = Link::new(Box::new(reader), Box::new(client), closer, Box::new(NoExit));
        let thread = thread::spawn(move || self.script.serve(server));
        (link, drop_handle, thread)
    }

    fn connect(self) -> Link {
        let probe = self.probe.clone();
        let (link, drop_handle, thread) = self.pair();
        *lock(&probe.slot) = Some((drop_handle, thread));
        link
    }
}

impl Script {
    fn serve(self, mut stream: UnixStream) {
        if let Ok(mut writer) = stream.try_clone() {
            if self.converse(&mut stream, &mut writer) {
                let mut buf = [0u8; 256];
                while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
    }

    /// Plays the daemon's side of the handshake. True when the link should now be held open.
    fn converse(self, reader: &mut UnixStream, writer: &mut UnixStream) -> bool {
        fn hello(reader: &mut UnixStream) -> bool {
            matches!(read_message::<ClientMessage, _>(reader), Ok(Some(_)))
        }
        fn send(writer: &mut UnixStream, message: &DaemonMessage) -> bool {
            write_message(writer, message).is_ok() && writer.flush().is_ok()
        }
        match self {
            Script::Welcome(id) => daemon_handshake(reader, writer, &id).is_ok(),
            Script::WelcomeThenFrame(id, frame) => {
                daemon_handshake(reader, writer, &id).is_ok()
                    && writer.write_all(&frame).is_ok()
                    && writer.flush().is_ok()
            }
            Script::WelcomeVersion(version, host_id) => {
                hello(reader)
                    && send(
                        writer,
                        &DaemonMessage::Welcome(Welcome {
                            protocol_version: version,
                            host_id,
                        }),
                    )
            }
            Script::Refuse(error) => {
                if hello(reader) {
                    send(writer, &DaemonMessage::Error(error));
                }
                false
            }
            Script::CloseAfterHello => {
                hello(reader);
                false
            }
        }
    }
}

/// Closes the server end of a [`Double`], as a daemon that went away would.
pub struct DropHandle {
    stream: UnixStream,
}

impl DropHandle {
    pub fn drop_now(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

type Served = (DropHandle, JoinHandle<()>);

/// Reaches the handles of a [`Double`] after a [`ScriptedConnector`] has taken it.
#[derive(Clone, Default)]
pub struct Probe {
    slot: Arc<Mutex<Option<Served>>>,
}

impl Probe {
    pub fn drop_now(&self) {
        if let Some((handle, _)) = lock(&self.slot).as_ref() {
            handle.drop_now();
        }
    }

    /// True when the double's server thread ended within [`WAIT`], which means the client
    /// closed the link.
    pub fn closed_by_client(&self) -> bool {
        let taken = lock(&self.slot).take();
        taken.is_some_and(|(_, thread)| joins_within(thread, WAIT))
    }
}

struct NoExit;

impl ExitReport for NoExit {
    fn finish(self: Box<Self>) -> Option<ExitInfo> {
        None
    }
}

/// Accepts one connection on a Unix socket at `path` and serves `double` on it. The socket is
/// bound before this returns, so the client can connect at once.
pub fn serve_on_socket(path: &Path, double: Double) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("bind the test socket");
    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            double.script.serve(stream);
        }
    })
}

pub enum Step {
    Serve(Double),
    /// A link that is at EOF and whose child exited with `code` after writing `stderr`.
    Exit {
        stderr: &'static str,
        code: Option<i32>,
    },
    ConnectError(ConnectError),
}

struct ExitInfoReport(ExitInfo);

impl ExitReport for ExitInfoReport {
    fn finish(self: Box<Self>) -> Option<ExitInfo> {
        Some(self.0)
    }
}

/// A connector that plays its steps in order. An exhausted script is a permanent failure, so
/// every supervisor test ends in `failed`.
pub struct ScriptedConnector {
    steps: Mutex<VecDeque<Step>>,
    connects: AtomicUsize,
}

impl ScriptedConnector {
    pub fn new(steps: Vec<Step>) -> Self {
        ScriptedConnector {
            steps: Mutex::new(steps.into()),
            connects: AtomicUsize::new(0),
        }
    }

    pub fn connect_count(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }
}

impl Connector for ScriptedConnector {
    fn connect(&self) -> Result<Link, ConnectError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let step = lock(&self.steps).pop_front();
        match step {
            Some(Step::Serve(double)) => Ok(double.connect()),
            Some(Step::Exit { stderr, code }) => Ok(Link::new(
                Box::new(io::empty()),
                Box::new(io::sink()),
                Arc::new(|| {}),
                Box::new(ExitInfoReport(ExitInfo {
                    code,
                    stderr: stderr.to_string(),
                })),
            )),
            Some(Step::ConnectError(e)) => Err(e),
            None => Err(ConnectError::Permanent(FailureText {
                text: "scripted connector exhausted".to_string(),
                detail: None,
            })),
        }
    }
}

/// Records each requested delay and returns at once. `stop_after(n)` stops on the n-th sleep.
pub struct FakeClock {
    delays: Mutex<Vec<Duration>>,
    stop_on: Option<usize>,
}

impl FakeClock {
    pub fn new() -> Self {
        FakeClock {
            delays: Mutex::new(Vec::new()),
            stop_on: None,
        }
    }

    pub fn stop_after(n: usize) -> Self {
        FakeClock {
            delays: Mutex::new(Vec::new()),
            stop_on: Some(n),
        }
    }

    pub fn delays(&self) -> Vec<Duration> {
        lock(&self.delays).clone()
    }
}

impl Clock for FakeClock {
    fn sleep(&self, delay: Duration, _stop: &StopSignal) -> Wake {
        let mut delays = lock(&self.delays);
        delays.push(delay);
        if self.stop_on == Some(delays.len()) {
            Wake::Stopped
        } else {
            Wake::Elapsed
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connection(String, ConnectionState),
    HostId(String, HostId),
    Spaces(HostId, Vec<SpaceView>),
}

/// Sends every [`HostEvents`] call to a channel.
pub struct RecordingEvents {
    tx: Mutex<Sender<Event>>,
}

impl RecordingEvents {
    pub fn new() -> (Arc<RecordingEvents>, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(RecordingEvents { tx: Mutex::new(tx) }), rx)
    }

    fn send(&self, event: Event) {
        let _ = lock(&self.tx).send(event);
    }
}

impl HostEvents for RecordingEvents {
    fn connection(&self, name: &str, state: ConnectionState) {
        self.send(Event::Connection(name.to_string(), state));
    }

    fn host_id(&self, name: &str, host_id: &HostId) {
        self.send(Event::HostId(name.to_string(), host_id.clone()));
    }

    fn spaces(&self, host_id: &HostId, spaces: Vec<SpaceView>) {
        self.send(Event::Spaces(host_id.clone(), spaces));
    }
}

/// What a [`RecordingLayoutSink`] was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutEvent {
    Layout(HostId, Option<RequestId>, LayoutSnapshot),
    Error(HostId, RequestId, ErrorCode, String),
    Disconnected(HostId),
}

/// Sends every layout call to a channel.
pub struct RecordingLayoutSink {
    tx: Mutex<Sender<LayoutEvent>>,
}

impl RecordingLayoutSink {
    pub fn new() -> (Arc<RecordingLayoutSink>, Receiver<LayoutEvent>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(RecordingLayoutSink { tx: Mutex::new(tx) }), rx)
    }

    fn send(&self, event: LayoutEvent) {
        let _ = lock(&self.tx).send(event);
    }
}

impl LayoutSink for RecordingLayoutSink {
    fn layout(&self, host_id: &HostId, request: Option<RequestId>, snapshot: LayoutSnapshot) {
        self.send(LayoutEvent::Layout(host_id.clone(), request, snapshot));
    }

    fn layout_error(&self, host_id: &HostId, request: RequestId, code: ErrorCode, message: String) {
        self.send(LayoutEvent::Error(host_id.clone(), request, code, message));
    }

    fn layout_disconnected(&self, host_id: &HostId) {
        self.send(LayoutEvent::Disconnected(host_id.clone()));
    }
}

/// Sends every published snapshot to a channel.
pub struct RecordingSink {
    tx: Mutex<Sender<HostsSnapshot>>,
}

impl RecordingSink {
    pub fn new() -> (Arc<RecordingSink>, Receiver<HostsSnapshot>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(RecordingSink { tx: Mutex::new(tx) }), rx)
    }
}

impl SnapshotSink for RecordingSink {
    fn publish(&self, snapshot: &HostsSnapshot) {
        let _ = lock(&self.tx).send(snapshot.clone());
    }
}

/// Receives snapshots until one satisfies `wanted`, or fails the test after [`WAIT`].
pub fn wait_for_snapshot(
    rx: &Receiver<HostsSnapshot>,
    wanted: impl Fn(&HostsSnapshot) -> bool,
) -> HostsSnapshot {
    loop {
        let snapshot = recv(rx);
        if wanted(&snapshot) {
            return snapshot;
        }
    }
}

/// A [`HostIdStore`] that records its calls and can be set to fail.
pub struct MemoryHostIdStore {
    calls: Mutex<Vec<(String, HostId)>>,
    error: Mutex<Option<StoreError>>,
}

impl MemoryHostIdStore {
    pub fn new() -> Self {
        MemoryHostIdStore {
            calls: Mutex::new(Vec::new()),
            error: Mutex::new(None),
        }
    }

    pub fn failing(error: StoreError) -> Self {
        MemoryHostIdStore {
            calls: Mutex::new(Vec::new()),
            error: Mutex::new(Some(error)),
        }
    }

    pub fn calls(&self) -> Vec<(String, HostId)> {
        lock(&self.calls).clone()
    }
}

impl HostIdStore for MemoryHostIdStore {
    fn record_first_contact(&self, name: &str, host_id: &HostId) -> Result<(), StoreError> {
        lock(&self.calls).push((name.to_string(), host_id.clone()));
        match lock(&self.error).clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
