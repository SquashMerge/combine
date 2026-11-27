#![cfg(feature = "test-support")]
//! A backend call that never returns stops neither start-up nor a running daemon from shutting
//! down, and a hanging attachment write doesn't stop the daemon from serving other connections.
//! The backend is the fake behind a decorator that holds chosen calls until the test releases
//! them. Every wait is a predicate with a bound, so a regression fails instead of hanging.

mod common;
mod daemon_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use combine_protocol::{
    AttachmentId, BackendGeneration, ClientMessage, DaemonMessage, Display, Fidelity, HostId,
    OwnershipGeneration, RequestId, TermId, WatchTerminals,
};
use combined::adapters::fs::data_dir::DataDir;
use combined::model::{SpawnSpec, TermEvent, TermInfo};
use combined::ports::{
    AttachEvents, AttachOptions, Attachment, BackendResult, Capabilities, TerminalBackend,
};
use combined::rpc::{ServeOptions, ServerHandle, ServerStats};
use combined::testing::{FakeBackend, FakeConfig, FakeControl};
use combined::wiring::ids::IdReservation;
use combined::wiring::{self, Served};
use common::{request_options, spawn_spec};
use daemon_support::{
    server_with_backend, wait_until, work_dir, DaemonPaths, ShortTempDir, WireClient,
};
use futures::executor::block_on;
use futures::stream::BoxStream;

const HOST: &str = "5b0c1f3e-6a3d-4c52-9d0e-2f7a1b8c4d90";
const BOUND: Duration = Duration::from_secs(10);
const STALL: &[u8] = b"<stall>";

/// Holds the calls a test marks until it releases them, and records which calls arrived.
struct Gate {
    released: tokio::sync::watch::Sender<bool>,
    entered: Mutex<Vec<&'static str>>,
    stall_watch: AtomicBool,
    stall_list: AtomicBool,
}

impl Gate {
    fn new() -> Arc<Gate> {
        Arc::new(Gate {
            released: tokio::sync::watch::channel(false).0,
            entered: Mutex::new(Vec::new()),
            stall_watch: AtomicBool::new(false),
            stall_list: AtomicBool::new(false),
        })
    }

    fn release(&self) {
        self.released.send_replace(true);
    }

    async fn pass(&self, call: &'static str) {
        self.entered.lock().unwrap().push(call);
        let _ = self
            .released
            .subscribe()
            .wait_for(|released| *released)
            .await;
    }

    fn entered(&self) -> Vec<&'static str> {
        self.entered.lock().unwrap().clone()
    }
}

/// Releases the gate when it drops, so a failed assertion lets a stalled server finish before
/// the server handle's drop joins its thread. Declare it after the handle.
struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct StallingBackend {
    inner: Arc<FakeBackend>,
    gate: Arc<Gate>,
}

#[async_trait]
impl TerminalBackend for StallingBackend {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn list(&self) -> BackendResult<Vec<TermInfo>> {
        if self.gate.stall_list.load(Ordering::SeqCst) {
            self.gate.pass("list").await;
        }
        self.inner.list().await
    }

    async fn watch(&self) -> BackendResult<BoxStream<'static, TermEvent>> {
        if self.gate.stall_watch.load(Ordering::SeqCst) {
            self.gate.pass("watch").await;
        }
        self.inner.watch().await
    }

    async fn spawn(&self, spec: SpawnSpec) -> BackendResult<TermId> {
        self.inner.spawn(spec).await
    }

    async fn attach(&self, id: &TermId, opts: AttachOptions) -> BackendResult<Box<dyn Attachment>> {
        let inner = self.inner.attach(id, opts).await?;
        Ok(Box::new(StallingAttachment {
            inner,
            gate: Arc::clone(&self.gate),
        }))
    }

    async fn kill(&self, id: &TermId) -> BackendResult<()> {
        self.inner.kill(id).await
    }
}

struct StallingAttachment {
    inner: Box<dyn Attachment>,
    gate: Arc<Gate>,
}

#[async_trait]
impl Attachment for StallingAttachment {
    fn fidelity(&self) -> Fidelity {
        self.inner.fidelity()
    }

    fn ordered_resizes(&self) -> bool {
        self.inner.ordered_resizes()
    }

    fn events(&mut self) -> AttachEvents {
        self.inner.events()
    }

    async fn write(&mut self, ownership: OwnershipGeneration, bytes: &[u8]) -> BackendResult<()> {
        if bytes == STALL {
            self.gate.pass("write").await;
        }
        self.inner.write(ownership, bytes).await
    }

    async fn write_automatic(
        &mut self,
        generation: BackendGeneration,
        bytes: &[u8],
    ) -> BackendResult<()> {
        self.inner.write_automatic(generation, bytes).await
    }

    async fn resize(
        &mut self,
        ownership: OwnershipGeneration,
        cols: u16,
        rows: u16,
    ) -> BackendResult<()> {
        self.inner.resize(ownership, cols, rows).await
    }

    async fn focus(&mut self, ownership: OwnershipGeneration) -> BackendResult<()> {
        self.inner.focus(ownership).await
    }

    async fn set_display(
        &mut self,
        ownership: OwnershipGeneration,
        display: Display,
    ) -> BackendResult<()> {
        self.inner.set_display(ownership, display).await
    }

    async fn credit(&mut self, bytes: u32) -> BackendResult<()> {
        self.inner.credit(bytes).await
    }

    async fn detach(self: Box<Self>) -> BackendResult<()> {
        self.inner.detach().await
    }
}

fn input_frame(attachment: AttachmentId, bytes: &[u8]) -> Vec<u8> {
    let mut payload = attachment.0.to_be_bytes().to_vec();
    payload.extend_from_slice(bytes);
    let mut frame = u32::try_from(payload.len() + 1)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    frame.push(3);
    frame.extend_from_slice(&payload);
    frame
}

/// Attaches and returns the attachment, with everything up to and including `started` read.
fn attach(client: &mut WireClient, term: &TermId) -> AttachmentId {
    client.send(&ClientMessage::Attach(combine_protocol::Attach {
        request: RequestId(1),
        terminal: term.clone(),
        options: request_options(80, 24, false),
        instance: None,
    }));
    let mut attachment = None;
    loop {
        match client.recv() {
            DaemonMessage::Attached(attached) => attachment = Some(attached.attachment),
            DaemonMessage::Started(_) => return attachment.expect("started before attached"),
            _ => {}
        }
    }
}

/// Shuts `server` down on another thread and returns what it reported, or `None` if it took
/// more than the bound.
fn shutdown_within_bound(mut server: ServerHandle) -> Option<Option<combined::rpc::ServeError>> {
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(server.shutdown());
    });
    result.recv_timeout(BOUND).ok()
}

/// Everything a test needs once client A's write is stuck in the backend.
struct StuckWrite {
    server: ServerHandle,
    gate: Arc<Gate>,
    control: FakeControl,
    term: TermId,
    socket: PathBuf,
    /// Held open: the attachment whose write is stuck.
    a: WireClient,
    attachment: AttachmentId,
}

/// Serves one terminal and attaches client A, whose `STALL` input the backend holds. Returns
/// once the write has reached the backend. The caller declares a [`ReleaseOnDrop`] after it
/// takes the server.
fn start_with_stuck_write(scratch: &ShortTempDir) -> StuckWrite {
    let (fake, control) = FakeBackend::new(FakeConfig::default());
    let work = work_dir(scratch);
    fs::create_dir_all(&work).unwrap();
    let term = block_on(TerminalBackend::spawn(
        &*fake,
        spawn_spec(work.to_str().unwrap(), 80, 24),
    ))
    .unwrap();
    let gate = Gate::new();
    let server = server_with_backend(
        scratch,
        "s",
        Arc::new(StallingBackend {
            inner: fake,
            gate: Arc::clone(&gate),
        }),
        HOST,
        ServeOptions::default(),
    );
    let socket = DaemonPaths::in_scratch(scratch, "s").socket;
    let (mut a, _) = WireClient::open(&socket);
    let attachment = attach(&mut a, &term);
    a.send_bytes(&input_frame(attachment, STALL));
    let reached = wait_until(BOUND, || gate.entered() == ["write"]);
    if !reached {
        gate.release();
    }
    assert!(reached, "the write never reached the backend");
    StuckWrite {
        server,
        gate,
        control,
        term,
        socket,
        a,
        attachment,
    }
}

#[test]
fn a_hanging_attachment_write_does_not_stall_other_connections() {
    let scratch = ShortTempDir::new();
    let StuckWrite {
        mut server,
        gate,
        control,
        term,
        socket,
        mut a,
        attachment,
    } = start_with_stuck_write(&scratch);
    let _release = ReleaseOnDrop(Arc::clone(&gate));
    a.send_bytes(&input_frame(attachment, b"after"));

    let (mut b, welcome) = WireClient::open(&socket);
    assert_eq!(welcome.host_id, HostId(HOST.to_string()));
    b.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(7),
    }));
    loop {
        if let DaemonMessage::Terminals(terminals) = b.recv() {
            if terminals.request == Some(RequestId(7)) {
                assert!(terminals.terminals.iter().any(|info| info.id == term));
                break;
            }
        }
    }
    attach(&mut b, &term);

    assert!(
        control.typed_input(&term).is_empty(),
        "a write was applied while the first was held"
    );
    gate.release();
    let expected = [STALL, b"after"].concat();
    assert!(
        wait_until(BOUND, || control.typed_input(&term) == expected),
        "typed input was {:?}",
        control.typed_input(&term)
    );
    assert!(server.shutdown().is_none());
}

#[test]
fn shutdown_completes_while_an_attachment_write_hangs() {
    let scratch = ShortTempDir::new();
    let stuck = start_with_stuck_write(&scratch);
    let _release = ReleaseOnDrop(Arc::clone(&stuck.gate));
    let _a = stuck.a;
    let outcome = shutdown_within_bound(stuck.server);
    assert!(
        matches!(outcome, Some(None)),
        "the server did not stop cleanly: {outcome:?}"
    );
}

fn shutdown_while_stalled(call: &'static str) {
    let scratch = ShortTempDir::new();
    let (fake, _control) = FakeBackend::new(FakeConfig::default());
    let gate = Gate::new();
    match call {
        "watch" => gate.stall_watch.store(true, Ordering::SeqCst),
        _ => gate.stall_list.store(true, Ordering::SeqCst),
    }
    let server = server_with_backend(
        &scratch,
        "s",
        Arc::new(StallingBackend {
            inner: fake,
            gate: Arc::clone(&gate),
        }),
        HOST,
        ServeOptions::default(),
    );
    let _release = ReleaseOnDrop(Arc::clone(&gate));
    assert!(
        wait_until(BOUND, || gate.entered() == [call]),
        "the backend {call} was never called"
    );
    let outcome = shutdown_within_bound(server);
    assert!(
        matches!(outcome, Some(None)),
        "the server did not stop cleanly: {outcome:?}"
    );
}

#[test]
fn shutdown_completes_while_the_backend_watch_hangs() {
    shutdown_while_stalled("watch");
}

#[test]
fn shutdown_completes_while_the_backend_list_hangs() {
    shutdown_while_stalled("list");
}

fn start_up_cancelled_while_stalled(call: &'static str) {
    let scratch = ShortTempDir::new();
    let paths = DaemonPaths::in_scratch(&scratch, "d");
    let (fake, _control) = FakeBackend::new(FakeConfig::default());
    let gate = Gate::new();
    match call {
        "watch" => gate.stall_watch.store(true, Ordering::SeqCst),
        _ => gate.stall_list.store(true, Ordering::SeqCst),
    }
    let data_dir = DataDir::open(&paths.data_dir).unwrap();
    let (ids, id_base) = IdReservation::start(&data_dir.ids_json()).unwrap();
    let parts = wiring::server_parts(
        Arc::new(StallingBackend {
            inner: fake,
            gate: Arc::clone(&gate),
        }),
        id_base,
        HostId("host-a".into()),
        ids,
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let watcher = {
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            let reached = wait_until(BOUND, || gate.entered() == [call]);
            let _ = stop_tx.send(());
            reached
        })
    };
    let outcome = runtime.block_on(async {
        let shutdown = pin!(async move {
            let _ = stop_rx.await;
        });
        tokio::time::timeout(
            BOUND,
            wiring::serve_after_build(parts, &paths.socket, shutdown, Arc::new(ServerStats::new())),
        )
        .await
    });
    assert!(
        watcher.join().unwrap(),
        "the backend {call} was never called"
    );
    assert!(
        matches!(outcome, Ok(Ok(Served::Cancelled))),
        "start-up was not cancelled: {outcome:?}"
    );
    assert!(!paths.socket.exists());
    let mut lock = paths.socket.clone().into_os_string();
    lock.push(".lock");
    assert!(!Path::new(&lock).exists());
    drop(data_dir);
}

#[test]
fn start_up_is_cancelled_while_the_backend_watch_hangs() {
    start_up_cancelled_while_stalled("watch");
}

#[test]
fn start_up_is_cancelled_while_the_backend_list_hangs() {
    start_up_cancelled_while_stalled("list");
}
