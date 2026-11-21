//! The driver, run against a stub backend that lives in this file.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use combine_protocol::{
    Attach, AttachmentId, BackendGeneration, ClientMessage, CloseReason, DaemonMessage, Detach,
    Display, ErrorCode, Fidelity, Input, RequestId, RequestOwnership, TermId,
};
use combined::core::{BackendCommand, ConnectionId, Core, CoreConfig, Driver};
use combined::model::{Millis, SpawnSpec, TermEvent, TermInfo};
use combined::ports::{
    AttachEvent, AttachEvents, AttachOptions, Attachment, BackendError, BackendResult,
    Capabilities, TerminalBackend,
};
use common::{display, request_options};
use futures::channel::{mpsc, oneshot};
use futures::executor::block_on;
use futures::stream::{BoxStream, StreamExt};
use futures::task::{waker, ArcWake};

type Sender = mpsc::UnboundedSender<(BackendGeneration, AttachEvent)>;

fn caps() -> Capabilities {
    Capabilities {
        ordered_attach: true,
        byte_credit: false,
        initial_credit: 0,
        display_queries: true,
        automatic_replies: true,
        resync_signal: true,
        agent_state: false,
    }
}

#[derive(Default)]
struct Stub {
    log: Arc<Mutex<Vec<String>>>,
    senders: Arc<Mutex<BTreeMap<String, Sender>>>,
    gate: Mutex<Option<oneshot::Receiver<()>>>,
}

impl Stub {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// The next attach waits until the returned sender fires.
    fn gate_next_attach(&self) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        *self.gate.lock().unwrap() = Some(rx);
        tx
    }

    fn emit(&self, terminal: &str, generation: u64, event: AttachEvent) {
        let senders = self.senders.lock().unwrap();
        senders[terminal]
            .unbounded_send((BackendGeneration(generation), event))
            .unwrap();
    }

    fn end_stream(&self, terminal: &str) {
        self.senders.lock().unwrap().remove(terminal);
    }

    fn start(&self, terminal: &str) {
        self.emit(
            terminal,
            1,
            AttachEvent::Started {
                cols: 80,
                rows: 24,
                fidelity: Fidelity::Exact,
                ordered_resizes: true,
            },
        );
    }
}

struct StubAttachment {
    name: String,
    log: Arc<Mutex<Vec<String>>>,
    events: Option<mpsc::UnboundedReceiver<(BackendGeneration, AttachEvent)>>,
}

impl StubAttachment {
    fn note(&self, what: String) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{what} {}", self.name));
    }
}

#[async_trait]
impl TerminalBackend for Stub {
    fn id(&self) -> &'static str {
        "stub"
    }

    fn capabilities(&self) -> Capabilities {
        caps()
    }

    async fn list(&self) -> BackendResult<Vec<TermInfo>> {
        Ok(Vec::new())
    }

    async fn watch(&self) -> BackendResult<BoxStream<'static, TermEvent>> {
        Ok(futures::stream::empty().boxed())
    }

    async fn spawn(&self, _spec: SpawnSpec) -> BackendResult<TermId> {
        Err(BackendError::Failed("not in this stub".into()))
    }

    async fn attach(
        &self,
        id: &TermId,
        _opts: AttachOptions,
    ) -> BackendResult<Box<dyn Attachment>> {
        if id.0 == "fake:missing" {
            return Err(BackendError::UnknownTerminal(id.clone()));
        }
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.await;
        }
        let (tx, rx) = mpsc::unbounded();
        self.senders.lock().unwrap().insert(id.0.clone(), tx);
        Ok(Box::new(StubAttachment {
            name: id.0.clone(),
            log: Arc::clone(&self.log),
            events: Some(rx),
        }))
    }

    async fn kill(&self, _id: &TermId) -> BackendResult<()> {
        Ok(())
    }
}

#[async_trait]
impl Attachment for StubAttachment {
    fn fidelity(&self) -> Fidelity {
        Fidelity::Exact
    }

    fn ordered_resizes(&self) -> bool {
        true
    }

    fn events(&mut self) -> AttachEvents {
        match self.events.take() {
            Some(rx) => rx.boxed(),
            None => futures::stream::empty().boxed(),
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> BackendResult<()> {
        self.note(format!("write {}", String::from_utf8_lossy(bytes)));
        Ok(())
    }

    async fn write_automatic(&mut self, bytes: &[u8]) -> BackendResult<()> {
        self.note(format!("auto {}", String::from_utf8_lossy(bytes)));
        Ok(())
    }

    async fn resize(&mut self, cols: u16, rows: u16) -> BackendResult<()> {
        self.note(format!("resize {cols}x{rows}"));
        Ok(())
    }

    async fn focus(&mut self) -> BackendResult<()> {
        self.note("focus".to_string());
        Ok(())
    }

    async fn set_display(&mut self, _display: Display) -> BackendResult<()> {
        self.note("set_display".to_string());
        Ok(())
    }

    async fn credit(&mut self, bytes: u32) -> BackendResult<()> {
        self.note(format!("credit {bytes}"));
        Ok(())
    }

    async fn detach(self: Box<Self>) -> BackendResult<()> {
        self.note("detach".to_string());
        Ok(())
    }
}

const NOW: Millis = Millis(0);

struct Env {
    driver: Driver,
    stub: Arc<Stub>,
    conn: ConnectionId,
}

impl Env {
    fn new() -> Env {
        Env::with(CoreConfig::default())
    }

    fn with(config: CoreConfig) -> Env {
        let stub = Arc::new(Stub::default());
        let backend: Arc<dyn TerminalBackend> = stub.clone();
        let mut driver = Driver::new(Core::new(config, caps()).unwrap(), backend);
        let conn = driver.core_mut().connect();
        Env { driver, stub, conn }
    }

    fn send(&mut self, message: ClientMessage) {
        self.driver
            .core_mut()
            .client_message(self.conn, message, NOW);
    }

    fn run(&mut self) {
        block_on(self.driver.run_effects(NOW));
    }

    fn pull(&mut self) -> Vec<DaemonMessage> {
        self.driver.core_mut().pull(self.conn, usize::MAX, NOW)
    }

    fn attach_message(&self, request: u64, terminal: &str) -> ClientMessage {
        ClientMessage::Attach(Attach {
            request: RequestId(request),
            terminal: TermId(terminal.to_string()),
            options: request_options(80, 24, false),
        })
    }

    /// Attaches to `fake:<n>` and returns the ID once `attached` has come back.
    fn attach(&mut self, n: u32) -> AttachmentId {
        let message = self.attach_message(n as u64, &format!("fake:{n}"));
        self.send(message);
        self.run();
        let messages = self.pull();
        match messages.first() {
            Some(DaemonMessage::Attached(attached)) => attached.attachment,
            other => panic!("not attached: {other:?}"),
        }
    }

    fn poll(&mut self) -> Poll<()> {
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        self.driver.poll_event(&mut cx, NOW)
    }
}

fn type_names(messages: &[DaemonMessage]) -> Vec<&'static str> {
    messages.iter().map(|m| m.type_name()).collect()
}

#[test]
fn driver_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Driver>();
}

#[test]
fn run_effects_completes_a_ready_attach_and_journals_it() {
    let mut env = Env::new();
    env.driver.record_commands(true);
    let message = env.attach_message(7, "fake:1");
    env.send(message);
    env.run();
    let journal = env.driver.take_journal();
    let [BackendCommand::Attach {
        attachment,
        terminal,
        options,
    }] = journal.as_slice()
    else {
        panic!("{journal:?}");
    };
    assert_eq!(terminal.0, "fake:1");
    assert_eq!(options.display, display());
    assert!(env.driver.attachment(*attachment).is_some());
    let messages = env.pull();
    assert_eq!(type_names(&messages), ["attached", "ownership"]);
    assert!(env.driver.take_journal().is_empty());
    assert!(env.driver.core_mut().take_effects().is_empty());
}

#[test]
fn an_attach_is_not_awaited_inline() {
    let mut env = Env::new();
    let release = env.stub.gate_next_attach();
    let message = env.attach_message(1, "fake:1");
    env.send(message);
    env.run();
    assert!(env.pull().is_empty());
    assert!(env.poll().is_pending());

    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    let messages = env.pull();
    assert_eq!(type_names(&messages), ["attached", "ownership"]);
    let DaemonMessage::Attached(attached) = &messages[0] else {
        panic!()
    };
    assert!(env.driver.attachment(attached.attachment).is_some());
}

#[test]
fn a_failed_attach_reaches_core() {
    let mut env = Env::new();
    let message = env.attach_message(3, "fake:missing");
    env.send(message);
    env.run();
    let messages = env.pull();
    let [DaemonMessage::Error(error)] = messages.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(error.code, ErrorCode::UnknownTerminal);
    assert_eq!(error.request, Some(RequestId(3)));
}

#[test]
fn detach_removes_the_attachment_and_ignores_unknown_ids() {
    let mut env = Env::new();
    let id = env.attach(1);
    env.send(ClientMessage::Detach(Detach { attachment: id }));
    env.run();
    assert!(env.driver.attachment(id).is_none());
    assert_eq!(env.stub.log(), ["detach fake:1"]);

    env.driver
        .core_mut()
        .attach_result(AttachmentId(999), Ok(()), NOW);
    env.driver.record_commands(true);
    env.run();
    assert_eq!(
        env.driver.take_journal(),
        [BackendCommand::Detach {
            attachment: AttachmentId(999)
        }]
    );
    assert_eq!(env.stub.log(), ["detach fake:1"]);
}

#[test]
fn commands_run_in_order_on_the_stored_attachment() {
    let mut env = Env::new();
    let id = env.attach(1);
    env.send(ClientMessage::Input(Input {
        attachment: id,
        bytes: b"ls".to_vec(),
    }));
    env.run();
    assert_eq!(
        env.stub.log(),
        [
            "set_display fake:1",
            "resize 80x24 fake:1",
            "focus fake:1",
            "write ls fake:1"
        ]
    );
}

#[test]
fn recording_can_be_switched_off() {
    let mut env = Env::new();
    env.driver.record_commands(true);
    env.attach(1);
    assert_eq!(env.driver.take_journal().len(), 1);
    env.driver.record_commands(false);
    env.attach(2);
    assert!(env.driver.take_journal().is_empty());
}

#[test]
fn disconnects_are_queued_for_the_caller() {
    let mut env = Env::with(CoreConfig {
        max_control_messages: 1,
        ..CoreConfig::default()
    });
    let bad = ClientMessage::Attach(Attach {
        request: RequestId(1),
        terminal: TermId("fake:1".into()),
        options: request_options(0, 0, false),
    });
    env.send(bad.clone());
    env.send(bad);
    env.run();
    assert_eq!(env.driver.take_disconnects(), [env.conn]);
    assert!(env.driver.take_disconnects().is_empty());
}

#[test]
fn poll_event_round_robins_the_streams_in_id_order() {
    let mut env = Env::new();
    let ids = [env.attach(1), env.attach(2), env.attach(3)];
    for n in 1..=3 {
        env.stub.start(&format!("fake:{n}"));
    }
    for n in 1..=3 {
        env.stub.emit(
            &format!("fake:{n}"),
            1,
            AttachEvent::Resized { cols: 5, rows: 5 },
        );
    }
    let mut served = Vec::new();
    for _ in 0..6 {
        assert!(env.poll().is_ready());
        for message in env.pull() {
            served.push(message.stream_attachment().unwrap());
        }
    }
    assert_eq!(served, [ids[0], ids[1], ids[2], ids[0], ids[1], ids[2]]);
    assert!(env.poll().is_pending());
}

#[test]
fn an_ended_stream_becomes_lost() {
    let mut env = Env::new();
    let [a, b] = [env.attach(1), env.attach(2)];
    env.stub.start("fake:1");
    env.stub.start("fake:2");
    assert_eq!(block_on(env.driver.pump_ready(NOW)), 2);
    env.pull();
    env.stub.end_stream("fake:2");
    assert!(env.poll().is_ready());
    env.run();
    let messages = env.pull();
    assert_eq!(type_names(&messages), ["lost"]);
    assert_eq!(messages[0].stream_attachment(), Some(b));
    assert!(env.driver.attachment(b).is_none());
    assert!(env.driver.attachment(a).is_some());
    assert!(env.poll().is_pending());
    assert_eq!(env.stub.log(), ["detach fake:2"]);
}

#[test]
fn pending_attaches_are_polled_before_streams() {
    let mut env = Env::new();
    env.attach(1);
    env.stub.start("fake:1");
    let release = env.stub.gate_next_attach();
    let message = env.attach_message(2, "fake:2");
    env.send(message);
    env.run();
    release.send(()).unwrap();

    assert!(env.poll().is_ready());
    let first = env.pull();
    assert_eq!(type_names(&first), ["attached", "ownership"]);
    assert!(env.poll().is_ready());
    assert_eq!(type_names(&env.pull()), ["started"]);
}

struct Flag(AtomicBool);

impl ArcWake for Flag {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn a_pending_poll_registers_the_callers_waker() {
    let mut env = Env::new();
    env.attach(1);
    let flag = Arc::new(Flag(AtomicBool::new(false)));
    let waker = waker(Arc::clone(&flag));
    let mut cx = Context::from_waker(&waker);
    assert!(env.driver.poll_event(&mut cx, NOW).is_pending());
    assert!(!flag.0.load(Ordering::SeqCst));
    env.stub.start("fake:1");
    assert!(flag.0.load(Ordering::SeqCst));
    assert!(env.driver.poll_event(&mut cx, NOW).is_ready());
}

#[test]
fn pump_ready_drains_what_is_ready_and_runs_effects() {
    let mut env = Env::new();
    env.attach(1);
    env.attach(2);
    env.stub.start("fake:1");
    env.stub.start("fake:2");
    env.stub
        .emit("fake:1", 1, AttachEvent::Closed(CloseReason::Exited));
    assert_eq!(block_on(env.driver.pump_ready(NOW)), 3);
    assert_eq!(block_on(env.driver.pump_ready(NOW)), 0);
    assert_eq!(env.stub.log(), ["detach fake:1"]);
}

#[test]
fn requesting_ownership_runs_the_grant_commands() {
    let mut env = Env::new();
    let id = env.attach(1);
    env.send(ClientMessage::RequestOwnership(RequestOwnership {
        attachment: id,
    }));
    env.run();
    assert_eq!(
        env.stub.log(),
        ["set_display fake:1", "resize 80x24 fake:1", "focus fake:1"]
    );
}
