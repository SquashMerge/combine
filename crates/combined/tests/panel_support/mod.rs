#![allow(dead_code)]
//! Test support for the panels in the daemon: a server on the fake backend with in-memory
//! stores and a fake diagram check, a file store that lets an outside write land at a chosen
//! moment, and polls that never block the runtime driving the server.

use std::collections::BTreeMap;
use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, HostId, Layout, Panels, RequestId, TermId, WatchLayout,
};
use combine_protocol::{PanelState, Renderer};
use combined::adapters::fs::data_dir::DataDir;
use combined::adapters::loader::DiagramLimits;
use combined::core::{LayoutTiming, PanelApiError, PanelCreate, PanelTiming, PanelView};
use combined::mcp_server::McpAudit;
use combined::model::{Lifetime, SpaceId, SpawnSpec};
use combined::ports::{
    DiagramCheck, DirListing, FileChange, FileStore, ScratchStore, StoreError, StoreRevision,
    StoredFile, TerminalBackend, WriteOutcome,
};
use combined::rpc::{self, PanelHandle, ServeOptions, ServerHandle};
use combined::testing::{
    FakeBackend, FakeCallerIdentity, FakeConfig, FakeControl, FakeDiagramCheck, FakeWallClock,
    MemoryFileStore, MemoryScratchStore,
};
use combined::wiring::ids::IdReservation;
use combined::wiring::panels::PanelParts;
use combined::wiring::server_parts_with_panels;
use futures::stream::BoxStream;

#[allow(unused_imports)]
pub use combined::testing::{attach, read_until_tail};

use crate::daemon_support::{check_in_scratch, wait_until, work_dir, ShortTempDir, WireClient};

/// The wall time every server starts at: far from the epoch, so a stamp is never 0.
pub const START_WALL_MS: u64 = 1_700_000_000_000;

/// Short timers, so a test waits for tens of milliseconds, not seconds.
pub const TEST_TIMING: PanelTiming = PanelTiming {
    debounce_ms: 20,
    max_wait_ms: 80,
    retry_ms: 50,
    check_retry_ms: [10, 20, 40],
    sweep_every_ms: 50,
};

/// Which diagram check the server runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestDiagramCheck {
    /// The test's own [`FakeDiagramCheck`].
    Fake,
    /// The daemon's real checker, built as `combined serve` builds it.
    Pele,
}

/// How a [`PanelServer`] is built.
pub struct PanelServerConfig {
    pub timing: PanelTiming,
    /// How the server debounces its layout writes.
    pub layout_timing: LayoutTiming,
    pub options: ServeOptions,
    /// The file store the server uses, when it isn't the memory store itself.
    pub served: Option<Arc<dyn FileStore>>,
    pub diagram: TestDiagramCheck,
}

impl Default for PanelServerConfig {
    fn default() -> Self {
        PanelServerConfig {
            timing: TEST_TIMING,
            layout_timing: LayoutTiming::default(),
            options: ServeOptions::default(),
            served: None,
            diagram: TestDiagramCheck::Fake,
        }
    }
}

/// A server on the fake backend with in-memory stores, called through its [`PanelHandle`].
pub struct PanelServer {
    pub server: ServerHandle,
    pub panels: PanelHandle,
    pub backend: Arc<FakeBackend>,
    pub control: FakeControl,
    pub store: Arc<MemoryFileStore>,
    pub scratch: Arc<MemoryScratchStore>,
    pub check: Arc<FakeDiagramCheck>,
    pub wall: Arc<FakeWallClock>,
    pub callers: Arc<FakeCallerIdentity>,
    pub audit: Arc<AuditLog>,
    /// The project folder the panels are made in. It is a space once a terminal runs there.
    pub space: SpaceId,
    pub socket: PathBuf,
    pub dir: ShortTempDir,
    served: Arc<dyn FileStore>,
    config: PanelServerConfig,
    starts: u32,
}

impl PanelServer {
    pub fn start(config: PanelServerConfig) -> PanelServer {
        let store = Arc::new(MemoryFileStore::new());
        let served: Arc<dyn FileStore> = match &config.served {
            Some(served) => Arc::clone(served),
            None => store.clone(),
        };
        let dir = ShortTempDir::new();
        let (backend, control) = FakeBackend::new(FakeConfig::default());
        let work = work_dir(&dir);
        fs::create_dir_all(work.join(".git")).unwrap();
        let space = SpaceId(work.to_str().unwrap().to_string());
        let scratch = Arc::new(MemoryScratchStore::new());
        let check = Arc::new(FakeDiagramCheck::new());
        let wall = Arc::new(FakeWallClock::new(START_WALL_MS));
        let callers = Arc::new(FakeCallerIdentity::new(None));
        let audit = Arc::new(AuditLog::new());
        let fixtures = Fixtures {
            scratch: scratch.clone(),
            check: check.clone(),
            wall: wall.clone(),
            callers: callers.clone(),
            audit: audit.clone(),
        };
        let (server, socket) = launch_server(&dir, 1, &backend, &served, &fixtures, &config);
        PanelServer {
            panels: server.panels(),
            server,
            backend,
            control,
            store,
            scratch,
            check,
            wall,
            callers,
            audit,
            space,
            socket,
            dir,
            served,
            config,
            starts: 1,
        }
    }

    /// Stops the server and starts another on the same data dir and stores, as a daemon restart.
    pub fn restart(&mut self) {
        assert!(self.server.shutdown().is_none());
        self.starts += 1;
        let fixtures = Fixtures {
            scratch: self.scratch.clone(),
            check: self.check.clone(),
            wall: self.wall.clone(),
            callers: self.callers.clone(),
            audit: self.audit.clone(),
        };
        let (server, socket) = launch_server(
            &self.dir,
            self.starts,
            &self.backend,
            &self.served,
            &fixtures,
            &self.config,
        );
        self.panels = server.panels();
        self.server = server;
        self.socket = socket;
    }

    /// Runs a terminal in the project folder, which makes the project a space.
    pub fn spawn_terminal(&self) -> TermId {
        self.spawn_terminal_in(Path::new(&self.space.0))
    }

    /// Runs a terminal in `cwd`.
    pub fn spawn_terminal_in(&self, cwd: &Path) -> TermId {
        let spec = SpawnSpec {
            argv: vec!["sh".to_string()],
            cwd: cwd.to_str().unwrap().to_string(),
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
            cols: 80,
            rows: 24,
        };
        futures::executor::block_on(TerminalBackend::spawn(&*self.backend, spec)).unwrap()
    }
}

/// The fakes a server is wired to, which a restart keeps.
struct Fixtures {
    scratch: Arc<dyn ScratchStore>,
    check: Arc<FakeDiagramCheck>,
    wall: Arc<FakeWallClock>,
    callers: Arc<FakeCallerIdentity>,
    audit: Arc<AuditLog>,
}

/// Starts a server on a fresh socket and the data dir `dir` keeps across starts.
fn launch_server(
    dir: &ShortTempDir,
    start: u32,
    backend: &Arc<FakeBackend>,
    served: &Arc<dyn FileStore>,
    fixtures: &Fixtures,
    config: &PanelServerConfig,
) -> (ServerHandle, PathBuf) {
    let data_path = dir.path().join("p").join("data");
    let socket_path = dir.sock(&format!("p-{start}.sock"));
    check_in_scratch(&data_path, dir);
    check_in_scratch(&socket_path, dir);
    let data_dir = DataDir::open(&data_path).unwrap();
    let (ids, id_base) = IdReservation::start(&data_dir.ids_json()).unwrap();
    let socket = rpc::socket::bind(&socket_path).unwrap();
    let backend: Arc<dyn TerminalBackend> = backend.clone();
    let served = Arc::clone(served);
    let fake_check: Arc<dyn DiagramCheck> = fixtures.check.clone();
    let diagram = config.diagram;
    let wall = fixtures.wall.clone();
    let scratch = fixtures.scratch.clone();
    let timing = config.timing;
    let layout_timing = config.layout_timing;
    let callers = fixtures.callers.clone();
    let audit = fixtures.audit.clone();
    let server = rpc::spawn(
        socket,
        move || {
            // Each test serves its own data dir, so no other writer needs the lock.
            drop(data_dir);
            // Built here, inside the server's runtime, which Pele's warm-up needs.
            let diagram_check = match diagram {
                TestDiagramCheck::Fake => fake_check,
                TestDiagramCheck::Pele => {
                    combined::wiring::diagram::build(DiagramLimits::default())
                }
            };
            let mut parts = server_parts_with_panels(
                backend,
                id_base,
                HostId("host-a".to_string()),
                ids,
                served,
                layout_timing,
                PanelParts {
                    diagram_check,
                    wall_clock: wall,
                    scratch,
                    timing,
                },
            )
            .unwrap();
            parts.caller_identity = Some(callers);
            parts.mcp_audit = audit;
            parts
        },
        config.options.clone(),
    )
    .unwrap();
    (server, socket_path)
}

/// Collects the audit lines of the MCP calls.
#[derive(Default)]
pub struct AuditLog {
    lines: Mutex<Vec<String>>,
}

impl AuditLog {
    pub fn new() -> AuditLog {
        AuditLog::default()
    }

    /// Every line recorded so far, in order.
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl McpAudit for AuditLog {
    fn record(&self, line: &str) {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.to_string());
    }
}

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

/// A file store call a [`StallingStore`] saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOp {
    Read(String),
    List(String),
    Write(String, Vec<u8>),
    Remove(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StallKind {
    Read,
    List,
}

struct StallSlot {
    kind: StallKind,
    space: SpaceId,
    path: String,
    release: mpsc::Receiver<()>,
}

/// Frees the call a [`StallingStore`] is holding when it is released or dropped.
pub struct Stall {
    release: Option<mpsc::Sender<()>>,
}

impl Stall {
    pub fn release(mut self) {
        self.release.take();
    }
}

/// A [`MemoryFileStore`] whose next read or listing of a chosen path blocks its thread, with a
/// record of every call it saw.
pub struct StallingStore {
    inner: Arc<MemoryFileStore>,
    slots: Mutex<Vec<StallSlot>>,
    entered: AtomicBool,
    log: Mutex<Vec<StoreOp>>,
}

impl StallingStore {
    pub fn new(inner: Arc<MemoryFileStore>) -> StallingStore {
        StallingStore {
            inner,
            slots: Mutex::new(Vec::new()),
            entered: AtomicBool::new(false),
            log: Mutex::new(Vec::new()),
        }
    }

    /// The next `read` of `path` in `space` blocks in `recv`, not awaiting, until the returned
    /// guard is released or dropped.
    pub fn stall(&self, space: &SpaceId, path: &str) -> Stall {
        self.arm_stall(StallKind::Read, space, path)
    }

    /// As [`StallingStore::stall`], for the next `list_dir`.
    pub fn stall_list(&self, space: &SpaceId, path: &str) -> Stall {
        self.arm_stall(StallKind::List, space, path)
    }

    fn arm_stall(&self, kind: StallKind, space: &SpaceId, path: &str) -> Stall {
        let (release, wait) = mpsc::channel();
        self.entered.store(false, Ordering::SeqCst);
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(StallSlot {
                kind,
                space: space.clone(),
                path: path.to_string(),
                release: wait,
            });
        Stall {
            release: Some(release),
        }
    }

    /// Whether a stalled call has started.
    pub fn entered(&self) -> bool {
        self.entered.load(Ordering::SeqCst)
    }

    /// Every read, listing, write and removal so far, in the order the calls started.
    pub fn log(&self) -> Vec<StoreOp> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record(&self, op: StoreOp) {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(op);
    }

    fn block_if_stalled(&self, kind: StallKind, space: &SpaceId, path: &str) {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            let at = slots
                .iter()
                .position(|slot| slot.kind == kind && &slot.space == space && slot.path == path);
            at.map(|at| slots.remove(at))
        };
        if let Some(slot) = slot {
            self.entered.store(true, Ordering::SeqCst);
            let _ = slot.release.recv();
        }
    }
}

#[async_trait]
impl FileStore for StallingStore {
    async fn read(
        &self,
        space: &SpaceId,
        path: &str,
        max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError> {
        self.record(StoreOp::Read(path.to_string()));
        self.block_if_stalled(StallKind::Read, space, path);
        self.inner.read(space, path, max_bytes).await
    }

    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        self.record(StoreOp::Write(path.to_string(), bytes.clone()));
        self.inner.write(space, path, bytes).await
    }

    async fn write_project(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        self.record(StoreOp::Write(path.to_string(), bytes.clone()));
        self.inner.write_project(space, path, bytes).await
    }

    async fn remove_file(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        self.record(StoreOp::Remove(path.to_string()));
        self.inner.remove_file(space, path).await
    }

    async fn remove_dir(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        self.record(StoreOp::Remove(path.to_string()));
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
        self.record(StoreOp::List(path.to_string()));
        self.block_if_stalled(StallKind::List, space, path);
        self.inner.list_dir(space, path, max_entries).await
    }

    async fn watch(&self, space: &SpaceId) -> Result<(), StoreError> {
        self.inner.watch(space).await
    }

    fn changes(&self) -> BoxStream<'static, FileChange> {
        self.inner.changes()
    }
}

/// A [`PanelServer`] with the panel API's helpers: the project folder's panels are made, read
/// and counted through it. `restart` keeps the data dir and every store.
pub struct PanelRig {
    inner: PanelServer,
}

impl Deref for PanelRig {
    type Target = PanelServer;

    fn deref(&self) -> &PanelServer {
        &self.inner
    }
}

impl DerefMut for PanelRig {
    fn deref_mut(&mut self) -> &mut PanelServer {
        &mut self.inner
    }
}

impl PanelRig {
    pub fn new() -> PanelRig {
        PanelRig::with_timing(TEST_TIMING)
    }

    pub fn with_timing(timing: PanelTiming) -> PanelRig {
        PanelRig {
            inner: PanelServer::start(PanelServerConfig {
                timing,
                ..PanelServerConfig::default()
            }),
        }
    }

    /// A rig whose file store is a [`RacingStore`] over the rig's memory store.
    pub fn racing() -> (PanelRig, Arc<RacingStore>) {
        PanelRig::racing_with(ServeOptions::default())
    }

    /// A rig whose file store is a [`StallingStore`] over the rig's memory store.
    pub fn stalling() -> (PanelRig, Arc<StallingStore>) {
        let store = Arc::new(MemoryFileStore::new());
        let stalling = Arc::new(StallingStore::new(Arc::clone(&store)));
        let served: Arc<dyn FileStore> = stalling.clone();
        let mut inner = PanelServer::start(PanelServerConfig {
            served: Some(served),
            ..PanelServerConfig::default()
        });
        inner.store = store;
        (PanelRig { inner }, stalling)
    }

    /// A rig built from `config`.
    pub fn with_config(config: PanelServerConfig) -> PanelRig {
        PanelRig {
            inner: PanelServer::start(config),
        }
    }

    /// A rig serving with `options`.
    pub fn with_options(options: ServeOptions) -> PanelRig {
        PanelRig {
            inner: PanelServer::start(PanelServerConfig {
                options,
                ..PanelServerConfig::default()
            }),
        }
    }

    /// [`PanelRig::racing`], serving with `options`.
    pub fn racing_with(options: ServeOptions) -> (PanelRig, Arc<RacingStore>) {
        // The server's own memory store goes unused when a served store is given, so the field
        // is pointed at the one the racing store wraps.
        let store = Arc::new(MemoryFileStore::new());
        let racing = Arc::new(RacingStore::new(Arc::clone(&store)));
        let served: Arc<dyn FileStore> = racing.clone();
        let mut inner = PanelServer::start(PanelServerConfig {
            served: Some(served),
            options,
            ..PanelServerConfig::default()
        });
        inner.store = store;
        (PanelRig { inner }, racing)
    }

    /// A client of the running server.
    pub fn client(&self) -> WireClient {
        WireClient::open(&self.socket).0
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

/// Starts a server on a fresh socket and the data dir `dir` keeps across starts, with the
/// default serve options and its own caller identity and audit log.
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
    let fixtures = Fixtures {
        scratch,
        check: check.clone(),
        wall: wall.clone(),
        callers: Arc::new(FakeCallerIdentity::new(None)),
        audit: Arc::new(AuditLog::new()),
    };
    let config = PanelServerConfig {
        timing,
        ..PanelServerConfig::default()
    };
    launch_server(dir, start, backend, served, &fixtures, &config)
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
        placement: None,
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

/// Sends `watch_layout` for `space` and returns the `layout` that answers it and the `panels`
/// that follows it at once. Anything between the two fails the test.
pub fn watch_panels(client: &mut WireClient, space: &SpaceId, request: u64) -> (Layout, Panels) {
    let deadline = Instant::now() + WAIT;
    let layout = loop {
        client.send(&ClientMessage::WatchLayout(WatchLayout {
            request: RequestId(request),
            space: space.0.clone(),
        }));
        match client.recv() {
            DaemonMessage::Layout(layout) => break layout,
            // The terminal that made the project a space may not have been seen yet.
            DaemonMessage::Error(error)
                if error.code == ErrorCode::UnknownSpace && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => panic!("expected the layout, got {other:?}"),
        }
    };
    assert_eq!(layout.request, Some(RequestId(request)));
    let panels = match client.recv() {
        DaemonMessage::Panels(panels) => panels,
        other => panic!("expected the panels right after the layout, got {other:?}"),
    };
    assert_eq!(panels.request, Some(RequestId(request)));
    (layout, panels)
}

/// The next `panels` message, skipping any other. `None` when none arrives within `within`.
pub fn next_panels(client: &WireClient, within: Duration) -> Option<Panels> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let DaemonMessage::Panels(panels) = client.recv_within(left)? {
            return Some(panels);
        }
    }
}
