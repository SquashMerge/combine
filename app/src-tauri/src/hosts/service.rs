//! The hosts service: owns one supervisor per host, keeps the registry and publishes a
//! snapshot after every change.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use combine_protocol::{ErrorCode, HostId, LayoutSnapshot, Panels, RequestId};

use super::attachments::{Attachments, TerminalSink};
use super::config::{HostConfig, HostsStore, Transport};
use super::link::Connector;
use super::local::{LocalSocketConnector, SocketGuardEnv};
use super::model::{
    BackendView, ConfigStatus, ConnectionState, HostView, HostsSnapshot, SpaceView,
};
use super::outbound::Outbound;
use super::panels::PanelReply;
use super::registry::HostRegistry;
use super::session::{LayoutSink, LivePhase};
use super::spawn::{SpawnReply, SpawnSink};
use super::ssh::{SshConnector, SYSTEM_SSH};
use super::supervisor::{
    spawn_supervisor, Clock, HostEvents, PhaseFactory, SessionPhase, StopSignal, SupervisorDeps,
    SystemClock,
};

pub trait SnapshotSink: Send + Sync {
    fn publish(&self, snapshot: &HostsSnapshot);
}

pub type ConnectorFor = Box<dyn Fn(&HostConfig) -> Arc<dyn Connector> + Send + Sync>;

pub struct ServiceDeps {
    pub connector_for: ConnectorFor,
    pub clock: Arc<dyn Clock>,
    pub phase: PhaseFactory,
    pub outbound: Arc<Outbound>,
    pub layout_sink: Arc<dyn LayoutSink>,
    pub spawn_sink: Arc<dyn SpawnSink>,
    pub attachments: Arc<Attachments>,
}

impl ServiceDeps {
    /// The real thing: system `ssh`, the guarded local socket, the system clock and the live
    /// session phase, with layout traffic going to `layout_sink`, spawn replies to `spawn_sink`
    /// and terminal traffic to `terminal_sink`.
    pub fn system(
        layout_sink: Arc<dyn LayoutSink>,
        spawn_sink: Arc<dyn SpawnSink>,
        terminal_sink: Arc<dyn TerminalSink>,
    ) -> Self {
        let outbound = Arc::new(Outbound::default());
        let attachments = Arc::new(Attachments::new(terminal_sink));
        let live = Arc::clone(&outbound);
        let routed = Arc::clone(&attachments);
        ServiceDeps {
            connector_for: Box::new(|host| match &host.transport {
                Transport::Ssh {
                    destination,
                    combined_path,
                } => Arc::new(SshConnector::new(
                    SYSTEM_SSH,
                    destination.as_str(),
                    combined_path.as_str(),
                )),
                Transport::Local { socket } => Arc::new(LocalSocketConnector::new(
                    socket.clone(),
                    SocketGuardEnv::from_process(),
                )),
            }),
            clock: Arc::new(SystemClock),
            phase: Arc::new(move || {
                Box::new(LivePhase::new(Arc::clone(&live), Arc::clone(&routed)))
                    as Box<dyn SessionPhase>
            }),
            outbound,
            layout_sink,
            spawn_sink,
            attachments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryError {
    UnknownHost(String),
}

impl fmt::Display for RetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetryError::UnknownHost(name) => write!(f, "no host named \"{name}\" in hosts.json"),
        }
    }
}

impl std::error::Error for RetryError {}

struct Inner {
    registry: Mutex<HostRegistry>,
    sink: Arc<dyn SnapshotSink>,
    store: Arc<HostsStore>,
    hosts: Vec<HostConfig>,
    deps: ServiceDeps,
}

impl Inner {
    fn registry(&self) -> MutexGuard<'_, HostRegistry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mutates the registry and publishes the result while still holding the lock, so
    /// snapshots reach the sink in revision order.
    fn mutate(&self, change: impl FnOnce(&mut HostRegistry)) {
        let mut registry = self.registry();
        change(&mut registry);
        self.sink.publish(&registry.snapshot());
    }
}

impl HostEvents for Inner {
    fn connection(&self, name: &str, state: ConnectionState) {
        self.mutate(|r| r.set_connection(name, state));
    }

    fn host_id(&self, name: &str, host_id: &HostId) {
        self.mutate(|r| r.set_host_id(name, host_id));
    }

    fn spaces(&self, host_id: &HostId, spaces: Vec<SpaceView>, backend: BackendView) {
        self.mutate(|r| r.replace_spaces(host_id, spaces, backend));
    }

    fn layout(&self, host_id: &HostId, request: Option<RequestId>, snapshot: LayoutSnapshot) {
        self.deps.layout_sink.layout(host_id, request, snapshot);
    }

    fn layout_error(&self, host_id: &HostId, request: RequestId, code: ErrorCode, message: String) {
        self.deps
            .layout_sink
            .layout_error(host_id, request, code, message);
    }

    fn layout_disconnected(&self, host_id: &HostId) {
        self.deps.layout_sink.layout_disconnected(host_id);
    }

    fn spawn_reply(&self, host_id: &HostId, reply: SpawnReply) {
        self.deps.spawn_sink.spawn_reply(host_id, reply);
    }

    fn panels(&self, host_id: &HostId, panels: Panels) {
        self.deps.layout_sink.panels(host_id, panels);
    }

    fn panel_reply(&self, host_id: &HostId, reply: PanelReply) {
        self.deps.layout_sink.panel_reply(host_id, reply);
    }
}

struct Running {
    name: String,
    stop: StopSignal,
    thread: JoinHandle<()>,
}

pub struct HostsService {
    inner: Arc<Inner>,
    running: Mutex<Vec<Running>>,
}

impl HostsService {
    /// Loads `hosts.json` from `data_dir` and starts one supervisor per entry. An invalid file
    /// shows its error, adds no hosts and starts nothing.
    pub fn start(data_dir: &Path, deps: ServiceDeps, sink: Arc<dyn SnapshotSink>) -> HostsService {
        let path = data_dir.join("hosts.json");
        let (store, loaded) = HostsStore::load(path.clone());
        let (error, hosts) = match loaded {
            Ok(config) => (None, config.hosts),
            Err(e) => (Some(e.to_string()), Vec::new()),
        };
        let views = hosts
            .iter()
            .map(|h| HostView {
                name: h.name.clone(),
                host_id: h.host_id.clone(),
                connection: ConnectionState::Connecting,
                spaces: Vec::new(),
                backend: None,
            })
            .collect();
        let status = ConfigStatus {
            path: path.to_string_lossy().into_owned(),
            error,
        };
        let inner = Arc::new(Inner {
            registry: Mutex::new(HostRegistry::new(status, views)),
            sink,
            store: Arc::new(store),
            hosts,
            deps,
        });
        let service = HostsService {
            inner,
            running: Mutex::new(Vec::new()),
        };
        {
            let mut running = service.running();
            for host in &service.inner.hosts {
                running.push(service.spawn(host, host.host_id.clone()));
            }
        }
        service
    }

    fn running(&self) -> MutexGuard<'_, Vec<Running>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn spawn(&self, host: &HostConfig, stored: Option<HostId>) -> Running {
        let inner = &self.inner;
        let stop = StopSignal::new();
        let ssh = match &host.transport {
            Transport::Ssh {
                destination,
                combined_path,
            } => Some((destination.clone(), combined_path.clone())),
            Transport::Local { .. } => None,
        };
        let deps = SupervisorDeps {
            connector: (inner.deps.connector_for)(host),
            store: inner.store.clone(),
            clock: Arc::clone(&inner.deps.clock),
            events: inner.clone(),
            phase: Arc::clone(&inner.deps.phase),
            outbound: Arc::clone(&inner.deps.outbound),
        };
        Running {
            name: host.name.clone(),
            thread: spawn_supervisor(host.name.clone(), stored, ssh, deps, stop.clone()),
            stop,
        }
    }

    /// The channel that sends to each connected host's daemon.
    pub fn outbound(&self) -> Arc<Outbound> {
        Arc::clone(&self.inner.deps.outbound)
    }

    /// The router for panel streams.
    pub fn attachments(&self) -> Arc<Attachments> {
        Arc::clone(&self.inner.deps.attachments)
    }

    pub fn snapshot(&self) -> HostsSnapshot {
        self.inner.registry().snapshot()
    }

    /// Starts a fresh supervisor for a failed host, with a fresh backoff and the host ID
    /// recorded so far. It does not reload `hosts.json`. Any other state is a no-op.
    pub fn retry(&self, name: &str) -> Result<(), RetryError> {
        let host = self
            .inner
            .hosts
            .iter()
            .find(|h| h.name == name)
            .ok_or_else(|| RetryError::UnknownHost(name.to_string()))?;
        let mut running = self.running();
        let stored = {
            let registry = self.inner.registry();
            match registry.host(name) {
                Some(view) if matches!(view.connection, ConnectionState::Failed { .. }) => {
                    view.host_id.clone()
                }
                _ => return Ok(()),
            }
        };
        if let Some(index) = running.iter().position(|r| r.name == name) {
            let old = running.swap_remove(index);
            old.stop.stop();
            let _ = old.thread.join();
        }
        running.push(self.spawn(host, stored));
        Ok(())
    }

    /// Stops every supervisor, closing its link, and waits for them to end.
    pub fn stop(&self) {
        let all: Vec<Running> = std::mem::take(&mut *self.running());
        for r in &all {
            r.stop.stop();
        }
        for r in all {
            let _ = r.thread.join();
        }
    }
}

/// Stops the supervisors if `stop` wasn't called, so a test that fails before its `stop`
/// doesn't leave them retrying until the process exits. After `stop` it does nothing.
impl Drop for HostsService {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use combine_protocol::{ErrorCode, ErrorMessage, Spawned, TermId, MAX_FRAME_LEN};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Receiver;

    use super::super::config::parse_hosts;
    use super::super::identity::{duplicate_message, mismatch_message};
    use super::super::link::{ConnectError, FailureText};
    use super::super::supervisor::IdlePhase;
    use super::super::testing::{
        host_id, wait_for_snapshot, within, Double, FakeClock, Probe, RecordingLayoutSink,
        RecordingSink, RecordingSpawnSink, RecordingTerminalSink, ScriptedConnector, ShortTempDir,
        Step, HOST_A, HOST_B,
    };
    use super::*;

    const MINI_ENTRY: &str = r#"{"name":"mini","ssh":"example@mini"}"#;

    struct Fixture {
        dir: ShortTempDir,
        service: Arc<HostsService>,
        snapshots: Receiver<HostsSnapshot>,
        connectors: Arc<HashMap<String, Arc<ScriptedConnector>>>,
        built: Arc<AtomicUsize>,
    }

    impl Fixture {
        fn hosts_json(&self) -> PathBuf {
            self.dir.path().join("hosts.json")
        }

        fn bytes(&self) -> Vec<u8> {
            fs::read(self.hosts_json()).expect("read hosts.json")
        }
    }

    /// Stops the service, failing the test if its joins do not finish (#82).
    fn stop(service: &Arc<HostsService>) {
        let service = Arc::clone(service);
        within("HostsService::stop", move || service.stop());
    }

    /// Retries a host, failing the test if the call does not return (#82).
    fn retry(service: &Arc<HostsService>, name: &str) -> Result<(), RetryError> {
        let (service, name) = (Arc::clone(service), name.to_string());
        within("HostsService::retry", move || service.retry(&name))
    }

    fn file(entries: &[&str]) -> String {
        format!(r#"{{"version":1,"hosts":[{}]}}"#, entries.join(","))
    }

    fn start(contents: &str, scripts: Vec<(&str, Vec<Step>)>, clock: FakeClock) -> Fixture {
        let dir = ShortTempDir::new();
        fs::write(dir.path().join("hosts.json"), contents).expect("write hosts.json");
        let connectors: Arc<HashMap<String, Arc<ScriptedConnector>>> = Arc::new(
            scripts
                .into_iter()
                .map(|(name, steps)| (name.to_string(), Arc::new(ScriptedConnector::new(steps))))
                .collect(),
        );
        let built = Arc::new(AtomicUsize::new(0));
        let (lookup, counter) = (Arc::clone(&connectors), Arc::clone(&built));
        let deps = ServiceDeps {
            connector_for: Box::new(move |host| {
                counter.fetch_add(1, Ordering::SeqCst);
                match lookup.get(&host.name) {
                    Some(connector) => Arc::clone(connector) as Arc<dyn Connector>,
                    None => Arc::new(ScriptedConnector::new(Vec::new())),
                }
            }),
            clock: Arc::new(clock),
            phase: Arc::new(|| Box::new(IdlePhase) as Box<dyn SessionPhase>),
            outbound: Arc::new(Outbound::default()),
            layout_sink: RecordingLayoutSink::new().0,
            spawn_sink: RecordingSpawnSink::new().0,
            attachments: Arc::new(Attachments::new(RecordingTerminalSink::new())),
        };
        let (sink, snapshots) = RecordingSink::new();
        let service = Arc::new(HostsService::start(dir.path(), deps, sink));
        Fixture {
            dir,
            service,
            snapshots,
            connectors,
            built,
        }
    }

    fn connection_of(snapshot: &HostsSnapshot, name: &str) -> Option<ConnectionState> {
        snapshot
            .hosts
            .iter()
            .find(|h| h.name == name)
            .map(|h| h.connection.clone())
    }

    fn wait_failed(f: &Fixture, name: &str) -> ConnectionState {
        let snapshot = wait_for_snapshot(&f.snapshots, |s| {
            matches!(connection_of(s, name), Some(ConnectionState::Failed { .. }))
        });
        connection_of(&snapshot, name).expect("the host is listed")
    }

    fn failed(message: &str, detail: Option<&str>) -> ConnectionState {
        ConnectionState::Failed {
            message: message.to_string(),
            detail: detail.map(str::to_string),
        }
    }

    fn refusal(message: String) -> ErrorMessage {
        ErrorMessage {
            code: ErrorCode::Internal,
            message,
            request: None,
            attachment: None,
        }
    }

    fn frame(json: &str) -> Vec<u8> {
        let mut bytes = ((json.len() + 1) as u32).to_be_bytes().to_vec();
        bytes.push(1u8);
        bytes.extend_from_slice(json.as_bytes());
        bytes
    }

    #[test]
    fn invalid_hosts_json_shows_error_and_starts_nothing() {
        let bad = r#"{"version":2,"hosts":[]}"#;
        let f = start(bad, Vec::new(), FakeClock::new());
        let snapshot = f.service.snapshot();
        assert_eq!(
            snapshot.config.error.as_deref(),
            Some("hosts.json has version 2; this Combine reads version 1")
        );
        assert!(snapshot.hosts.is_empty());
        assert_eq!(f.built.load(Ordering::SeqCst), 0);
        assert_eq!(f.bytes(), bad.as_bytes());
        stop(&f.service);
    }

    #[test]
    fn missing_hosts_json_means_no_hosts_and_no_error() {
        let dir = ShortTempDir::new();
        let (sink, _snapshots) = RecordingSink::new();
        let service = Arc::new(HostsService::start(
            dir.path(),
            ServiceDeps::system(
                RecordingLayoutSink::new().0,
                RecordingSpawnSink::new().0,
                RecordingTerminalSink::new(),
            ),
            sink,
        ));
        let snapshot = service.snapshot();
        assert_eq!(snapshot.config.error, None);
        assert!(snapshot.hosts.is_empty());
        assert_eq!(
            snapshot.config.path,
            dir.path().join("hosts.json").to_string_lossy()
        );
        stop(&service);
    }

    #[test]
    fn spawn_replies_reach_the_spawn_sink() {
        let dir = ShortTempDir::new();
        let (sink, _snapshots) = RecordingSink::new();
        let (spawn_sink, received) = RecordingSpawnSink::new();
        let service = Arc::new(HostsService::start(
            dir.path(),
            ServiceDeps::system(
                RecordingLayoutSink::new().0,
                spawn_sink,
                RecordingTerminalSink::new(),
            ),
            sink,
        ));
        let events: &dyn HostEvents = &*service.inner;
        let host = host_id(HOST_A);
        let spawned = Spawned {
            request: RequestId(4),
            space: "/work".to_string(),
            terminal: TermId("t1".to_string()),
        };
        events.spawn_reply(&host, SpawnReply::Spawned(spawned.clone()));
        events.spawn_reply(&host, SpawnReply::Disconnected);
        let recv =
            |rx: &Receiver<(HostId, SpawnReply)>| rx.recv_timeout(super::super::testing::WAIT);
        assert_eq!(
            recv(&received),
            Ok((host.clone(), SpawnReply::Spawned(spawned)))
        );
        assert_eq!(recv(&received), Ok((host, SpawnReply::Disconnected)));
        stop(&service);
    }

    #[test]
    fn layout_events_reach_the_layout_sink() {
        use super::super::testing::LayoutEvent;
        use combine_protocol::{LayoutNode, LayoutSnapshot};

        let dir = ShortTempDir::new();
        let (sink, snapshots) = RecordingSink::new();
        let (layout_sink, received) = RecordingLayoutSink::new();
        let service = Arc::new(HostsService::start(
            dir.path(),
            ServiceDeps::system(
                layout_sink,
                RecordingSpawnSink::new().0,
                RecordingTerminalSink::new(),
            ),
            sink,
        ));
        drop(snapshots);
        let events: &dyn HostEvents = &*service.inner;
        let host = host_id(HOST_A);
        let snapshot = LayoutSnapshot {
            space: "/work".to_string(),
            layout_revision: 2,
            root: LayoutNode::Tile {
                id: "t1".to_string(),
                panels: Vec::new(),
                active: None,
            },
            read_only: false,
            error: None,
            dropped: None,
        };
        events.layout(&host, Some(RequestId(4)), snapshot.clone());
        events.layout_error(
            &host,
            RequestId(5),
            ErrorCode::StaleLayout,
            "stale".to_string(),
        );
        events.layout_disconnected(&host);
        let recv = |rx: &Receiver<LayoutEvent>| rx.recv_timeout(super::super::testing::WAIT);
        assert_eq!(
            recv(&received),
            Ok(LayoutEvent::Layout(
                host.clone(),
                Some(RequestId(4)),
                snapshot
            ))
        );
        assert_eq!(
            recv(&received),
            Ok(LayoutEvent::Error(
                host.clone(),
                RequestId(5),
                ErrorCode::StaleLayout,
                "stale".to_string()
            ))
        );
        assert_eq!(recv(&received), Ok(LayoutEvent::Disconnected(host)));
        stop(&service);
    }

    #[test]
    fn panel_events_reach_the_layout_sink() {
        use super::super::testing::LayoutEvent;
        use combine_protocol::{Panel, PanelState, Renderer};

        let dir = ShortTempDir::new();
        let (sink, snapshots) = RecordingSink::new();
        let (layout_sink, received) = RecordingLayoutSink::new();
        let service = Arc::new(HostsService::start(
            dir.path(),
            ServiceDeps::system(
                layout_sink,
                RecordingSpawnSink::new().0,
                RecordingTerminalSink::new(),
            ),
            sink,
        ));
        drop(snapshots);
        let events: &dyn HostEvents = &*service.inner;
        let host = host_id(HOST_A);
        let list = Panels {
            request: None,
            space: "/work".to_string(),
            panels: Vec::new(),
            skipped: Vec::new(),
            error: None,
        };
        let panel = Panel {
            request: RequestId(3),
            space: "/work".to_string(),
            id: "notes".to_string(),
            revision: 1,
            renderer: Renderer::Markdown,
            source: String::new(),
            state: PanelState::Validated,
            error: None,
        };
        events.panels(&host, list.clone());
        events.panel_reply(&host, PanelReply::Panel(panel.clone()));
        events.panel_reply(&host, PanelReply::Disconnected);
        let recv = |rx: &Receiver<LayoutEvent>| rx.recv_timeout(super::super::testing::WAIT);
        assert_eq!(recv(&received), Ok(LayoutEvent::Panels(host.clone(), list)));
        assert_eq!(
            recv(&received),
            Ok(LayoutEvent::PanelReply(
                host.clone(),
                PanelReply::Panel(panel)
            ))
        );
        assert_eq!(
            recv(&received),
            Ok(LayoutEvent::PanelReply(host, PanelReply::Disconnected))
        );
        stop(&service);
    }

    #[test]
    fn hosts_start_connecting_in_file_order() {
        let contents = file(&[
            r#"{"name":"zeta","ssh":"example@zeta"}"#,
            r#"{"name":"alpha","ssh":"example@alpha"}"#,
        ]);
        let f = start(&contents, Vec::new(), FakeClock::new());
        let snapshot = f.service.snapshot();
        let names: Vec<&str> = snapshot.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["zeta", "alpha"]);
        wait_for_snapshot(&f.snapshots, |s| {
            s.hosts
                .iter()
                .all(|h| matches!(h.connection, ConnectionState::Failed { .. }))
        });
        stop(&f.service);
    }

    #[test]
    fn first_contact_is_written_to_hosts_json() {
        let double = Double::welcome(host_id(HOST_A));
        let probe = double.probe();
        let f = start(
            &file(&[MINI_ENTRY]),
            vec![("mini", vec![Step::Serve(double)])],
            FakeClock::new(),
        );
        let snapshot = wait_for_snapshot(&f.snapshots, |s| {
            connection_of(s, "mini") == Some(ConnectionState::Connected)
        });
        assert_eq!(snapshot.hosts[0].host_id, Some(host_id(HOST_A)));
        let written = parse_hosts(&f.bytes()).expect("hosts.json stays valid");
        assert_eq!(written.hosts[0].host_id, Some(host_id(HOST_A)));
        stop(&f.service);
        assert!(probe.closed_by_client());
    }

    #[test]
    fn stored_id_mismatch_leaves_hosts_json_unchanged() {
        let entry = format!(r#"{{"name":"mini","ssh":"example@mini","host_id":"{HOST_A}"}}"#);
        let double = Double::welcome(host_id(HOST_B));
        let probe = double.probe();
        let f = start(
            &file(&[&entry]),
            vec![("mini", vec![Step::Serve(double)])],
            FakeClock::new(),
        );
        let before = f.bytes();
        let expected = mismatch_message("mini", &host_id(HOST_A), &host_id(HOST_B));
        assert_eq!(wait_failed(&f, "mini"), failed(&expected, None));
        assert_eq!(f.bytes(), before);
        assert_eq!(f.service.snapshot().hosts[0].host_id, Some(host_id(HOST_A)));
        stop(&f.service);
        assert!(probe.closed_by_client());
    }

    #[test]
    fn two_entries_reaching_one_machine_fail_the_second() {
        let first = format!(r#"{{"name":"first","ssh":"example@one","host_id":"{HOST_A}"}}"#);
        let second = r#"{"name":"second","ssh":"example@two"}"#;
        let (to_first, to_second) = (
            Double::welcome(host_id(HOST_A)),
            Double::welcome(host_id(HOST_A)),
        );
        let (probe_first, probe_second) = (to_first.probe(), to_second.probe());
        let f = start(
            &file(&[&first, second]),
            vec![
                ("first", vec![Step::Serve(to_first)]),
                ("second", vec![Step::Serve(to_second)]),
            ],
            FakeClock::new(),
        );
        let before = f.bytes();
        let expected = duplicate_message("second", "first", &host_id(HOST_A));
        let snapshot = wait_for_snapshot(&f.snapshots, |s| {
            connection_of(s, "first") == Some(ConnectionState::Connected)
                && matches!(
                    connection_of(s, "second"),
                    Some(ConnectionState::Failed { .. })
                )
        });
        assert_eq!(
            connection_of(&snapshot, "second"),
            Some(failed(&expected, None))
        );
        assert_eq!(snapshot.hosts[1].host_id, None);
        assert_eq!(f.bytes(), before);
        stop(&f.service);
        assert!(probe_first.closed_by_client());
        assert!(probe_second.closed_by_client());
    }

    #[test]
    fn one_mebibyte_refusal_is_published_trimmed() {
        let double = Double::refuse(refusal("x".repeat(MAX_FRAME_LEN as usize - 1024)));
        let f = start(
            &file(&[MINI_ENTRY]),
            vec![("mini", vec![Step::Serve(double)])],
            FakeClock::new(),
        );
        let expected_detail = format!("{}…", "x".repeat(256));
        assert_eq!(
            wait_failed(&f, "mini"),
            failed(
                "the daemon refused the connection (internal)",
                Some(&expected_detail)
            )
        );
        assert_eq!(expected_detail.chars().count(), 257);
        stop(&f.service);
    }

    #[test]
    fn session_decode_error_is_published_trimmed() {
        let json = format!(r#"{{"type":"{}"}}"#, r"a\u0007".repeat(300));
        let double = Double::welcome_then_frame(host_id(HOST_A), frame(&json));
        let f = start(
            &file(&[MINI_ENTRY]),
            vec![("mini", vec![Step::Serve(double)])],
            FakeClock::stop_after(1),
        );
        let snapshot = wait_for_snapshot(&f.snapshots, |s| {
            matches!(
                connection_of(s, "mini"),
                Some(ConnectionState::Dropped { .. })
            )
        });
        let Some(ConnectionState::Dropped {
            reason,
            detail: Some(detail),
            ..
        }) = connection_of(&snapshot, "mini")
        else {
            panic!("expected a dropped host with detail");
        };
        assert_eq!(reason, "the daemon sent a malformed frame");
        assert!(
            detail.starts_with(r#"unknown message type "a\u{7}a\u{7}"#),
            "{detail}"
        );
        assert_eq!(detail.chars().count(), 257);
        assert!(detail.ends_with('…'));
        stop(&f.service);
    }

    #[test]
    fn stderr_control_characters_are_stripped_from_detail() {
        let f = start(
            &file(&[MINI_ENTRY]),
            vec![(
                "mini",
                vec![Step::Exit {
                    stderr:
                        "ssh: \u{1b}[31mconnect to host mini port 22: Operation timed out\u{7}\r\n",
                    code: Some(255),
                }],
            )],
            FakeClock::stop_after(1),
        );
        let snapshot = wait_for_snapshot(&f.snapshots, |s| {
            matches!(
                connection_of(s, "mini"),
                Some(ConnectionState::Dropped { .. })
            )
        });
        assert_eq!(
            connection_of(&snapshot, "mini"),
            Some(ConnectionState::Dropped {
                reason: "ssh exited with status 255".to_string(),
                detail: Some(
                    "ssh: [31mconnect to host mini port 22: Operation timed out".to_string()
                ),
                retry_in_ms: 1000
            })
        );
        stop(&f.service);
    }

    #[test]
    fn retry_names_an_unknown_host() {
        let f = start(&file(&[MINI_ENTRY]), Vec::new(), FakeClock::new());
        let error = retry(&f.service, "nope").expect_err("unknown host");
        assert_eq!(error.to_string(), r#"no host named "nope" in hosts.json"#);
        stop(&f.service);
    }

    #[test]
    fn retry_restarts_only_a_failed_host() {
        let double = Double::welcome(host_id(HOST_A));
        let probe: Probe = double.probe();
        let permanent = Step::ConnectError(ConnectError::Permanent(FailureText {
            text: "cannot run /usr/bin/ssh".to_string(),
            detail: None,
        }));
        let f = start(
            &file(&[MINI_ENTRY]),
            vec![("mini", vec![permanent, Step::Serve(double)])],
            FakeClock::new(),
        );
        wait_failed(&f, "mini");
        let connector = f.connectors.get("mini").expect("scripted").clone();
        assert_eq!(connector.connect_count(), 1);

        retry(&f.service, "mini").expect("retry a failed host");
        wait_for_snapshot(&f.snapshots, |s| {
            connection_of(s, "mini") == Some(ConnectionState::Connected)
        });
        assert_eq!(connector.connect_count(), 2);

        retry(&f.service, "mini").expect("a connected host is a no-op");
        assert_eq!(connector.connect_count(), 2);
        assert_eq!(f.built.load(Ordering::SeqCst), 2);
        stop(&f.service);
        assert!(probe.closed_by_client());
    }
}
