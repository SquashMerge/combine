//! Two in-process daemons on the fake backend, with the hosts service pointed at both through
//! local sockets: spaces from both hosts, a dropped host that dims and reconnects, and a
//! refused host-ID mismatch. No ssh, no network listener and no herdr-core.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use combine_app::hosts::attachments::{StreamDelivery, StreamId, TerminalSink};
use combine_app::hosts::config::Transport;
use combine_app::hosts::identity::mismatch_message;
use combine_app::hosts::local::{herdr_core_socket_refusal, LocalSocketConnector, SocketGuardEnv};
use combine_app::hosts::model::{ConnectionState, HostView, HostsSnapshot};
use combine_app::hosts::service::{HostsService, ServiceDeps, SnapshotSink};
use combine_app::hosts::session::LayoutSink;
use combine_app::hosts::spawn::{SpawnReply, SpawnSink};
use combine_app::hosts::supervisor::{Clock, StopSignal, SystemClock, Wake};
use combine_protocol::{ErrorCode, HostId, LayoutSnapshot, RequestId, TermId};
use combined::adapters::fs::data_dir::DataDir;
use combined::model::SpawnSpec;
use combined::ports::TerminalBackend;
use combined::rpc::socket::{bind, MAX_SOCKET_PATH};
use combined::rpc::{self, ServeOptions, ServerHandle};
use combined::testing::{fresh_dir, FakeBackend, FakeConfig};
use combined::wiring::ids::IdReservation;
use combined::wiring::server_parts;

const HOST_A: &str = "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c";
const HOST_B: &str = "0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c";
const HOST_C: &str = "5b0c1f3e-6a3d-4c52-9d0e-2f7a1b8c4d90";
const STRANGER: &str = "11111111-2222-4333-8444-555555555555";

const POLL: Duration = Duration::from_millis(20);
const DEADLINE: Duration = Duration::from_secs(30);

/// `/tmp/c23-<8 random hex digits>`, always new, mode 0700, removed on drop.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> ScratchDir {
        let path = fresh_dir(Path::new("/tmp"), "c23");
        ScratchDir { path }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A daemon on the fake backend, served in this process.
struct Daemon {
    backend: Arc<FakeBackend>,
    data_dir: PathBuf,
    socket: PathBuf,
    work: PathBuf,
    host_id: HostId,
    term: TermId,
    server: Option<ServerHandle>,
}

impl Daemon {
    /// Seeds one terminal in `<scratch>/<name>/work` and starts serving.
    fn new(scratch: &ScratchDir, name: &str, host_id: &str) -> Daemon {
        let root = scratch.path.join(name);
        let data_dir = root.join("data");
        let work = root.join("work");
        fs::create_dir_all(work.join(".git")).expect("create the project");
        let (backend, _control) = FakeBackend::new(FakeConfig::default());
        let spec = SpawnSpec {
            argv: vec!["sh".to_string()],
            cwd: work.to_string_lossy().into_owned(),
            env: Default::default(),
            labels: Default::default(),
            cols: 80,
            rows: 24,
        };
        let term = futures::executor::block_on(TerminalBackend::spawn(&*backend, spec))
            .expect("seed a terminal");
        let mut daemon = Daemon {
            backend,
            socket: data_dir.join("combined.sock"),
            data_dir,
            work,
            host_id: HostId(host_id.to_string()),
            term,
            server: None,
        };
        daemon.start();
        daemon
    }

    fn start(&mut self) {
        assert!(self.server.is_none(), "the daemon is already running");
        let data_dir = DataDir::open(&self.data_dir).expect("open the data dir");
        let (ids, id_base) = IdReservation::start(&data_dir.ids_json()).expect("reserve IDs");
        let socket = bind(&self.socket).expect("bind the socket");
        let backend: Arc<dyn TerminalBackend> = self.backend.clone();
        let host_id = self.host_id.clone();
        let server = rpc::spawn(
            socket,
            move || {
                drop(data_dir);
                server_parts(backend, id_base, host_id, ids).expect("build the server")
            },
            ServeOptions::default(),
        )
        .expect("start the server");
        self.server = Some(server);
    }

    fn stop(&mut self) {
        let mut server = self.server.take().expect("the daemon is running");
        assert!(
            server.shutdown().is_none(),
            "the server stopped with an error"
        );
    }

    fn connections(&self) -> usize {
        self.server
            .as_ref()
            .expect("the daemon is running")
            .stats()
            .connections()
    }

    fn checked_socket(&self) -> &Path {
        let len = self.socket.as_os_str().len();
        assert!(
            len <= MAX_SOCKET_PATH,
            "{} is {len} bytes",
            self.socket.display()
        );
        assert_eq!(
            herdr_core_socket_refusal(&self.socket, &SocketGuardEnv::from_process()),
            None,
            "{} must not look like a herdr-core socket",
            self.socket.display()
        );
        &self.socket
    }
}

struct NoLayouts;

impl LayoutSink for NoLayouts {
    fn layout(&self, _: &HostId, _: Option<RequestId>, _: LayoutSnapshot) {}
    fn layout_error(&self, _: &HostId, _: RequestId, _: ErrorCode, _: String) {}
    fn layout_disconnected(&self, _: &HostId) {}
}

struct NoSpawns;

impl SpawnSink for NoSpawns {
    fn spawn_reply(&self, _: &HostId, _: SpawnReply) {}
}

struct NoTerminals;

impl TerminalSink for NoTerminals {
    fn deliver(&self, _: StreamId, _: StreamDelivery<'_>) {}
}

struct IgnoreSnapshots;

impl SnapshotSink for IgnoreSnapshots {
    fn publish(&self, _: &HostsSnapshot) {}
}

/// The system clock with every delay capped, so a reconnect does not wait out the backoff.
struct QuickClock;

impl Clock for QuickClock {
    fn sleep(&self, delay: Duration, stop: &StopSignal) -> Wake {
        SystemClock.sleep(delay.min(Duration::from_millis(50)), stop)
    }
}

fn start_service(client_dir: &Path) -> HostsService {
    let mut deps = ServiceDeps::system(
        Arc::new(NoLayouts),
        Arc::new(NoSpawns),
        Arc::new(NoTerminals),
    );
    deps.clock = Arc::new(QuickClock);
    deps.connector_for = Box::new(|host| match &host.transport {
        Transport::Local { socket } => Arc::new(LocalSocketConnector::new(
            socket.clone(),
            SocketGuardEnv::from_process(),
        )),
        Transport::Ssh { .. } => panic!("this test never runs ssh"),
    });
    HostsService::start(client_dir, deps, Arc::new(IgnoreSnapshots))
}

fn write_hosts(client_dir: &Path, hosts: serde_json::Value) -> Vec<u8> {
    fs::create_dir_all(client_dir).expect("create the client dir");
    let bytes = serde_json::to_vec(&serde_json::json!({ "version": 1, "hosts": hosts }))
        .expect("encode hosts.json");
    fs::write(client_dir.join("hosts.json"), &bytes).expect("write hosts.json");
    bytes
}

/// Polls the service's snapshot until `done` holds, and panics with the last one at the deadline.
fn wait_for(service: &HostsService, what: &str, done: impl Fn(&HostsSnapshot) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let snapshot = service.snapshot();
        if done(&snapshot) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; last snapshot: {snapshot:#?}"
        );
        thread::sleep(POLL);
    }
}

fn wait_until(limit: Duration, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(POLL);
    }
}

fn host<'a>(snapshot: &'a HostsSnapshot, name: &str) -> &'a HostView {
    snapshot
        .hosts
        .iter()
        .find(|h| h.name == name)
        .unwrap_or_else(|| panic!("no host {name} in {snapshot:?}"))
}

/// The host is connected, reports `host_id`, and shows `daemon`'s terminal in its project.
fn shows_space(snapshot: &HostsSnapshot, name: &str, daemon: &Daemon) -> bool {
    let view = host(snapshot, name);
    view.connection == ConnectionState::Connected
        && view.host_id.as_ref() == Some(&daemon.host_id)
        && view.spaces.iter().any(|space| {
            space.key.host_id == daemon.host_id
                && space.key.path.ends_with(&format!("/{name}/work"))
                && space.terminals.iter().any(|t| t.id == daemon.term)
        })
}

fn is_dimmed(view: &HostView) -> bool {
    matches!(
        view.connection,
        ConnectionState::Dropped { .. } | ConnectionState::Reconnecting { .. }
    )
}

#[test]
fn two_hosts_show_spaces_from_both_and_a_dropped_host_reconnects() {
    let scratch = ScratchDir::new();
    let mut alpha = Daemon::new(&scratch, "alpha", HOST_A);
    let mut beta = Daemon::new(&scratch, "beta", HOST_B);
    assert!(alpha.work.ends_with("alpha/work") && beta.work.ends_with("beta/work"));
    let client_dir = scratch.path.join("client");
    write_hosts(
        &client_dir,
        serde_json::json!([
            { "name": "alpha", "socket": alpha.checked_socket() },
            { "name": "beta", "socket": beta.checked_socket() },
        ]),
    );
    let service = start_service(&client_dir);

    wait_for(&service, "both hosts connected with their spaces", |s| {
        shows_space(s, "alpha", &alpha) && shows_space(s, "beta", &beta)
    });

    beta.stop();
    wait_for(&service, "beta dimmed while alpha stays connected", |s| {
        is_dimmed(host(s, "beta")) && host(s, "alpha").connection == ConnectionState::Connected
    });

    beta.start();
    wait_for(&service, "beta reconnected with its space", |s| {
        shows_space(s, "alpha", &alpha) && shows_space(s, "beta", &beta)
    });

    service.stop();
    alpha.stop();
    beta.stop();
}

#[test]
fn a_host_id_mismatch_is_refused() {
    let scratch = ScratchDir::new();
    let mut gamma = Daemon::new(&scratch, "gamma", HOST_C);
    let client_dir = scratch.path.join("client");
    let written = write_hosts(
        &client_dir,
        serde_json::json!([
            { "name": "gamma", "socket": gamma.checked_socket(), "host_id": STRANGER },
        ]),
    );
    let service = start_service(&client_dir);

    let expected = ConnectionState::Failed {
        message: mismatch_message(
            "gamma",
            &HostId(STRANGER.to_string()),
            &HostId(HOST_C.to_string()),
        ),
        detail: None,
    };
    wait_for(&service, "the mismatch to be refused", |s| {
        host(s, "gamma").connection == expected
    });
    assert_eq!(fs::read(client_dir.join("hosts.json")).unwrap(), written);
    wait_until(
        Duration::from_secs(10),
        "the client to close its connection",
        || gamma.connections() == 0,
    );

    service.stop();
    gamma.stop();
}
