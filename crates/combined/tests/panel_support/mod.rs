#![allow(dead_code)]
//! Test support for the panels in the daemon: a server on the fake backend with in-memory
//! stores and a fake diagram check, a file store that lets an outside write land at a chosen
//! moment, and polls that never block the runtime driving the server.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use combine_protocol::{
    AttachmentId, ClientMessage, Cursor, DaemonMessage, HostId, RequestId, TermId,
};
use combine_protocol::{PanelState, Renderer};
use combined::adapters::fs::data_dir::DataDir;
use combined::core::{LayoutTiming, PanelApiError, PanelCreate, PanelTiming, PanelView};
use combined::model::{Lifetime, SpaceId, SpawnSpec};
use combined::ports::{
    DirListing, FileChange, FileStore, ScratchStore, StoreError, StoreRevision, StoredFile,
    TerminalBackend, WriteOutcome,
};
use combined::rpc::{self, PanelHandle, ServeOptions, ServerHandle};
use combined::testing::{
    FakeBackend, FakeConfig, FakeControl, FakeDiagramCheck, FakeWallClock, MemoryFileStore,
    MemoryScratchStore,
};
use combined::wiring::ids::IdReservation;
use combined::wiring::panels::PanelParts;
use combined::wiring::server_parts_with_panels;
use futures::stream::BoxStream;

use crate::common::request_options;
use crate::daemon_support::{
    check_in_scratch, wait_until, work_dir, DaemonPaths, ShortTempDir, WireClient,
};

/// The wall time every rig starts at: far from the epoch, so a stamp is never 0.
pub const START_WALL_MS: u64 = 1_700_000_000_000;

/// Short timers, so a test waits for tens of milliseconds, not seconds.
pub const TEST_TIMING: PanelTiming = PanelTiming {
    debounce_ms: 20,
    max_wait_ms: 80,
    retry_ms: 50,
    check_retry_ms: [10, 20, 40],
    sweep_every_ms: 50,
};

/// How long a test waits for something the server does by itself.
pub const WAIT: Duration = Duration::from_secs(5);

/// When a [`RacingStore`] hook fires relative to the read it hangs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Race {
    /// The outside write lands first, so the read sees it.
    BeforeCompare,
    /// The read sees the old file and the outside write lands right after it.
    AfterCompare,
}

struct Hook {
    space: SpaceId,
    path: String,
    when: Race,
    bytes: Vec<u8>,
}

/// A [`MemoryFileStore`] with a one-shot hook: on the next read of a path it writes outside, as
/// someone else would, either just before or just after the read.
pub struct RacingStore {
    inner: Arc<MemoryFileStore>,
    hook: Mutex<Option<Hook>>,
}

impl RacingStore {
    pub fn new(inner: Arc<MemoryFileStore>) -> RacingStore {
        RacingStore {
            inner,
            hook: Mutex::new(None),
        }
    }

    /// The next read of `path` in `space` writes `bytes` outside, `when` told. The hook then
    /// goes.
    pub fn arm(&self, space: &SpaceId, path: &str, when: Race, bytes: Vec<u8>) {
        *self.hook.lock().unwrap_or_else(PoisonError::into_inner) = Some(Hook {
            space: space.clone(),
            path: path.to_string(),
            when,
            bytes,
        });
    }

    /// Whether the hook is still waiting for its read.
    pub fn armed(&self) -> bool {
        self.hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn take_hook(&self, space: &SpaceId, path: &str) -> Option<Hook> {
        let mut hook = self.hook.lock().unwrap_or_else(PoisonError::into_inner);
        if hook
            .as_ref()
            .is_some_and(|hook| &hook.space == space && hook.path == path)
        {
            hook.take()
        } else {
            None
        }
    }
}

#[async_trait]
impl FileStore for RacingStore {
    async fn read(
        &self,
        space: &SpaceId,
        path: &str,
        max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError> {
        let hook = self.take_hook(space, path);
        if let Some(hook) = hook
            .as_ref()
            .filter(|hook| hook.when == Race::BeforeCompare)
        {
            self.inner.write_outside(space, path, hook.bytes.clone());
        }
        let read = self.inner.read(space, path, max_bytes).await;
        if let Some(hook) = hook.filter(|hook| hook.when == Race::AfterCompare) {
            self.inner.write_outside(space, path, hook.bytes);
        }
        read
    }

    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        self.inner.write(space, path, bytes).await
    }

    async fn write_project(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        self.inner.write_project(space, path, bytes).await
    }

    async fn remove_file(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        self.inner.remove_file(space, path).await
    }

    async fn remove_dir(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        self.inner.remove_dir(space, path).await
    }

    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError> {
        self.inner.revision(space).await
    }

    async fn list_dir(
        &self,
        space: &SpaceId,
        path: &str,
        max_entries: usize,
    ) -> Result<Option<DirListing>, StoreError> {
        self.inner.list_dir(space, path, max_entries).await
    }

    async fn watch(&self, space: &SpaceId) -> Result<(), StoreError> {
        self.inner.watch(space).await
    }

    fn changes(&self) -> BoxStream<'static, FileChange> {
        self.inner.changes()
    }
}

/// A server on the fake backend with in-memory stores, the fake diagram check and a fake wall
/// clock, called through its [`PanelHandle`]. `restart` keeps the data dir and every store.
pub struct PanelRig {
    pub server: ServerHandle,
    pub panels: PanelHandle,
    pub backend: Arc<FakeBackend>,
    pub control: FakeControl,
    pub store: Arc<MemoryFileStore>,
    pub scratch: Arc<MemoryScratchStore>,
    pub check: Arc<FakeDiagramCheck>,
    pub wall: Arc<FakeWallClock>,
    /// The project folder the panels are made in. It is a space once a terminal runs there.
    pub space: SpaceId,
    pub socket: PathBuf,
    pub dir: ShortTempDir,
    served: Arc<dyn FileStore>,
    timing: PanelTiming,
    starts: u32,
}

impl PanelRig {
    pub fn new() -> PanelRig {
        PanelRig::with_timing(TEST_TIMING)
    }

    pub fn with_timing(timing: PanelTiming) -> PanelRig {
        let store = Arc::new(MemoryFileStore::new());
        PanelRig::start_on(timing, Arc::clone(&store), store)
    }

    /// A rig whose file store is a [`RacingStore`] over the rig's memory store.
    pub fn racing() -> (PanelRig, Arc<RacingStore>) {
        let store = Arc::new(MemoryFileStore::new());
        let racing = Arc::new(RacingStore::new(Arc::clone(&store)));
        let served: Arc<dyn FileStore> = racing.clone();
        (PanelRig::start_on(TEST_TIMING, store, served), racing)
    }

    fn start_on(
        timing: PanelTiming,
        store: Arc<MemoryFileStore>,
        served: Arc<dyn FileStore>,
    ) -> PanelRig {
        let dir = ShortTempDir::new();
        let (backend, control) = FakeBackend::new(FakeConfig::default());
        let work = work_dir(&dir);
        fs::create_dir_all(work.join(".git")).unwrap();
        let space = SpaceId(work.to_str().unwrap().to_string());
        let scratch = Arc::new(MemoryScratchStore::new());
        let check = Arc::new(FakeDiagramCheck::new());
        let wall = Arc::new(FakeWallClock::new(START_WALL_MS));
        let (server, socket) = launch(
            &dir,
            1,
            &backend,
            &served,
            scratch.clone(),
            &check,
            &wall,
            timing,
        );
        PanelRig {
            panels: server.panels(),
            server,
            backend,
            control,
            store,
            scratch,
            check,
            wall,
            space,
            socket,
            dir,
            served,
            timing,
            starts: 1,
        }
    }

    /// Stops the server and starts another on the same data dir and stores, as a daemon restart.
    pub fn restart(&mut self) {
        assert!(self.server.shutdown().is_none());
        self.starts += 1;
        let (server, socket) = launch(
            &self.dir,
            self.starts,
            &self.backend,
            &self.served,
            self.scratch.clone(),
            &self.check,
            &self.wall,
            self.timing,
        );
        self.panels = server.panels();
        self.server = server;
        self.socket = socket;
    }

    /// A client of the running server.
    pub fn client(&self) -> WireClient {
        WireClient::open(&self.socket).0
    }

    /// Runs a terminal in the project folder, which makes the project a space.
    pub fn spawn_terminal(&self) -> TermId {
        let spec = SpawnSpec {
            argv: vec!["sh".to_string()],
            cwd: self.space.0.clone(),
            env: std::collections::BTreeMap::new(),
            labels: std::collections::BTreeMap::new(),
            cols: 80,
            rows: 24,
        };
        futures::executor::block_on(TerminalBackend::spawn(&*self.backend, spec)).unwrap()
    }

    pub fn create(&self, create: PanelCreate) -> Result<PanelView, PanelApiError> {
        self.panels.create(&self.space, create)
    }

    /// The panel as `get` shows it, with its served source.
    pub fn get(&self, id: &str) -> Result<(PanelView, String), PanelApiError> {
        self.panels.get(&self.space, id)
    }

    /// Waits for the panel to be in `state` and returns it with its source.
    pub fn wait_state(&self, id: &str, state: PanelState) -> (PanelView, String) {
        let mut last = None;
        let reached = wait_until(WAIT, || {
            last = Some(self.get(id));
            matches!(&last, Some(Ok((view, _))) if view.state == state)
        });
        assert!(reached, "{id} never reached {state:?}; it is {last:?}");
        last.unwrap().unwrap()
    }

    /// The IDs of the space's panels, sorted.
    pub fn list_ids(&self) -> Vec<String> {
        let list = self.panels.list(&self.space).unwrap();
        let mut ids: Vec<String> = list.panels.into_iter().map(|view| view.id).collect();
        ids.sort();
        ids
    }

    pub fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.store.bytes(&self.space, path)
    }

    pub fn write_count(&self, path: &str) -> u64 {
        self.store.write_count(&self.space, path)
    }

    pub fn remove_count(&self, path: &str) -> u64 {
        self.store.remove_count(&self.space, path)
    }

    pub fn connections(&self) -> usize {
        self.server.stats().connections()
    }
}

/// Starts a server on a fresh socket and the data dir `dir` keeps across starts.
#[allow(clippy::too_many_arguments)]
pub fn launch(
    dir: &ShortTempDir,
    start: u32,
    backend: &Arc<FakeBackend>,
    served: &Arc<dyn FileStore>,
    scratch: Arc<dyn ScratchStore>,
    check: &Arc<FakeDiagramCheck>,
    wall: &Arc<FakeWallClock>,
    timing: PanelTiming,
) -> (ServerHandle, PathBuf) {
    let paths = DaemonPaths::in_scratch(dir, "p");
    let socket_path = dir.sock(&format!("p-{start}.sock"));
    check_in_scratch(&paths.data_dir, dir);
    check_in_scratch(&socket_path, dir);
    let data_dir = DataDir::open(&paths.data_dir).unwrap();
    let (ids, id_base) = IdReservation::start(&data_dir.ids_json()).unwrap();
    let socket = rpc::socket::bind(&socket_path).unwrap();
    let backend: Arc<dyn TerminalBackend> = backend.clone();
    let served = Arc::clone(served);
    let parts = PanelParts {
        diagram_check: check.clone(),
        wall_clock: wall.clone(),
        scratch,
        timing,
    };
    let server = rpc::spawn(
        socket,
        move || {
            // Each test serves its own data dir, so no other writer needs the lock.
            drop(data_dir);
            server_parts_with_panels(
                backend,
                id_base,
                HostId("host-a".to_string()),
                ids,
                served,
                LayoutTiming::default(),
                parts,
            )
            .unwrap()
        },
        ServeOptions::default(),
    )
    .unwrap();
    (server, socket_path)
}

/// A persistent Markdown panel with a daemon-made ID.
pub fn markdown(content: &str) -> PanelCreate {
    PanelCreate {
        id: None,
        lifetime: Lifetime::Persistent,
        renderer: Renderer::Markdown,
        title: None,
        idempotency_key: None,
        created_by: None,
        content: content.to_string(),
    }
}

/// A persistent Mermaid panel with a daemon-made ID.
pub fn mermaid(content: &str) -> PanelCreate {
    PanelCreate {
        renderer: Renderer::Mermaid,
        ..markdown(content)
    }
}

pub fn named(id: &str, create: PanelCreate) -> PanelCreate {
    PanelCreate {
        id: Some(id.to_string()),
        ..create
    }
}

pub fn scratch(create: PanelCreate) -> PanelCreate {
    PanelCreate {
        lifetime: Lifetime::Scratch,
        ..create
    }
}

/// The read count of `path` once it is at least `min` and has not changed for
/// 4 x TEST_TIMING.max_wait_ms (320 ms). Polls every 10 ms, as `wait_until` does, so it never
/// blocks the runtime driving the server under test; panics after 5 s, naming the path.
pub fn settled_read_count(store: &MemoryFileStore, space: &SpaceId, path: &str, min: u64) -> u64 {
    let quiet = Duration::from_millis(4 * TEST_TIMING.max_wait_ms);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = store.read_count(space, path);
    let mut since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let now = store.read_count(space, path);
        if now != last {
            last = now;
            since = Instant::now();
        } else if now >= min && since.elapsed() >= quiet {
            return now;
        }
        assert!(
            Instant::now() < deadline,
            "the read count of {path} never settled at {min} or more; it is {now}"
        );
    }
}

/// Attaches `client` to `term` and returns the attachment, with everything up to and including
/// `started` read.
pub fn attach(client: &mut WireClient, term: &TermId) -> AttachmentId {
    client.send(&ClientMessage::Attach(combine_protocol::Attach {
        request: RequestId(1),
        terminal: term.clone(),
        options: request_options(80, 24, false),
    }));
    let mut attachment = None;
    loop {
        let message = client.recv();
        if let DaemonMessage::Attached(attached) = &message {
            attachment = Some(attached.attachment);
        }
        if matches!(message, DaemonMessage::Started(_)) {
            return attachment.expect("started before attached");
        }
    }
}

/// Reads until the bytes of `attachment`'s data end with `tail`, acking each event, or until
/// `within` passes: then `None`.
pub fn read_until_tail(
    client: &mut WireClient,
    attachment: AttachmentId,
    tail: &[u8],
    within: Duration,
) -> Option<Vec<u8>> {
    let deadline = Instant::now() + within;
    let mut bytes = Vec::new();
    while !bytes.ends_with(tail) {
        let left = deadline.saturating_duration_since(Instant::now());
        let message = client.recv_within(left)?;
        if let Some(cursor) = message
            .cursor()
            .filter(|c: &Cursor| c.attachment == attachment)
        {
            if let DaemonMessage::Data(data) = &message {
                bytes.extend_from_slice(&data.bytes);
            }
            client.ack(cursor);
        }
    }
    Some(bytes)
}
