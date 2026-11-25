//! An in-process daemon on the fake backend with in-memory stores, for the shim to talk to.
//!
//! A thin copy of the rig in `crates/combined/tests/panel_support/mod.rs`, which `combined`'s own
//! tests keep to themselves: it holds only what the shim tests use.

use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use combine_protocol::{
    ClientMessage, DaemonMessage, HostId, McpCaller, McpOpen, RequestId, TermId,
};
use combined::adapters::fs::data_dir::DataDir;
use combined::adapters::loader::DiagramLimits;
use combined::core::{LayoutTiming, PanelTiming};
use combined::mcp_server::McpAudit;
use combined::model::{SpaceId, SpawnSpec};
use combined::ports::{DiagramCheck, FileStore, ScratchStore, TerminalBackend};
use combined::rpc::{self, ServeOptions, ServerHandle};
use combined::testing::{
    check_in_scratch, work_dir, FakeBackend, FakeCallerIdentity, FakeConfig, FakeControl,
    FakeDiagramCheck, FakeWallClock, MemoryFileStore, MemoryScratchStore, ShortTempDir, WireClient,
};
use combined::wiring::ids::IdReservation;
use combined::wiring::panels::PanelParts;
use combined::wiring::server_parts_with_panels;

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
    pub options: ServeOptions,
    pub diagram: TestDiagramCheck,
}

impl Default for PanelServerConfig {
    fn default() -> Self {
        PanelServerConfig {
            options: ServeOptions::default(),
            diagram: TestDiagramCheck::Fake,
        }
    }
}

/// Collects the audit lines of the MCP calls, so they don't reach the test's stderr.
#[derive(Default)]
pub struct AuditLog {
    lines: Mutex<Vec<String>>,
}

impl AuditLog {
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

/// A server on the fake backend with in-memory stores.
pub struct PanelServer {
    pub server: ServerHandle,
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
    config: PanelServerConfig,
    starts: u32,
}

impl PanelServer {
    pub fn start(config: PanelServerConfig) -> PanelServer {
        let store = Arc::new(MemoryFileStore::new());
        let dir = ShortTempDir::new();
        let (backend, control) = FakeBackend::new(FakeConfig::default());
        let work = work_dir(&dir);
        fs::create_dir_all(work.join(".git")).unwrap();
        let space = SpaceId(work.to_str().unwrap().to_string());
        let scratch = Arc::new(MemoryScratchStore::new());
        let check = Arc::new(FakeDiagramCheck::new());
        let wall = Arc::new(FakeWallClock::new(START_WALL_MS));
        let callers = Arc::new(FakeCallerIdentity::new(None));
        let audit = Arc::new(AuditLog::default());
        let (server, socket) = launch_server(
            &dir, 1, &backend, &store, &check, &wall, &scratch, &callers, &audit, &config,
        );
        PanelServer {
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
            config,
            starts: 1,
        }
    }

    /// Stops the server and starts another on the same data dir and stores, as a daemon restart.
    pub fn restart(&mut self) {
        assert!(self.server.shutdown().is_none());
        self.starts += 1;
        let (server, socket) = launch_server(
            &self.dir,
            self.starts,
            &self.backend,
            &self.store,
            &self.check,
            &self.wall,
            &self.scratch,
            &self.callers,
            &self.audit,
            &self.config,
        );
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
        block_on(TerminalBackend::spawn(&*self.backend, spec)).unwrap()
    }

    /// Waits until the daemon accepts a caller whose working directory is `cwd`: a terminal that
    /// just started is listed a moment later, and the shim would exit 1 before that.
    pub fn wait_listed(&self, cwd: &Path) {
        let caller = McpCaller {
            terminal_id: None,
            terminal_socket: None,
            cwd: Some(cwd.to_str().unwrap().to_string()),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (mut client, _) = WireClient::open(&self.socket);
            client.send(&ClientMessage::McpOpen(McpOpen {
                request: RequestId(1),
                caller: caller.clone(),
            }));
            match client.recv() {
                DaemonMessage::McpOpened(_) => return,
                DaemonMessage::Error(_) if Instant::now() < deadline => {
                    drop(client);
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => panic!("{} never became a caller: {other:?}", cwd.display()),
            }
        }
    }
}

/// Starts a server on a fresh socket and the data dir `dir` keeps across starts.
#[allow(clippy::too_many_arguments)]
fn launch_server(
    dir: &ShortTempDir,
    start: u32,
    backend: &Arc<FakeBackend>,
    store: &Arc<MemoryFileStore>,
    check: &Arc<FakeDiagramCheck>,
    wall: &Arc<FakeWallClock>,
    scratch: &Arc<MemoryScratchStore>,
    callers: &Arc<FakeCallerIdentity>,
    audit: &Arc<AuditLog>,
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
    let served: Arc<dyn FileStore> = store.clone();
    let fake_check: Arc<dyn DiagramCheck> = check.clone();
    let diagram = config.diagram;
    let wall = wall.clone();
    let scratch: Arc<dyn ScratchStore> = scratch.clone();
    let callers = callers.clone();
    let audit = audit.clone();
    let server = rpc::spawn(
        socket,
        move || {
            // The test serves its own data dir, so no other writer needs the lock.
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
                LayoutTiming::default(),
                PanelParts {
                    diagram_check,
                    wall_clock: wall,
                    scratch,
                    timing: TEST_TIMING,
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

struct ThreadWaker(std::thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs a future that never needs a runtime, as the fake backend's calls don't.
fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut future = Box::pin(future);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::park();
    }
}
