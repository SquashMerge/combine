#![allow(dead_code)]
//! What the panel integration tests share: a scratch directory, an in-process daemon on the
//! fake backend with its panel API, the hosts file, and a layout sink that records panel
//! traffic. No ssh, no network listener and no herdr-core.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use combine_app::hosts::local::{herdr_core_socket_refusal, SocketGuardEnv};
use combine_app::hosts::panels::PanelReply;
use combine_app::hosts::session::LayoutSink;
use combine_protocol::{ErrorCode, HostId, LayoutSnapshot, Panels, RequestId, TermId};
use combined::adapters::fs::data_dir::DataDir;
use combined::model::{SpaceId, SpawnSpec};
use combined::ports::TerminalBackend;
use combined::rpc::socket::{bind, MAX_SOCKET_PATH};
use combined::rpc::{self, PanelHandle, ServeOptions, ServerHandle};
use combined::testing::{FakeBackend, FakeConfig};
use combined::wiring::ids::IdReservation;
use combined::wiring::server_parts;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// `/tmp/c48-<pid>-<n>`, mode 0700, removed on drop.
pub struct ScratchDir {
    pub path: PathBuf,
}

impl ScratchDir {
    pub fn new() -> ScratchDir {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = PathBuf::from(format!("/tmp/c48-{}-{n}", std::process::id()));
        fs::create_dir(&path).expect("create the scratch directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("restrict it");
        ScratchDir { path }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A daemon on the fake backend, served in this process, with the file store and the diagram
/// check `server_parts` gives. Its project is `<scratch>/<name>/work` with one terminal in it.
pub struct PanelDaemon {
    backend: Arc<FakeBackend>,
    data_dir: PathBuf,
    socket: PathBuf,
    work: PathBuf,
    pub host_id: HostId,
    pub term: TermId,
    server: Option<ServerHandle>,
}

impl PanelDaemon {
    /// Seeds the terminal and the project. The daemon is not serving until [`start`](Self::start).
    pub fn new(scratch: &ScratchDir, name: &str, host_id: &str) -> PanelDaemon {
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
        PanelDaemon {
            backend,
            socket: data_dir.join("combined.sock"),
            data_dir,
            work,
            host_id: HostId(host_id.to_string()),
            term,
            server: None,
        }
    }

    pub fn work(&self) -> &Path {
        &self.work
    }

    pub fn space(&self) -> SpaceId {
        SpaceId(self.work.to_string_lossy().into_owned())
    }

    /// Writes `<work>/.workspace/layout.json`, for the daemon to read when it first watches the
    /// space.
    pub fn seed_layout(&self, json: &str) {
        let dir = self.work.join(".workspace");
        fs::create_dir_all(&dir).expect("create .workspace");
        fs::write(dir.join("layout.json"), json).expect("write layout.json");
    }

    pub fn start(&mut self) {
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

    pub fn stop(&mut self) {
        let mut server = self.server.take().expect("the daemon is running");
        assert!(
            server.shutdown().is_none(),
            "the server stopped with an error"
        );
    }

    pub fn panels(&self) -> PanelHandle {
        self.server
            .as_ref()
            .expect("the daemon is running")
            .panels()
    }

    pub fn connections(&self) -> usize {
        self.server
            .as_ref()
            .expect("the daemon is running")
            .stats()
            .connections()
    }

    pub fn checked_socket(&self) -> &Path {
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

/// Writes `<client_dir>/hosts.json` and returns its bytes.
pub fn write_hosts(client_dir: &Path, hosts: serde_json::Value) -> Vec<u8> {
    fs::create_dir_all(client_dir).expect("create the client dir");
    let bytes = serde_json::to_vec(&serde_json::json!({ "version": 1, "hosts": hosts }))
        .expect("encode hosts.json");
    fs::write(client_dir.join("hosts.json"), &bytes).expect("write hosts.json");
    bytes
}

/// One thing a host's session handed to the layout sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEvent {
    Layout(HostId, Option<RequestId>, LayoutSnapshot),
    Panels(HostId, Panels),
    PanelReply(HostId, PanelReply),
}

/// A [`LayoutSink`] that sends the layout, panel list and panel reply events down a channel.
pub struct RecordingPanels {
    events: Mutex<Sender<PanelEvent>>,
}

impl RecordingPanels {
    pub fn new() -> (Arc<RecordingPanels>, Receiver<PanelEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(RecordingPanels {
                events: Mutex::new(tx),
            }),
            rx,
        )
    }

    fn record(&self, event: PanelEvent) {
        let _ = self.events.lock().expect("the sender").send(event);
    }
}

impl LayoutSink for RecordingPanels {
    fn layout(&self, host_id: &HostId, request: Option<RequestId>, snapshot: LayoutSnapshot) {
        self.record(PanelEvent::Layout(host_id.clone(), request, snapshot));
    }

    fn layout_error(&self, _: &HostId, _: RequestId, _: ErrorCode, _: String) {}

    fn layout_disconnected(&self, _: &HostId) {}

    fn panels(&self, host_id: &HostId, panels: Panels) {
        self.record(PanelEvent::Panels(host_id.clone(), panels));
    }

    fn panel_reply(&self, host_id: &HostId, reply: PanelReply) {
        self.record(PanelEvent::PanelReply(host_id.clone(), reply));
    }
}
