#![allow(dead_code)]
//! Test support for the panels in the daemon: a server on the fake backend with in-memory
//! stores and a fake diagram check, a file store that lets an outside write land at a chosen
//! moment, and polls that never block the runtime driving the server.

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use combine_protocol::HostId;
use combine_protocol::{PanelState, Renderer};
use combined::adapters::fs::data_dir::DataDir;
use combined::core::{LayoutTiming, PanelApiError, PanelCreate, PanelTiming, PanelView};
use combined::model::{Lifetime, SpaceId};
use combined::ports::{
    DirListing, FileChange, FileStore, ScratchStore, StoreError, StoreRevision, StoredFile,
    TerminalBackend, WriteOutcome,
};
use combined::rpc::{self, ServeOptions, ServerHandle};
use combined::testing::{
    FakeBackend, FakeDiagramCheck, FakeWallClock, MemoryFileStore, PanelServer, PanelServerConfig,
};
use combined::wiring::ids::IdReservation;
use combined::wiring::panels::PanelParts;
use combined::wiring::server_parts_with_panels;
use futures::stream::BoxStream;

#[allow(unused_imports)]
pub use combined::testing::{attach, read_until_tail, START_WALL_MS, TEST_TIMING};

use crate::daemon_support::{check_in_scratch, wait_until, DaemonPaths, ShortTempDir, WireClient};

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
