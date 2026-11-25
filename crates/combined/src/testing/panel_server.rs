#![cfg(feature = "test-support")]
//! A daemon server in this process on the fake backend, for the panels and the MCP channel: the
//! in-memory stores, the fake diagram check or the real one, a fake wall clock, a fake caller
//! identity and an audit log. `restart` keeps the data dir and every store.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use combine_protocol::{HostId, TermId};

use super::{check_in_scratch, work_dir};
use super::{
    AuditLog, FakeBackend, FakeCallerIdentity, FakeConfig, FakeControl, FakeDiagramCheck,
    FakeWallClock, MemoryFileStore, MemoryScratchStore, ShortTempDir,
};
use crate::adapters::fs::data_dir::DataDir;
use crate::adapters::loader::DiagramLimits;
use crate::core::{LayoutTiming, PanelTiming};
use crate::model::{SpaceId, SpawnSpec};
use crate::ports::{DiagramCheck, FileStore, TerminalBackend};
use crate::rpc::{self, PanelHandle, ServeOptions, ServerHandle};
use crate::wiring::ids::IdReservation;
use crate::wiring::panels::PanelParts;
use crate::wiring::server_parts_with_panels;

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
    pub options: ServeOptions,
    /// The file store the server uses, when it isn't the memory store itself.
    pub served: Option<Arc<dyn FileStore>>,
    pub diagram: TestDiagramCheck,
}

impl Default for PanelServerConfig {
    fn default() -> Self {
        PanelServerConfig {
            timing: TEST_TIMING,
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
        let (server, socket) = launch(&dir, 1, &backend, &served, &fixtures, &config);
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
        let (server, socket) = launch(
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
    scratch: Arc<MemoryScratchStore>,
    check: Arc<FakeDiagramCheck>,
    wall: Arc<FakeWallClock>,
    callers: Arc<FakeCallerIdentity>,
    audit: Arc<AuditLog>,
}

/// Starts a server on a fresh socket and the data dir `dir` keeps across starts.
fn launch(
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
                TestDiagramCheck::Pele => crate::wiring::diagram::build(DiagramLimits::default()),
            };
            let mut parts = server_parts_with_panels(
                backend,
                id_base,
                HostId("host-a".to_string()),
                ids,
                served,
                LayoutTiming::default(),
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
