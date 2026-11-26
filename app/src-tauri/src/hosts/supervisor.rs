//! The per-host reconnect loop: connect, handshake, check the host ID, run the session, and
//! start again after a delay.
//!
//! `message` and `reason` are always Combine text. `detail` is always raw external text, which
//! the registry trims. Transient failures retry forever at a capped delay. Everything that
//! needs the user to act stops the loop until Retry.

use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use combine_protocol::{
    read_message, DaemonMessage, ErrorCode, HostId, LayoutSnapshot, Panels, ReadError, RequestId,
};

use super::backoff::Backoff;
use super::config::StoreError;
use super::failure::classify;
use super::handshake::{run_handshake, HandshakeFailure};
use super::identity::{
    check, duplicate_message, is_valid_host_id, mismatch_message, HostIdStore, IdentityCheck,
};
use super::link::{abort_link, Closer, ConnectError, Connector, ExitInfo, Link};
use super::model::{ConnectionState, SpaceView};
use super::outbound::Outbound;
use super::panels::PanelReply;
use super::spawn::SpawnReply;
use super::text::is_blank;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Elapsed,
    Stopped,
}

pub trait Clock: Send + Sync {
    /// Waits `delay`, or until `stop` is raised.
    fn sleep(&self, delay: Duration, stop: &StopSignal) -> Wake;
}

struct StopState {
    stopped: bool,
    current: Option<Closer>,
}

/// Ends a supervisor: raising it also closes the link the supervisor is blocked on.
#[derive(Clone)]
pub struct StopSignal {
    inner: Arc<(Mutex<StopState>, Condvar)>,
}

impl Default for StopSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl StopSignal {
    pub fn new() -> Self {
        StopSignal {
            inner: Arc::new((
                Mutex::new(StopState {
                    stopped: false,
                    current: None,
                }),
                Condvar::new(),
            )),
        }
    }

    fn state(&self) -> MutexGuard<'_, StopState> {
        self.inner.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn stop(&self) {
        let closer = {
            let mut state = self.state();
            state.stopped = true;
            state.current.clone()
        };
        self.inner.1.notify_all();
        if let Some(close) = closer {
            close();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.state().stopped
    }

    /// Registers the closer of the link the supervisor is about to block on. A stop that
    /// already happened closes it at once.
    pub fn set_closer(&self, closer: Closer) {
        let close_now = {
            let mut state = self.state();
            if state.stopped {
                Some(closer)
            } else {
                state.current = Some(closer);
                None
            }
        };
        if let Some(close) = close_now {
            close();
        }
    }

    pub fn clear_closer(&self) {
        self.state().current = None;
    }
}

/// Waits on the stop signal's condition variable, so a stop wakes it at once.
pub struct SystemClock;

impl Clock for SystemClock {
    fn sleep(&self, delay: Duration, stop: &StopSignal) -> Wake {
        let (guard, _) = stop
            .inner
            .1
            .wait_timeout_while(stop.state(), delay, |s| !s.stopped)
            .unwrap_or_else(PoisonError::into_inner);
        if guard.stopped {
            Wake::Stopped
        } else {
            Wake::Elapsed
        }
    }
}

pub trait HostEvents: Send + Sync {
    fn connection(&self, name: &str, state: ConnectionState);
    fn host_id(&self, name: &str, host_id: &HostId);
    /// The seam S4 uses to publish a host's spaces.
    fn spaces(&self, host_id: &HostId, spaces: Vec<SpaceView>);
    /// A space's layout: the reply to a request, or a push when `request` is `None`.
    fn layout(&self, _host_id: &HostId, _request: Option<RequestId>, _snapshot: LayoutSnapshot) {}
    /// The daemon refused a layout request. `message` is trimmed external text.
    fn layout_error(
        &self,
        _host_id: &HostId,
        _request: RequestId,
        _code: ErrorCode,
        _message: String,
    ) {
    }
    /// The session ended, so the layout watch is gone.
    fn layout_disconnected(&self, _host_id: &HostId) {}
    /// A reply to a spawn request, or the end of the session that could still answer one.
    fn spawn_reply(&self, _host_id: &HostId, _reply: SpawnReply) {}
    /// A space's panel list: the reply to a layout watch, or a push after a change.
    fn panels(&self, _host_id: &HostId, _panels: Panels) {}
    /// A reply to a panel fetch, or the end of the session that could still answer one.
    fn panel_reply(&self, _host_id: &HostId, _reply: PanelReply) {}
}

pub struct SessionIo<'a> {
    pub host_id: &'a HostId,
    pub reader: &'a mut (dyn Read + Send),
    pub writer: &'a mut (dyn Write + Send),
    pub events: &'a dyn HostEvents,
}

/// How a session ended. The strings are external text, published as `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    Closed,
    Io(String),
    Protocol(String),
}

pub trait SessionPhase: Send {
    fn run(&mut self, io: SessionIo<'_>) -> SessionEnd;
}

/// Reads daemon messages until the link ends and ignores them. The service runs
/// [`LivePhase`](super::session::LivePhase); this one stands in for it in tests.
#[derive(Default)]
pub struct IdlePhase;

impl SessionPhase for IdlePhase {
    fn run(&mut self, io: SessionIo<'_>) -> SessionEnd {
        loop {
            match read_message::<DaemonMessage, _>(&mut *io.reader) {
                Ok(None) => return SessionEnd::Closed,
                Ok(Some(_)) => {}
                Err(ReadError::Io(e)) => return SessionEnd::Io(e.to_string()),
                Err(ReadError::Decode(e)) => return SessionEnd::Protocol(e.to_string()),
            }
        }
    }
}

pub type PhaseFactory = Arc<dyn Fn() -> Box<dyn SessionPhase> + Send + Sync>;

#[derive(Clone)]
pub struct SupervisorDeps {
    pub connector: Arc<dyn Connector>,
    pub store: Arc<dyn HostIdStore>,
    pub clock: Arc<dyn Clock>,
    pub events: Arc<dyn HostEvents>,
    pub phase: PhaseFactory,
    pub outbound: Arc<Outbound>,
}

fn stderr_detail(exit: &Option<ExitInfo>) -> Option<String> {
    exit.as_ref()
        .map(|e| e.stderr.clone())
        .filter(|s| !is_blank(s))
}

pub fn store_failure_message(e: &StoreError) -> String {
    format!("could not save the host ID: {e}")
}

enum Attempt {
    /// The loop ends: stopped, or failed until the user retries.
    End,
    Dropped {
        reason: String,
        detail: Option<String>,
    },
}

struct Supervisor {
    name: String,
    stored: Option<HostId>,
    ssh: Option<(String, String)>,
    deps: SupervisorDeps,
    stop: StopSignal,
    backoff: Backoff,
    attempt: u32,
    connected_once: bool,
}

/// Runs the loop for one host until it is stopped or fails for good. `ssh` is the destination
/// and `combined_path`, which the failure messages name.
pub fn run_supervisor(
    name: String,
    stored: Option<HostId>,
    ssh: Option<(String, String)>,
    deps: SupervisorDeps,
    stop: StopSignal,
) {
    Supervisor {
        name,
        stored,
        ssh,
        deps,
        stop,
        backoff: Backoff::new(),
        attempt: 0,
        connected_once: false,
    }
    .run();
}

pub fn spawn_supervisor(
    name: String,
    stored: Option<HostId>,
    ssh: Option<(String, String)>,
    deps: SupervisorDeps,
    stop: StopSignal,
) -> JoinHandle<()> {
    thread::spawn(move || run_supervisor(name, stored, ssh, deps, stop))
}

impl Supervisor {
    fn run(&mut self) {
        loop {
            if self.stop.is_stopped() {
                break;
            }
            let state = if !self.connected_once && self.attempt == 0 {
                ConnectionState::Connecting
            } else {
                ConnectionState::Reconnecting {
                    attempt: self.attempt,
                }
            };
            self.deps.events.connection(&self.name, state);
            let (reason, detail) = match self.attempt_once() {
                Attempt::End => break,
                Attempt::Dropped { reason, detail } => (reason, detail),
            };
            self.attempt += 1;
            let delay = self.backoff.next_delay();
            self.deps.events.connection(
                &self.name,
                ConnectionState::Dropped {
                    reason,
                    detail,
                    retry_in_ms: u32::try_from(delay.as_millis()).unwrap_or(u32::MAX),
                },
            );
            if self.deps.clock.sleep(delay, &self.stop) == Wake::Stopped {
                break;
            }
        }
    }

    fn fail(&self, message: String, detail: Option<String>) -> Attempt {
        self.deps
            .events
            .connection(&self.name, ConnectionState::Failed { message, detail });
        Attempt::End
    }

    fn attempt_once(&mut self) -> Attempt {
        match self.deps.connector.connect() {
            Err(ConnectError::Permanent(ft)) => self.fail(ft.text, ft.detail),
            Err(ConnectError::Transient(ft)) => Attempt::Dropped {
                reason: ft.text,
                detail: ft.detail,
            },
            Ok(link) => {
                self.stop.set_closer(link.closer());
                let attempt = self.with_link(link);
                self.stop.clear_closer();
                attempt
            }
        }
    }

    fn with_link(&mut self, mut link: Link) -> Attempt {
        match run_handshake(&mut *link.reader, &mut *link.writer) {
            Err(failure) => {
                let exit = link.finish();
                self.after_failed_handshake(failure, exit)
            }
            Ok(welcome) => {
                let id = welcome.host_id;
                if !is_valid_host_id(&id.0) {
                    abort_link(link);
                    return self.fail(
                        "the daemon reported an invalid host ID".to_string(),
                        Some(id.0),
                    );
                }
                if let Err((message, detail)) = self.verify_identity(&id) {
                    abort_link(link);
                    return self.fail(message, detail);
                }
                self.run_session(link, &id)
            }
        }
    }

    fn after_failed_handshake(&self, failure: HandshakeFailure, exit: Option<ExitInfo>) -> Attempt {
        let classified = match (&exit, &self.ssh) {
            (Some(e), Some((dest, path))) => {
                classify(e.code, &e.stderr).map(|f| f.message(dest, path))
            }
            _ => None,
        };
        if let Some(message) = classified {
            return self.fail(message, stderr_detail(&exit));
        }
        match failure {
            HandshakeFailure::Permanent(ft) => {
                let detail = ft.detail.or_else(|| stderr_detail(&exit));
                self.fail(ft.text, detail)
            }
            HandshakeFailure::Transient(ft) => {
                let reason = exit
                    .as_ref()
                    .and_then(|e| e.code)
                    .filter(|code| *code != 0)
                    .map(|code| format!("ssh exited with status {code}"))
                    .unwrap_or(ft.text);
                Attempt::Dropped {
                    reason,
                    detail: stderr_detail(&exit).or(ft.detail),
                }
            }
        }
    }

    /// Checks the reported ID against the stored one, recording it on first contact. The
    /// error is a Combine message and its external detail.
    fn verify_identity(&mut self, id: &HostId) -> Result<(), (String, Option<String>)> {
        match check(self.stored.as_ref(), id) {
            IdentityCheck::Match => Ok(()),
            IdentityCheck::Mismatch => Err(self.mismatch(id)),
            IdentityCheck::FirstContact => self.first_contact(id),
        }
    }

    fn mismatch(&self, reported: &HostId) -> (String, Option<String>) {
        let stored = self.stored.clone().unwrap_or_else(|| reported.clone());
        (mismatch_message(&self.name, &stored, reported), None)
    }

    fn first_contact(&mut self, id: &HostId) -> Result<(), (String, Option<String>)> {
        match self.deps.store.record_first_contact(&self.name, id) {
            Ok(()) => {
                self.stored = Some(id.clone());
                self.deps.events.host_id(&self.name, id);
                Ok(())
            }
            Err(StoreError::AlreadyRecorded(recorded)) => {
                self.stored = Some(recorded.clone());
                if recorded == *id {
                    self.deps.events.host_id(&self.name, id);
                    Ok(())
                } else {
                    Err(self.mismatch(id))
                }
            }
            Err(StoreError::DuplicateHostId { other }) => {
                Err((duplicate_message(&self.name, &other, id), None))
            }
            Err(e) => Err((store_failure_message(&e), e.detail())),
        }
    }

    fn run_session(&mut self, mut link: Link, id: &HostId) -> Attempt {
        self.backoff.reset();
        self.attempt = 0;
        self.connected_once = true;
        self.deps
            .events
            .connection(&self.name, ConnectionState::Connected);
        let mut phase = (self.deps.phase)();
        let end = phase.run(SessionIo {
            host_id: id,
            reader: &mut *link.reader,
            writer: &mut *link.writer,
            events: &*self.deps.events,
        });
        self.stop.clear_closer();
        let exit = link.finish();
        let (reason, detail) = match end {
            SessionEnd::Closed => ("connection closed", None),
            SessionEnd::Io(e) => ("connection lost", Some(e)),
            SessionEnd::Protocol(e) => ("the daemon sent a malformed frame", Some(e)),
        };
        Attempt::Dropped {
            reason: reason.to_string(),
            detail: detail.or_else(|| stderr_detail(&exit)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::mpsc::Receiver;

    use super::super::failure::SshFailure;
    use super::super::link::{ticking_reader, FailureText};
    use super::super::ssh::SshConnector;
    use super::super::testing::{
        host_id, joins_within, recv, welcome_frame, write_fake_ssh, write_welcome_file, Double,
        Event, FakeClock, LostWakeupSocket, MemoryHostIdStore, NoExit, RecordingEvents,
        ScriptedConnector, ShortTempDir, Step, HOST_A, HOST_B, WAIT,
    };
    use super::*;

    struct Rig {
        connector: Arc<ScriptedConnector>,
        store: Arc<MemoryHostIdStore>,
        clock: Arc<FakeClock>,
        events: Receiver<Event>,
        deps: SupervisorDeps,
    }

    fn rig_with(steps: Vec<Step>, store: MemoryHostIdStore, clock: FakeClock) -> Rig {
        let connector = Arc::new(ScriptedConnector::new(steps));
        let store = Arc::new(store);
        let clock = Arc::new(clock);
        let (recorder, events) = RecordingEvents::new();
        let deps = SupervisorDeps {
            connector: connector.clone(),
            store: store.clone(),
            clock: clock.clone(),
            events: recorder,
            phase: Arc::new(|| Box::new(IdlePhase) as Box<dyn SessionPhase>),
            outbound: Arc::new(Outbound::default()),
        };
        Rig {
            connector,
            store,
            clock,
            events,
            deps,
        }
    }

    fn rig(steps: Vec<Step>) -> Rig {
        rig_with(steps, MemoryHostIdStore::new(), FakeClock::new())
    }

    fn ssh_pair() -> Option<(String, String)> {
        Some(("example@mini".to_string(), "/opt/combined".to_string()))
    }

    /// Runs the loop to its end, for scripts that end without outside help. A loop that does
    /// not end within [`WAIT`] is stopped and fails the test, instead of hanging it.
    fn run_to_end(rig: &Rig, stored: Option<HostId>, ssh: Option<(String, String)>) {
        let stop = StopSignal::new();
        let handle = spawn_supervisor(
            "mini".to_string(),
            stored,
            ssh,
            rig.deps.clone(),
            stop.clone(),
        );
        let ended = joins_within(handle, WAIT);
        stop.stop();
        assert!(ended, "the supervisor did not stop within {WAIT:?}");
    }

    fn connection(event: Event) -> ConnectionState {
        match event {
            Event::Connection(name, state) => {
                assert_eq!(name, "mini");
                state
            }
            other => panic!("expected a connection event, got {other:?}"),
        }
    }

    fn states(rx: &Receiver<Event>) -> Vec<ConnectionState> {
        rx.try_iter()
            .filter_map(|e| match e {
                Event::Connection(_, state) => Some(state),
                _ => None,
            })
            .collect()
    }

    fn failed(message: &str, detail: Option<&str>) -> ConnectionState {
        ConnectionState::Failed {
            message: message.to_string(),
            detail: detail.map(str::to_string),
        }
    }

    fn dropped(reason: &str, detail: Option<&str>, retry_in_ms: u32) -> ConnectionState {
        ConnectionState::Dropped {
            reason: reason.to_string(),
            detail: detail.map(str::to_string),
            retry_in_ms,
        }
    }

    fn a() -> HostId {
        host_id(HOST_A)
    }

    fn transient(text: &str) -> Step {
        Step::ConnectError(ConnectError::Transient(FailureText {
            text: text.to_string(),
            detail: None,
        }))
    }

    #[test]
    fn first_contact_records_host_id() {
        let double = Double::welcome(a());
        let probe = double.probe();
        let rig = rig_with(
            vec![Step::Serve(double)],
            MemoryHostIdStore::new(),
            FakeClock::stop_after(1),
        );
        let (store, events) = (rig.store.clone(), rig.events);
        let handle = spawn_supervisor("mini".to_string(), None, None, rig.deps, StopSignal::new());
        assert_eq!(connection(recv(&events)), ConnectionState::Connecting);
        assert_eq!(recv(&events), Event::HostId("mini".to_string(), a()));
        assert_eq!(connection(recv(&events)), ConnectionState::Connected);
        assert_eq!(store.calls(), [("mini".to_string(), a())]);
        probe.drop_now();
        assert_eq!(
            connection(recv(&events)),
            dropped("connection closed", None, 1000)
        );
        assert!(joins_within(handle, WAIT));
    }

    #[test]
    fn mismatch_on_later_connection_is_refused_and_closed() {
        let (first, second) = (Double::welcome(a()), Double::welcome(host_id(HOST_B)));
        let (probe_a, probe_b) = (first.probe(), second.probe());
        let rig = rig(vec![Step::Serve(first), Step::Serve(second)]);
        let (store, events) = (rig.store.clone(), rig.events);
        let handle = spawn_supervisor("mini".to_string(), None, None, rig.deps, StopSignal::new());
        assert_eq!(connection(recv(&events)), ConnectionState::Connecting);
        assert_eq!(recv(&events), Event::HostId("mini".to_string(), a()));
        assert_eq!(connection(recv(&events)), ConnectionState::Connected);
        probe_a.drop_now();
        assert!(joins_within(handle, WAIT));
        let rest: Vec<Event> = events.try_iter().collect();
        let mismatch = super::super::identity::mismatch_message("mini", &a(), &host_id(HOST_B));
        assert_eq!(
            rest.last(),
            Some(&Event::Connection(
                "mini".to_string(),
                failed(&mismatch, None)
            ))
        );
        assert!(rest
            .iter()
            .all(|e| !matches!(e, Event::HostId(..) | Event::Spaces(..))));
        assert_eq!(store.calls().len(), 1);
        assert!(probe_b.closed_by_client());
    }

    #[test]
    fn invalid_reported_host_id_is_refused() {
        let double = Double::welcome(host_id("NOT-A-UUID"));
        let probe = double.probe();
        let rig = rig(vec![Step::Serve(double)]);
        run_to_end(&rig, None, None);
        assert_eq!(
            states(&rig.events),
            [
                ConnectionState::Connecting,
                failed("the daemon reported an invalid host ID", Some("NOT-A-UUID"))
            ]
        );
        assert!(rig.store.calls().is_empty());
        assert!(probe.closed_by_client());
    }

    #[test]
    fn store_failure_stops_with_message() {
        let double = Double::welcome(a());
        let probe = double.probe();
        let store = MemoryHostIdStore::failing(StoreError::Io("disk full".to_string()));
        let rig = rig_with(vec![Step::Serve(double)], store, FakeClock::new());
        run_to_end(&rig, None, None);
        assert_eq!(
            states(&rig.events).last(),
            Some(&failed(
                "could not save the host ID: could not write hosts.json",
                Some("disk full")
            ))
        );
        assert_eq!(rig.connector.connect_count(), 1);
        assert!(rig.clock.delays().is_empty());
        assert!(probe.closed_by_client());
    }

    #[test]
    fn duplicate_host_id_stops_with_message() {
        let double = Double::welcome(a());
        let probe = double.probe();
        let store = MemoryHostIdStore::failing(StoreError::DuplicateHostId {
            other: "first".to_string(),
        });
        let rig = rig_with(vec![Step::Serve(double)], store, FakeClock::new());
        run_to_end(&rig, None, None);
        let message = super::super::identity::duplicate_message("mini", "first", &a());
        assert_eq!(states(&rig.events).last(), Some(&failed(&message, None)));
        assert_eq!(rig.connector.connect_count(), 1);
        assert!(probe.closed_by_client());
    }

    #[test]
    fn mismatch_over_ssh_kills_and_reaps_the_child() {
        let dir = ShortTempDir::new();
        let welcome = write_welcome_file(dir.path(), &host_id(HOST_B));
        let pidfile = dir.path().join("pid");
        let script = write_fake_ssh(
            dir.path(),
            &format!(
                "echo $$ > '{}'\ncat '{}'\nexec cat >/dev/null",
                pidfile.display(),
                welcome.display()
            ),
        );
        let rig = rig(Vec::new());
        let deps = SupervisorDeps {
            connector: Arc::new(SshConnector::new(&script, "example@mini", "/opt/combined")),
            ..rig.deps
        };
        run_supervisor(
            "mini".to_string(),
            Some(a()),
            ssh_pair(),
            deps,
            StopSignal::new(),
        );
        let mismatch = super::super::identity::mismatch_message("mini", &a(), &host_id(HOST_B));
        assert_eq!(states(&rig.events).last(), Some(&failed(&mismatch, None)));
        let pid = std::fs::read_to_string(&pidfile).expect("the script wrote its pid");
        let alive = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .output()
            .expect("run kill")
            .status
            .success();
        assert!(!alive, "the ssh child {} is still running", pid.trim());
    }

    #[test]
    fn ssh_failure_stops_without_retry() {
        const STDERR: &str = "example@mini: Permission denied (publickey).\n";
        let rig = rig(vec![Step::Exit {
            stderr: STDERR,
            code: Some(255),
        }]);
        run_to_end(&rig, None, ssh_pair());
        let message = SshFailure::AuthenticationFailed.message("example@mini", "/opt/combined");
        assert_eq!(
            states(&rig.events),
            [ConnectionState::Connecting, failed(&message, Some(STDERR))]
        );
        assert_eq!(rig.connector.connect_count(), 1);
        assert!(rig.clock.delays().is_empty());
    }

    #[test]
    fn reconnects_after_drop_with_backoff() {
        let (first, second) = (Double::welcome(a()), Double::welcome(a()));
        let (probe_first, probe_second) = (first.probe(), second.probe());
        let rig = rig(vec![Step::Serve(first), Step::Serve(second)]);
        let events = rig.events;
        let stop = StopSignal::new();
        let handle = spawn_supervisor("mini".to_string(), Some(a()), None, rig.deps, stop.clone());
        assert_eq!(connection(recv(&events)), ConnectionState::Connecting);
        assert_eq!(connection(recv(&events)), ConnectionState::Connected);
        probe_first.drop_now();
        assert_eq!(
            connection(recv(&events)),
            dropped("connection closed", None, 1000)
        );
        assert_eq!(
            connection(recv(&events)),
            ConnectionState::Reconnecting { attempt: 1 }
        );
        assert_eq!(connection(recv(&events)), ConnectionState::Connected);
        stop.stop();
        assert!(joins_within(handle, WAIT));
        assert!(probe_second.closed_by_client());
    }

    #[test]
    fn backoff_resets_after_handshake() {
        let double = Double::welcome(a());
        let probe = double.probe();
        let rig = rig(vec![
            transient("one"),
            transient("two"),
            Step::Serve(double),
        ]);
        let (clock, events) = (rig.clock.clone(), rig.events);
        let handle = spawn_supervisor(
            "mini".to_string(),
            Some(a()),
            None,
            rig.deps,
            StopSignal::new(),
        );
        loop {
            if connection(recv(&events)) == ConnectionState::Connected {
                break;
            }
        }
        probe.drop_now();
        assert!(joins_within(handle, WAIT));
        assert_eq!(
            clock.delays(),
            [
                Duration::from_millis(1000),
                Duration::from_millis(2000),
                Duration::from_millis(1000)
            ]
        );
        let tail = states(&events);
        assert_eq!(tail[0], dropped("connection closed", None, 1000));
        assert_eq!(
            tail.last(),
            Some(&failed("scripted connector exhausted", None))
        );
    }

    #[test]
    fn transient_failures_retry_with_capped_delay() {
        let steps = (0..7).map(|_| transient("cannot connect")).collect();
        let rig = rig(steps);
        run_to_end(&rig, None, None);
        let millis: Vec<u128> = rig.clock.delays().iter().map(Duration::as_millis).collect();
        assert_eq!(millis, [1000, 2000, 4000, 8000, 16000, 30000, 30000]);
        assert_eq!(rig.connector.connect_count(), 8);
        assert_eq!(
            states(&rig.events).last(),
            Some(&failed("scripted connector exhausted", None))
        );
    }

    #[test]
    fn mismatch_does_not_loop() {
        let rig = rig(vec![
            Step::Serve(Double::welcome(host_id(HOST_B))),
            Step::Serve(Double::welcome(host_id(HOST_B))),
        ]);
        run_to_end(&rig, Some(a()), None);
        assert_eq!(rig.connector.connect_count(), 1);
        assert!(rig.clock.delays().is_empty());
        assert!(matches!(
            states(&rig.events).last(),
            Some(ConnectionState::Failed { .. })
        ));
    }

    #[test]
    fn stop_ends_the_loop() {
        let double = Double::welcome(a());
        let probe = double.probe();
        let mut rig = rig(vec![Step::Serve(double)]);
        rig.deps.clock = Arc::new(SystemClock);
        let stop = StopSignal::new();
        let handle = spawn_supervisor("mini".to_string(), Some(a()), None, rig.deps, stop.clone());
        assert_eq!(connection(recv(&rig.events)), ConnectionState::Connecting);
        assert_eq!(connection(recv(&rig.events)), ConnectionState::Connected);
        stop.stop();
        assert!(joins_within(handle, WAIT));
        assert!(stop.is_stopped());
        assert!(probe.closed_by_client());
    }

    /// #82: macOS can lose the closer's wake-up of a session read that is just going to sleep,
    /// and `HostsService::stop` then joined the supervisor for ever. The socket here always
    /// loses it, and the test stops only once the session's read has blocked.
    #[test]
    fn stop_ends_a_session_whose_read_missed_the_wake_up() {
        let socket = LostWakeupSocket::new(welcome_frame(&a()));
        let link = Link::new(
            ticking_reader(socket.clone()).expect("set the read timeout"),
            Box::new(std::io::sink()),
            socket.closer(),
            Box::new(NoExit),
        );
        let rig = rig(vec![Step::Link(link)]);
        let stop = StopSignal::new();
        let handle = spawn_supervisor("mini".to_string(), Some(a()), None, rig.deps, stop.clone());
        assert_eq!(connection(recv(&rig.events)), ConnectionState::Connecting);
        assert_eq!(connection(recv(&rig.events)), ConnectionState::Connected);
        socket.wait_blocked(1);
        stop.stop();
        assert!(
            joins_within(handle, WAIT),
            "the supervisor did not stop within {WAIT:?}: its read slept through the closer"
        );
        assert_eq!(
            connection(recv(&rig.events)),
            dropped("connection closed", None, 1000)
        );
    }

    #[test]
    fn exhausted_script_ends_failed() {
        let rig = rig(Vec::new());
        run_to_end(&rig, None, None);
        assert_eq!(
            states(&rig.events),
            [
                ConnectionState::Connecting,
                failed("scripted connector exhausted", None)
            ]
        );
        assert_eq!(rig.connector.connect_count(), 1);
    }

    struct SpacesPhase;

    impl SessionPhase for SpacesPhase {
        fn run(&mut self, io: SessionIo<'_>) -> SessionEnd {
            io.events.spaces(io.host_id, Vec::new());
            SessionEnd::Io("reset".to_string())
        }
    }

    #[test]
    fn session_phase_publishes_spaces_and_its_end_becomes_the_drop() {
        let mut rig = rig_with(
            vec![Step::Serve(Double::welcome(a()))],
            MemoryHostIdStore::new(),
            FakeClock::stop_after(1),
        );
        rig.deps.phase = Arc::new(|| Box::new(SpacesPhase) as Box<dyn SessionPhase>);
        let rig = rig;
        run_to_end(&rig, Some(a()), None);
        let events: Vec<Event> = rig.events.try_iter().collect();
        assert!(events.contains(&Event::Spaces(a(), Vec::new())));
        assert_eq!(
            events.last(),
            Some(&Event::Connection(
                "mini".to_string(),
                dropped("connection lost", Some("reset"), 1000)
            ))
        );
    }

    #[test]
    fn stop_before_connect_closes_the_link_at_once() {
        let stop = StopSignal::new();
        stop.stop();
        let (closed, was_closed) = std::sync::mpsc::channel();
        stop.set_closer(Arc::new(move || {
            let _ = closed.send(());
        }));
        recv(&was_closed);
    }

    #[test]
    fn store_failure_message_names_the_store_error() {
        assert_eq!(
            store_failure_message(&StoreError::Invalid),
            "could not save the host ID: hosts.json is invalid, so Combine will not write it"
        );
    }

    #[test]
    fn already_recorded_with_the_same_id_publishes_it_and_continues() {
        let double = Double::welcome(a());
        let probe = double.probe();
        let store = MemoryHostIdStore::failing(StoreError::AlreadyRecorded(a()));
        let rig = rig_with(vec![Step::Serve(double)], store, FakeClock::stop_after(1));
        let events = rig.events;
        let handle = spawn_supervisor("mini".to_string(), None, None, rig.deps, StopSignal::new());
        assert_eq!(connection(recv(&events)), ConnectionState::Connecting);
        assert_eq!(recv(&events), Event::HostId("mini".to_string(), a()));
        assert_eq!(connection(recv(&events)), ConnectionState::Connected);
        probe.drop_now();
        assert_eq!(
            connection(recv(&events)),
            dropped("connection closed", None, 1000)
        );
        assert!(joins_within(handle, WAIT));
    }

    #[test]
    fn already_recorded_with_another_id_is_refused_as_a_mismatch() {
        let double = Double::welcome(a());
        let probe = double.probe();
        let store = MemoryHostIdStore::failing(StoreError::AlreadyRecorded(host_id(HOST_B)));
        let rig = rig_with(vec![Step::Serve(double)], store, FakeClock::new());
        run_to_end(&rig, None, None);
        let mismatch = super::super::identity::mismatch_message("mini", &host_id(HOST_B), &a());
        let events: Vec<Event> = rig.events.try_iter().collect();
        assert_eq!(
            events.last(),
            Some(&Event::Connection(
                "mini".to_string(),
                failed(&mismatch, None)
            ))
        );
        assert!(events.iter().all(|e| !matches!(e, Event::HostId(..))));
        assert_eq!(rig.connector.connect_count(), 1);
        assert!(probe.closed_by_client());
    }
}
