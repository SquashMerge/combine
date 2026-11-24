//! The driver, run against a stub backend that lives in this file.

mod common;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use combine_protocol::{
    Ack, Attach, AttachmentId, AutomaticReply, BackendGeneration, ClientMessage, CloseReason,
    Cursor, DaemonMessage, Detach, Display, Epoch, ErrorCode, Fidelity, Focus, Input,
    OwnershipGeneration, RequestId, RequestOwnership, Resize, SetDisplay, TermId,
};
use combined::core::{BackendCommand, ConnectionId, Core, CoreConfig, Driver, MAX_QUEUED_COMMANDS};
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

/// A call that waits until `release` fires, then fails if `fail` is set.
struct Hold {
    terminal: String,
    method: &'static str,
    release: oneshot::Receiver<()>,
    fail: bool,
}

#[derive(Default)]
struct Stub {
    log: Arc<Mutex<Vec<String>>>,
    senders: Arc<Mutex<BTreeMap<String, Sender>>>,
    gate: Mutex<Option<oneshot::Receiver<()>>>,
    holds: Arc<Mutex<Vec<Hold>>>,
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

    /// The next `method` call on `terminal` waits until the returned sender fires.
    fn hold_next(&self, terminal: &str, method: &'static str, fail: bool) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        self.holds.lock().unwrap().push(Hold {
            terminal: terminal.to_string(),
            method,
            release: rx,
            fail,
        });
        tx
    }

    /// The log entries for `terminal`.
    fn log_of(&self, terminal: &str) -> Vec<String> {
        let suffix = format!(" {terminal}");
        self.log()
            .into_iter()
            .filter(|entry| entry.ends_with(&suffix))
            .collect()
    }

    fn stream_closed(&self, terminal: &str) -> bool {
        self.senders.lock().unwrap()[terminal].is_closed()
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
    holds: Arc<Mutex<Vec<Hold>>>,
    events: Option<mpsc::UnboundedReceiver<(BackendGeneration, AttachEvent)>>,
}

impl StubAttachment {
    fn note(&self, what: String) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{what} {}", self.name));
    }

    /// Removes the first hold on this terminal's `method`.
    fn take_hold(&self, method: &str) -> Option<Hold> {
        let mut holds = self.holds.lock().unwrap();
        let index = holds
            .iter()
            .position(|hold| hold.terminal == self.name && hold.method == method)?;
        Some(holds.remove(index))
    }
}

/// Owns the hold, so the method futures stay `Send`.
async fn wait_hold(hold: Option<Hold>) -> BackendResult<()> {
    let Some(hold) = hold else {
        return Ok(());
    };
    let _ = hold.release.await;
    if hold.fail {
        Err(BackendError::Failed("held call failed".into()))
    } else {
        Ok(())
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
        // A second open attachment on the same terminal is named `<terminal>#<n>`.
        let name = {
            let mut senders = self.senders.lock().unwrap();
            let mut name = id.0.clone();
            let mut n = 1;
            while senders.get(&name).is_some_and(|sender| !sender.is_closed()) {
                n += 1;
                name = format!("{}#{n}", id.0);
            }
            senders.insert(name.clone(), tx);
            name
        };
        Ok(Box::new(StubAttachment {
            name,
            log: Arc::clone(&self.log),
            holds: Arc::clone(&self.holds),
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

    async fn write(&mut self, _ownership: OwnershipGeneration, bytes: &[u8]) -> BackendResult<()> {
        let result = wait_hold(self.take_hold("write")).await;
        self.note(format!("write {}", String::from_utf8_lossy(bytes)));
        result
    }

    async fn write_automatic(
        &mut self,
        _generation: BackendGeneration,
        bytes: &[u8],
    ) -> BackendResult<()> {
        let result = wait_hold(self.take_hold("write_automatic")).await;
        self.note(format!("auto {}", String::from_utf8_lossy(bytes)));
        result
    }

    async fn resize(
        &mut self,
        _ownership: OwnershipGeneration,
        cols: u16,
        rows: u16,
    ) -> BackendResult<()> {
        let result = wait_hold(self.take_hold("resize")).await;
        self.note(format!("resize {cols}x{rows}"));
        result
    }

    async fn focus(&mut self, _ownership: OwnershipGeneration) -> BackendResult<()> {
        let result = wait_hold(self.take_hold("focus")).await;
        self.note("focus".to_string());
        result
    }

    async fn set_display(
        &mut self,
        _ownership: OwnershipGeneration,
        _display: Display,
    ) -> BackendResult<()> {
        let result = wait_hold(self.take_hold("set_display")).await;
        self.note("set_display".to_string());
        result
    }

    async fn credit(&mut self, bytes: u32) -> BackendResult<()> {
        let result = wait_hold(self.take_hold("credit")).await;
        self.note(format!("credit {bytes}"));
        result
    }

    async fn detach(self: Box<Self>) -> BackendResult<()> {
        let result = wait_hold(self.take_hold("detach")).await;
        self.note("detach".to_string());
        result
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
        Env::with_caps(config, caps())
    }

    fn with_caps(config: CoreConfig, capabilities: Capabilities) -> Env {
        let stub = Arc::new(Stub::default());
        let backend: Arc<dyn TerminalBackend> = stub.clone();
        let mut driver = Driver::new(Core::new(config, capabilities).unwrap(), backend);
        let conn = driver.core_mut().connect();
        Env { driver, stub, conn }
    }

    fn send(&mut self, message: ClientMessage) {
        self.driver
            .core_mut()
            .client_message(self.conn, message, NOW);
    }

    /// Runs core's effects with one poll, and fails if the driver waits on a backend call.
    fn run(&mut self) {
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let mut effects = pin!(self.driver.run_effects(NOW));
        if effects.as_mut().poll(&mut cx).is_pending() {
            panic!("the driver waited on a backend call");
        }
    }

    /// Handles what is ready now with one poll, and returns how many events that took.
    fn pump(&mut self) -> usize {
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let mut pumped = pin!(self.driver.pump_ready(NOW));
        match pumped.as_mut().poll(&mut cx) {
            Poll::Ready(handled) => handled,
            Poll::Pending => panic!("the driver waited on a backend call"),
        }
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

fn credit_env() -> Env {
    Env::with_caps(
        CoreConfig {
            credit_min_grant: 1,
            ..CoreConfig::default()
        },
        Capabilities {
            byte_credit: true,
            initial_credit: 1_024,
            ..caps()
        },
    )
}

struct Live {
    id: AttachmentId,
    epoch: Epoch,
    ownership: OwnershipGeneration,
}

/// Attaches to `fake:<n>`, lets it start and makes it the terminal's owner.
fn live_owner(env: &mut Env, n: u32) -> Live {
    let id = env.attach(n);
    env.stub.start(&format!("fake:{n}"));
    env.pump();
    let epoch = env
        .pull()
        .iter()
        .find_map(|message| match message {
            DaemonMessage::Started(started) => Some(started.cursor.epoch),
            _ => None,
        })
        .unwrap();
    env.send(ClientMessage::RequestOwnership(RequestOwnership {
        attachment: id,
    }));
    env.run();
    env.pull();
    let ownership = env
        .driver
        .core()
        .ownership(&TermId(format!("fake:{n}")))
        .unwrap();
    assert_eq!(ownership.owner, Some(id));
    Live {
        id,
        epoch,
        ownership: ownership.generation,
    }
}

/// Emits `bytes` on `fake:<n>` and returns the cursor core sent them under.
fn data_cursor(env: &mut Env, n: u32, live: &Live, bytes: &[u8]) -> Cursor {
    env.stub
        .emit(&format!("fake:{n}"), 1, AttachEvent::Data(bytes.to_vec()));
    env.pump();
    let cursor = env
        .pull()
        .iter()
        .find_map(|message| match message {
            DaemonMessage::Data(data) => Some(data.cursor),
            _ => None,
        })
        .unwrap();
    assert_eq!(cursor.attachment, live.id);
    cursor
}

#[derive(Clone, Copy)]
enum Call {
    Write,
    WriteAutomatic,
    Resize,
    Focus,
    SetDisplay,
    Credit,
    Detach,
}

impl Call {
    fn method(self) -> &'static str {
        match self {
            Call::Write => "write",
            Call::WriteAutomatic => "write_automatic",
            Call::Resize => "resize",
            Call::Focus => "focus",
            Call::SetDisplay => "set_display",
            Call::Credit => "credit",
            Call::Detach => "detach",
        }
    }
}

/// Sends the message that makes core issue exactly `call` on `fake:1`, and runs it.
fn issue(env: &mut Env, live: &Live, call: Call) {
    let attachment = live.id;
    let message = match call {
        Call::Write => ClientMessage::Input(Input {
            attachment,
            bytes: b"held".to_vec(),
        }),
        Call::WriteAutomatic => ClientMessage::AutomaticReply(AutomaticReply {
            attachment,
            epoch: live.epoch,
            ownership_generation: live.ownership,
            bytes: b"held".to_vec(),
        }),
        Call::Resize => ClientMessage::Resize(Resize {
            attachment,
            cols: 100,
            rows: 30,
            ownership_generation: live.ownership,
        }),
        Call::Focus => ClientMessage::Focus(Focus { attachment }),
        Call::SetDisplay => ClientMessage::SetDisplay(SetDisplay {
            attachment,
            display: display(),
        }),
        Call::Credit => {
            let cursor = data_cursor(env, 1, live, b"held");
            ClientMessage::Ack(Ack { cursor })
        }
        Call::Detach => ClientMessage::Detach(Detach { attachment }),
    };
    env.send(message);
    env.run();
}

fn errors(messages: &[DaemonMessage]) -> Vec<&combine_protocol::ErrorMessage> {
    messages
        .iter()
        .filter_map(|message| match message {
            DaemonMessage::Error(error) => Some(error),
            _ => None,
        })
        .collect()
}

fn since(env: &Env, terminal: &str, base: usize) -> Vec<String> {
    env.stub.log_of(terminal)[base..].to_vec()
}

fn holds_back_only_its_attachment(call: Call) {
    let mut env = credit_env();
    let a = live_owner(&mut env, 1);
    let b = live_owner(&mut env, 2);
    let release = env.stub.hold_next("fake:1", call.method(), false);
    env.driver.record_commands(true);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, call);
    let journal = env.driver.take_journal();
    let [command] = journal.as_slice() else {
        panic!("{journal:?}");
    };
    assert_eq!(command.attachment(), a.id);
    let note = match (call, command) {
        (Call::Write, BackendCommand::Write { .. }) => "write held".to_string(),
        (Call::WriteAutomatic, BackendCommand::WriteAutomatic { .. }) => "auto held".to_string(),
        (Call::Resize, BackendCommand::Resize { .. }) => "resize 100x30".to_string(),
        (Call::Focus, BackendCommand::Focus { .. }) => "focus".to_string(),
        (Call::SetDisplay, BackendCommand::SetDisplay { .. }) => "set_display".to_string(),
        (Call::Credit, BackendCommand::Credit { bytes, .. }) => format!("credit {bytes}"),
        (Call::Detach, BackendCommand::Detach { .. }) => "detach".to_string(),
        _ => panic!("{command:?}"),
    };

    assert!(env.driver.attachment(a.id).is_none());
    assert!(since(&env, "fake:1", base).is_empty());

    env.send(ClientMessage::Input(Input {
        attachment: b.id,
        bytes: b"other".to_vec(),
    }));
    env.run();
    assert_eq!(
        env.stub.log_of("fake:2").last().map(String::as_str),
        Some("write other fake:2")
    );
    assert!(env.driver.attachment(b.id).is_some());
    let mut pulled = env.pull();

    let c = env.attach(3);
    assert!(env.driver.attachment(c).is_some());

    env.stub
        .emit("fake:2", 1, AttachEvent::Resized { cols: 5, rows: 5 });
    assert!(env.pump() >= 1);
    let messages = env.pull();
    assert!(messages
        .iter()
        .any(|m| matches!(m, DaemonMessage::Resized(_)) && m.stream_attachment() == Some(b.id)));
    pulled.extend(messages);

    if !matches!(call, Call::Detach) {
        env.send(ClientMessage::Input(Input {
            attachment: a.id,
            bytes: b"after".to_vec(),
        }));
        env.run();
        assert!(since(&env, "fake:1", base).is_empty());
    }

    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    let expected = if matches!(call, Call::Detach) {
        vec![format!("{note} fake:1")]
    } else {
        vec![format!("{note} fake:1"), "write after fake:1".to_string()]
    };
    assert_eq!(since(&env, "fake:1", base), expected);
    if matches!(call, Call::Detach) {
        assert!(env.driver.attachment(a.id).is_none());
        assert!(env.stub.stream_closed("fake:1"));
    } else {
        assert!(env.driver.attachment(a.id).is_some());
    }
    pulled.extend(env.pull());
    assert!(errors(&pulled).is_empty(), "{pulled:?}");
}

#[test]
fn a_write_in_flight_holds_back_only_its_attachment() {
    holds_back_only_its_attachment(Call::Write);
}

#[test]
fn a_write_automatic_in_flight_holds_back_only_its_attachment() {
    holds_back_only_its_attachment(Call::WriteAutomatic);
}

#[test]
fn a_resize_in_flight_holds_back_only_its_attachment() {
    holds_back_only_its_attachment(Call::Resize);
}

#[test]
fn a_focus_in_flight_holds_back_only_its_attachment() {
    holds_back_only_its_attachment(Call::Focus);
}

#[test]
fn a_set_display_in_flight_holds_back_only_its_attachment() {
    holds_back_only_its_attachment(Call::SetDisplay);
}

#[test]
fn a_credit_in_flight_holds_back_only_its_attachment() {
    holds_back_only_its_attachment(Call::Credit);
}

#[test]
fn a_detach_in_flight_holds_back_only_its_attachment() {
    holds_back_only_its_attachment(Call::Detach);
}

fn input(live: &Live, bytes: &str) -> ClientMessage {
    ClientMessage::Input(Input {
        attachment: live.id,
        bytes: bytes.as_bytes().to_vec(),
    })
}

fn credit_bytes(command: &BackendCommand) -> u32 {
    match command {
        BackendCommand::Credit { bytes, .. } => *bytes,
        other => panic!("{other:?}"),
    }
}

#[test]
fn queued_calls_run_in_issue_order_once_the_call_in_flight_completes() {
    let mut env = credit_env();
    let a = live_owner(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    let cursor = data_cursor(&mut env, 1, &a, b"held");
    env.driver.record_commands(true);

    env.send(input(&a, "w1"));
    env.send(ClientMessage::AutomaticReply(AutomaticReply {
        attachment: a.id,
        epoch: a.epoch,
        ownership_generation: a.ownership,
        bytes: b"a1".to_vec(),
    }));
    env.send(ClientMessage::Resize(Resize {
        attachment: a.id,
        cols: 100,
        rows: 30,
        ownership_generation: a.ownership,
    }));
    env.send(ClientMessage::Focus(Focus { attachment: a.id }));
    env.send(ClientMessage::SetDisplay(SetDisplay {
        attachment: a.id,
        display: display(),
    }));
    env.send(ClientMessage::Ack(Ack { cursor }));
    env.send(input(&a, "w2"));
    env.send(ClientMessage::Detach(Detach { attachment: a.id }));
    env.run();

    let journal = env.driver.take_journal();
    assert!(matches!(
        journal.as_slice(),
        [
            BackendCommand::Write { .. },
            BackendCommand::WriteAutomatic { .. },
            BackendCommand::Resize { .. },
            BackendCommand::Focus { .. },
            BackendCommand::SetDisplay { .. },
            BackendCommand::Credit { .. },
            BackendCommand::Write { .. },
            BackendCommand::Detach { .. },
        ]
    ));
    let n = credit_bytes(&journal[5]);
    assert_eq!(since(&env, "fake:1", base), Vec::<String>::new());
    assert!(env.stub.stream_closed("fake:1"));

    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    // The reply is not written: core has handled the detach by the time it would start, so it
    // is no longer the live owner's.
    let expected: Vec<String> = [
        "write held".to_string(),
        "write w1".to_string(),
        "resize 100x30".to_string(),
        "focus".to_string(),
        "set_display".to_string(),
        format!("credit {n}"),
        "write w2".to_string(),
        "detach".to_string(),
    ]
    .iter()
    .map(|entry| format!("{entry} fake:1"))
    .collect();
    assert_eq!(since(&env, "fake:1", base), expected);
}

#[test]
fn at_the_queue_bound_client_commands_are_refused_and_automatic_replies_dropped() {
    let mut env = credit_env();
    let a = live_owner(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    let cursor = data_cursor(&mut env, 1, &a, b"held");
    env.driver.record_commands(true);

    for n in 0..MAX_QUEUED_COMMANDS {
        env.send(input(&a, &format!("q{n}")));
    }
    env.run();
    assert!(errors(&env.pull()).is_empty());

    env.send(input(&a, "over"));
    env.send(ClientMessage::Resize(Resize {
        attachment: a.id,
        cols: 90,
        rows: 20,
        ownership_generation: a.ownership,
    }));
    env.send(ClientMessage::Focus(Focus { attachment: a.id }));
    env.send(ClientMessage::SetDisplay(SetDisplay {
        attachment: a.id,
        display: display(),
    }));
    env.send(ClientMessage::AutomaticReply(AutomaticReply {
        attachment: a.id,
        epoch: a.epoch,
        ownership_generation: a.ownership,
        bytes: b"auto-over".to_vec(),
    }));
    env.run();
    let pulled = env.pull();
    let refused = errors(&pulled);
    assert_eq!(refused.len(), 4, "{pulled:?}");
    for error in &refused {
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(error.attachment, Some(a.id));
        assert_eq!(error.request, None);
    }
    let messages: Vec<&str> = refused.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "the backend refused input for this attachment",
            "the backend refused the resize for this attachment",
            "the backend refused the focus for this attachment",
            "the backend refused the display change for this attachment",
        ]
    );

    env.send(ClientMessage::Ack(Ack { cursor }));
    env.send(ClientMessage::Detach(Detach { attachment: a.id }));
    env.run();
    assert!(errors(&env.pull()).is_empty());
    let journal = env.driver.take_journal();
    let n = journal
        .iter()
        .find_map(|command| match command {
            BackendCommand::Credit { bytes, .. } => Some(*bytes),
            _ => None,
        })
        .unwrap();

    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    let mut expected = vec!["write held fake:1".to_string()];
    expected.extend((0..MAX_QUEUED_COMMANDS).map(|n| format!("write q{n} fake:1")));
    expected.push(format!("credit {n} fake:1"));
    expected.push("detach fake:1".to_string());
    assert_eq!(since(&env, "fake:1", base), expected);
}

#[test]
fn credit_queued_behind_a_call_in_flight_is_summed_at_the_tail() {
    let mut env = credit_env();
    let a = live_owner(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    let first = data_cursor(&mut env, 1, &a, b"aaaa");
    let second = data_cursor(&mut env, 1, &a, b"bbbbbb");
    env.driver.record_commands(true);
    env.send(ClientMessage::Ack(Ack { cursor: first }));
    env.send(ClientMessage::Ack(Ack { cursor: second }));
    env.run();
    let journal = env.driver.take_journal();
    let [one, two] = journal.as_slice() else {
        panic!("{journal:?}");
    };
    let (n1, n2) = (credit_bytes(one), credit_bytes(two));
    assert!(n1 > 0 && n2 > 0);
    assert!(since(&env, "fake:1", base).is_empty());

    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    assert_eq!(
        since(&env, "fake:1", base),
        [
            "write held fake:1".to_string(),
            format!("credit {} fake:1", n1.saturating_add(n2)),
        ]
    );
}

#[test]
fn detach_during_a_call_in_flight_drops_events_at_once_and_runs_after_it() {
    let mut env = Env::new();
    let a = live_owner(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", true);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    env.send(ClientMessage::Detach(Detach { attachment: a.id }));
    env.run();
    assert!(env.stub.stream_closed("fake:1"));
    assert!(since(&env, "fake:1", base).is_empty());
    assert!(env.driver.attachment(a.id).is_none());
    let mut pulled = env.pull();

    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    assert_eq!(
        since(&env, "fake:1", base),
        ["write held fake:1", "detach fake:1"]
    );
    pulled.extend(env.pull());
    assert!(errors(&pulled).is_empty(), "{pulled:?}");
    assert!(env.poll().is_pending());
    assert!(env.driver.attachment(a.id).is_none());
}

#[test]
fn a_call_that_fails_in_flight_is_refused_when_it_completes() {
    let mut env = Env::new();
    let a = live_owner(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", true);
    issue(&mut env, &a, Call::Write);
    assert!(errors(&env.pull()).is_empty());

    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    let pulled = env.pull();
    let refused = errors(&pulled);
    assert_eq!(refused.len(), 1, "{pulled:?}");
    assert_eq!(refused[0].code, ErrorCode::Internal);
    assert_eq!(refused[0].attachment, Some(a.id));
    assert_eq!(
        refused[0].message,
        "the backend refused input for this attachment"
    );
    assert!(env.driver.attachment(a.id).is_some());
}

/// Attaches a second viewer to `fake:<n>`, named `fake:<n>#2` in the stub, and lets it start.
fn second_viewer(env: &mut Env, n: u32) -> AttachmentId {
    let message = env.attach_message(100 + n as u64, &format!("fake:{n}"));
    env.send(message);
    env.run();
    let id = env
        .pull()
        .iter()
        .find_map(|message| match message {
            DaemonMessage::Attached(attached) => Some(attached.attachment),
            _ => None,
        })
        .unwrap();
    env.stub.start(&format!("fake:{n}#2"));
    env.pump();
    env.pull();
    id
}

fn reply(live: &Live, bytes: &str) -> ClientMessage {
    ClientMessage::AutomaticReply(AutomaticReply {
        attachment: live.id,
        epoch: live.epoch,
        ownership_generation: live.ownership,
        bytes: bytes.as_bytes().to_vec(),
    })
}

fn take_ownership(env: &mut Env, attachment: AttachmentId) -> OwnershipGeneration {
    env.send(ClientMessage::RequestOwnership(RequestOwnership {
        attachment,
    }));
    env.run();
    let ownership = env
        .driver
        .core()
        .ownership(&TermId("fake:1".into()))
        .unwrap();
    assert_eq!(ownership.owner, Some(attachment));
    ownership.generation
}

/// Releases the call in flight on `fake:1` and runs what was queued behind it.
fn release_and_drain(env: &mut Env, release: oneshot::Sender<()>) {
    release.send(()).unwrap();
    assert!(env.poll().is_ready());
    env.run();
    env.pump();
}

fn entries(list: &[&str], terminal: &str) -> Vec<String> {
    list.iter()
        .map(|entry| format!("{entry} {terminal}"))
        .collect()
}

#[test]
fn a_queued_reply_is_dropped_once_another_attachment_takes_ownership() {
    let mut env = Env::new();
    let a = live_owner(&mut env, 1);
    let b = second_viewer(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    env.send(reply(&a, "a1"));
    env.run();
    let mut pulled = env.pull();

    take_ownership(&mut env, b);
    assert!(since(&env, "fake:1", base).is_empty());

    release_and_drain(&mut env, release);
    assert_eq!(
        since(&env, "fake:1", base),
        entries(&["write held"], "fake:1")
    );
    assert!(!env.stub.log().iter().any(|entry| entry.starts_with("auto")));
    pulled.extend(env.pull());
    assert!(errors(&pulled).is_empty(), "{pulled:?}");
}

#[test]
fn a_queued_reply_is_dropped_after_its_attachment_loses_and_regains_ownership() {
    let mut env = Env::new();
    let a = live_owner(&mut env, 1);
    let b = second_viewer(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    env.send(reply(&a, "a1"));
    env.run();
    let mut pulled = env.pull();

    take_ownership(&mut env, b);
    let regained = take_ownership(&mut env, a.id);
    assert!(regained.0 > a.ownership.0);
    let current = Live {
        id: a.id,
        epoch: a.epoch,
        ownership: regained,
    };
    env.send(reply(&current, "a2"));
    env.run();
    assert!(since(&env, "fake:1", base).is_empty());

    release_and_drain(&mut env, release);
    assert_eq!(
        since(&env, "fake:1", base),
        entries(
            &[
                "write held",
                "set_display",
                "resize 80x24",
                "focus",
                "auto a2"
            ],
            "fake:1"
        )
    );
    pulled.extend(env.pull());
    assert!(errors(&pulled).is_empty(), "{pulled:?}");
}

#[test]
fn queued_replies_from_the_current_owner_are_forwarded_in_order() {
    let mut env = Env::new();
    let a = live_owner(&mut env, 1);
    second_viewer(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    env.send(reply(&a, "a1"));
    env.send(input(&a, "w1"));
    env.send(reply(&a, "a2"));
    env.run();
    assert!(since(&env, "fake:1", base).is_empty());

    release_and_drain(&mut env, release);
    assert_eq!(
        since(&env, "fake:1", base),
        entries(&["write held", "auto a1", "write w1", "auto a2"], "fake:1")
    );
    assert!(errors(&env.pull()).is_empty());
}

#[test]
fn accepted_input_queued_with_a_stale_reply_is_still_written_after_a_hand_off() {
    let mut env = Env::new();
    let a = live_owner(&mut env, 1);
    let b = second_viewer(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    env.send(input(&a, "w1"));
    env.send(reply(&a, "a1"));
    env.send(input(&a, "w2"));
    env.run();
    let mut pulled = env.pull();

    take_ownership(&mut env, b);
    release_and_drain(&mut env, release);
    assert_eq!(
        since(&env, "fake:1", base),
        entries(&["write held", "write w1", "write w2"], "fake:1")
    );
    pulled.extend(env.pull());
    assert!(errors(&pulled).is_empty(), "{pulled:?}");
}

#[test]
fn a_reply_batched_with_a_hand_off_and_regain_keeps_its_original_authorization() {
    let mut env = Env::new();
    let a = live_owner(&mut env, 1);
    let b = second_viewer(&mut env, 1);
    let release = env.stub.hold_next("fake:1", "write", false);
    let base = env.stub.log_of("fake:1").len();
    issue(&mut env, &a, Call::Write);
    let mut pulled = env.pull();

    // Core takes all of these before the driver sees any of their effects.
    env.send(reply(&a, "a1"));
    env.send(input(&a, "w1"));
    env.send(ClientMessage::RequestOwnership(RequestOwnership {
        attachment: b,
    }));
    env.send(ClientMessage::RequestOwnership(RequestOwnership {
        attachment: a.id,
    }));
    env.send(input(&a, "w2"));
    env.run();
    let ownership = env
        .driver
        .core()
        .ownership(&TermId("fake:1".into()))
        .unwrap();
    assert_eq!(ownership.owner, Some(a.id));
    assert!(ownership.generation.0 > a.ownership.0);
    assert!(since(&env, "fake:1", base).is_empty());

    release_and_drain(&mut env, release);
    assert_eq!(
        since(&env, "fake:1", base),
        entries(
            &[
                "write held",
                "write w1",
                "set_display",
                "resize 80x24",
                "focus",
                "write w2"
            ],
            "fake:1"
        )
    );
    pulled.extend(env.pull());
    assert!(errors(&pulled).is_empty(), "{pulled:?}");
}
