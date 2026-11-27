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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use combine_protocol::{
    daemon_handshake, read_message, write_message, ClientMessage, DaemonMessage, ErrorCode,
    ErrorMessage, HostId, LayoutSnapshot, Panels, RequestId, Welcome, WireMessage,
};

use super::attachments::{StreamDelivery, StreamId, TerminalSink};
use super::config::StoreError;
use super::identity::HostIdStore;
use super::link::{
    ticking_reader, Closer, ConnectError, Connector, ExitInfo, ExitReport, FailureText, Link,
    TimedSocket,
};
use super::model::{BackendView, ConnectionState, HostsSnapshot, SpaceView};
use super::panels::PanelReply;
use super::service::SnapshotSink;
use super::session::LayoutSink;
use super::spawn::{SpawnReply, SpawnSink};
use super::supervisor::{Clock, HostEvents, StopSignal, Wake};

pub const HOST_A: &str = "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c";
pub const HOST_B: &str = "0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c";

/// How long a test waits for something that should already be happening. It is only an upper
/// bound, so a test fails instead of hanging; every wait returns as soon as its state arrives.
/// It is generous because `./tools/verify` runs these tests beside the whole workspace on a busy
/// machine, where a thread can go unscheduled for seconds (#27).
pub const WAIT: Duration = Duration::from_secs(30);

pub fn host_id(s: &str) -> HostId {
    HostId(s.to_string())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A new directory `<parent>/<prefix>-<8 random hex digits>`, mode 0700. As with `mkdtemp`, a
/// name that exists (a killed run's leftover: pids are reused) is retried, never reused.
/// A copy of `combined::testing::fresh_dir`: the app's own code doesn't depend on `combined`.
pub fn fresh_dir(parent: &Path, prefix: &str) -> PathBuf {
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

/// A new scratch directory `/tmp/cs5-<8 random hex digits>`, mode 0700, short enough for a Unix
/// socket path. Removed on drop.
pub struct ShortTempDir {
    path: PathBuf,
}

impl ShortTempDir {
    pub fn new() -> Self {
        ShortTempDir {
            path: fresh_dir(Path::new("/tmp"), "cs5"),
        }
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

/// One encoded `welcome` frame naming `id`.
pub fn welcome_frame(id: &HostId) -> Vec<u8> {
    DaemonMessage::Welcome(Welcome {
        protocol_version: combine_protocol::PROTOCOL_VERSION,
        host_id: id.clone(),
    })
    .encode()
    .expect("encode welcome")
}

/// Writes one encoded `welcome` frame, for a fake ssh to `cat`.
pub fn write_welcome_file(dir: &Path, id: &HostId) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = dir.join(format!("welcome-{n}.bin"));
    fs::write(&path, welcome_frame(id)).expect("write the frame");
    path
}

/// Receives the next value or fails the test after [`WAIT`].
pub fn recv<T>(rx: &Receiver<T>) -> T {
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("nothing arrived within {WAIT:?}"))
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

/// Runs `call` on a helper thread and returns its result, or fails the test naming `site` if it
/// has not returned within [`WAIT`]. A blocking call that never returns then fails in seconds
/// instead of using up the run's timeout (#82). On a timeout the helper is left behind.
pub fn within<T: Send + 'static>(site: &str, call: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(call());
    });
    match finished.recv_timeout(WAIT) {
        Ok(value) => value,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{site} did not return within {WAIT:?}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{site} panicked"),
    }
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
    /// the link, so joining it proves the client did. Both ends read through
    /// [`ticking_reader`], as a local socket link does.
    pub fn pair(self) -> (Link, DropHandle, JoinHandle<()>) {
        let (client, server) = UnixStream::pair().expect("socket pair");
        let drop_handle = DropHandle {
            stream: server.try_clone().expect("clone the server end"),
        };
        let reader = client
            .try_clone()
            .and_then(ticking_reader)
            .expect("clone the client end");
        let shutter = client.try_clone().expect("clone the client end");
        let closer: Closer = Arc::new(move || {
            let _ = shutter.shutdown(Shutdown::Both);
        });
        let link = Link::new(reader, Box::new(client), closer, Box::new(NoExit));
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
    fn serve(self, stream: UnixStream) {
        let ends = stream
            .try_clone()
            .and_then(ticking_reader)
            .and_then(|reader| Ok((reader, stream.try_clone()?)));
        if let Ok((mut reader, mut writer)) = ends {
            if self.converse(&mut *reader, &mut writer) {
                let mut buf = [0u8; 256];
                while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
    }

    /// Plays the daemon's side of the handshake. True when the link should now be held open.
    fn converse(self, reader: &mut (dyn Read + Send), writer: &mut UnixStream) -> bool {
        fn hello(reader: &mut (dyn Read + Send)) -> bool {
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

/// An exit report for a link with no child process.
pub struct NoExit;

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
    /// A link built by the test.
    Link(Link),
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
            Some(Step::Link(link)) => Ok(link),
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

struct LostWakeupState {
    unread: Vec<u8>,
    shut: bool,
    timeout: Option<Duration>,
    blocked: usize,
}

/// A socket whose closer's wake-up is always lost, as macOS sometimes loses it (#82).
///
/// Reads return `unread` first. After that a read blocks. The closer marks the socket shut but
/// never wakes a blocked read, so the read ends only when its read timeout runs out, with
/// `WouldBlock` as `SO_RCVTIMEO` reports it. A read with no timeout never returns. A read that
/// starts after the closer ran returns end of file.
#[derive(Clone)]
pub struct LostWakeupSocket {
    state: Arc<(Mutex<LostWakeupState>, Condvar)>,
}

impl LostWakeupSocket {
    pub fn new(unread: Vec<u8>) -> Self {
        LostWakeupSocket {
            state: Arc::new((
                Mutex::new(LostWakeupState {
                    unread,
                    shut: false,
                    timeout: None,
                    blocked: 0,
                }),
                Condvar::new(),
            )),
        }
    }

    /// Marks the socket shut without waking a blocked read.
    pub fn closer(&self) -> Closer {
        let state = Arc::clone(&self.state);
        Arc::new(move || lock(&state.0).shut = true)
    }

    /// Adds bytes for the next read to return, without waking a blocked read.
    pub fn push(&self, bytes: &[u8]) {
        lock(&self.state.0).unread.extend_from_slice(bytes);
    }

    /// Waits until reads have blocked `count` times in all, or fails the test after [`WAIT`].
    pub fn wait_blocked(&self, count: usize) {
        let deadline = Instant::now() + WAIT;
        while lock(&self.state.0).blocked < count {
            assert!(
                Instant::now() < deadline,
                "reads did not block {count} times within {WAIT:?}"
            );
            thread::yield_now();
        }
    }
}

impl Read for LostWakeupSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (state, never_notified) = &*self.state;
        let mut s = lock(state);
        if !s.unread.is_empty() {
            let n = buf.len().min(s.unread.len());
            buf[..n].copy_from_slice(&s.unread[..n]);
            s.unread.drain(..n);
            return Ok(n);
        }
        if s.shut {
            return Ok(0);
        }
        s.blocked += 1;
        match s.timeout {
            Some(timeout) => {
                drop(
                    never_notified
                        .wait_timeout(s, timeout)
                        .unwrap_or_else(PoisonError::into_inner),
                );
                Err(io::ErrorKind::WouldBlock.into())
            }
            None => loop {
                s = never_notified
                    .wait(s)
                    .unwrap_or_else(PoisonError::into_inner);
            },
        }
    }
}

impl TimedSocket for LostWakeupSocket {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        lock(&self.state.0).timeout = timeout;
        Ok(())
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
    Spaces(HostId, Vec<SpaceView>, BackendView),
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

    fn spaces(&self, host_id: &HostId, spaces: Vec<SpaceView>, backend: BackendView) {
        self.send(Event::Spaces(host_id.clone(), spaces, backend));
    }
}

/// What a [`RecordingLayoutSink`] was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutEvent {
    Layout(HostId, Option<RequestId>, LayoutSnapshot),
    Error(HostId, RequestId, ErrorCode, String),
    Disconnected(HostId),
    Panels(HostId, Panels),
    PanelReply(HostId, PanelReply),
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

    fn panels(&self, host_id: &HostId, panels: Panels) {
        self.send(LayoutEvent::Panels(host_id.clone(), panels));
    }

    fn panel_reply(&self, host_id: &HostId, reply: PanelReply) {
        self.send(LayoutEvent::PanelReply(host_id.clone(), reply));
    }
}

/// What a [`RecordingTerminalSink`] was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// Whether the session is ready, and the backend instance it named.
    Connection(bool, Option<String>),
    Message(DaemonMessage),
}

/// Records every delivery to a panel stream, in order.
#[derive(Default)]
pub struct RecordingTerminalSink {
    seen: Mutex<Vec<(StreamId, Delivered)>>,
}

impl RecordingTerminalSink {
    pub fn new() -> Arc<RecordingTerminalSink> {
        Arc::new(RecordingTerminalSink::default())
    }

    pub fn deliveries(&self) -> Vec<(StreamId, Delivered)> {
        lock(&self.seen).clone()
    }

    /// Waits until at least `count` deliveries have arrived, then returns all of them.
    pub fn wait_for(&self, count: usize) -> Vec<(StreamId, Delivered)> {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let seen = self.deliveries();
            if seen.len() >= count || std::time::Instant::now() > deadline {
                return seen;
            }
            thread::yield_now();
        }
    }
}

impl TerminalSink for RecordingTerminalSink {
    fn deliver(&self, stream: StreamId, event: StreamDelivery<'_>) {
        let delivered = match event {
            StreamDelivery::Connection {
                connected,
                instance,
            } => Delivered::Connection(connected, instance.map(str::to_string)),
            StreamDelivery::Message(message) => Delivered::Message(message.clone()),
        };
        lock(&self.seen).push((stream, delivered));
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

/// Receives snapshots until one satisfies `wanted`, or fails the test after [`WAIT`] in all, so a
/// stream of other snapshots cannot keep it waiting.
pub fn wait_for_snapshot(
    rx: &Receiver<HostsSnapshot>,
    wanted: impl Fn(&HostsSnapshot) -> bool,
) -> HostsSnapshot {
    let deadline = Instant::now() + WAIT;
    loop {
        // recv_timeout(0) still hands over a queued snapshot, so a stream that never runs dry
        // would outlive the deadline without this check.
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            panic!("no wanted snapshot arrived within {WAIT:?}");
        }
        let snapshot = rx
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("no wanted snapshot arrived within {WAIT:?}"));
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

/// Sends every spawn reply to a channel, with the host it came from.
pub struct RecordingSpawnSink {
    tx: Mutex<Sender<(HostId, SpawnReply)>>,
}

impl RecordingSpawnSink {
    pub fn new() -> (Arc<RecordingSpawnSink>, Receiver<(HostId, SpawnReply)>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(RecordingSpawnSink { tx: Mutex::new(tx) }), rx)
    }
}

impl SpawnSink for RecordingSpawnSink {
    fn spawn_reply(&self, host_id: &HostId, reply: SpawnReply) {
        let _ = lock(&self.tx).send((host_id.clone(), reply));
    }
}
