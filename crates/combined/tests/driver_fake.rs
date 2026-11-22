#![cfg(feature = "test-support")]
//! The driver and core against the fake backend, for orderings the contract suite can't force.

mod common;

use std::sync::Arc;

use combine_protocol::{
    Attach, AttachmentId, AutomaticReply, ClientMessage, DaemonMessage, Epoch, ErrorCode, Input,
    RequestId, RequestOwnership, TermId,
};
use combined::core::{Core, CoreConfig, Driver};
use combined::model::Millis;
use combined::ports::TerminalBackend;
use combined::testing::{FakeAttachmentRef, FakeBackend, FakeConfig, FakeControl, Half};
use common::{request_options, spawn_spec};
use futures::executor::block_on;

const NOW: Millis = Millis(0);

struct Env {
    driver: Driver,
    control: FakeControl,
    conn: combined::core::ConnectionId,
    term: TermId,
}

impl Env {
    fn new() -> Env {
        let (backend, control) = FakeBackend::new(FakeConfig::default());
        let term = block_on(backend.spawn(spawn_spec("/work", 80, 24))).unwrap();
        let core = Core::new(CoreConfig::default(), backend.capabilities()).unwrap();
        let backend: Arc<dyn TerminalBackend> = backend;
        let mut driver = Driver::new(core, backend);
        let conn = driver.core_mut().connect();
        Env {
            driver,
            control,
            conn,
            term,
        }
    }

    fn send(&mut self, message: ClientMessage) {
        self.driver
            .core_mut()
            .client_message(self.conn, message, NOW);
        block_on(self.driver.run_effects(NOW));
    }

    fn pump(&mut self) {
        block_on(self.driver.pump_ready(NOW));
    }

    fn pull(&mut self) -> Vec<DaemonMessage> {
        self.driver.core_mut().pull(self.conn, usize::MAX, NOW)
    }

    /// Attaches and makes the attachment the owner, with everything pumped and pulled.
    fn own(&mut self) -> (AttachmentId, Epoch) {
        self.send(ClientMessage::Attach(Attach {
            request: RequestId(1),
            terminal: self.term.clone(),
            options: request_options(80, 24, false),
        }));
        self.pump();
        let messages = self.pull();
        let attachment = messages
            .iter()
            .find_map(|message| match message {
                DaemonMessage::Attached(attached) => Some(attached.attachment),
                _ => None,
            })
            .unwrap();
        self.send(ClientMessage::RequestOwnership(RequestOwnership {
            attachment,
        }));
        self.pump();
        let mut messages = messages;
        messages.extend(self.pull());
        let epoch = messages
            .iter()
            .find_map(|message| match message {
                DaemonMessage::Started(started) => Some(started.cursor.epoch),
                _ => None,
            })
            .unwrap();
        (attachment, epoch)
    }
}

#[test]
fn a_reply_from_an_old_epoch_never_reaches_a_rebuilt_pair() {
    let mut env = Env::new();
    let (attachment, epoch) = env.own();
    let generation = env.driver.core().ownership(&env.term).unwrap().generation;

    env.control.fail_half(FakeAttachmentRef(1), Half::Input);
    assert!(env.control.step());

    env.send(ClientMessage::AutomaticReply(AutomaticReply {
        attachment,
        epoch,
        ownership_generation: generation,
        bytes: b"old-epoch-response".to_vec(),
    }));
    assert!(env.control.automatic_input(&env.term).is_empty());

    env.pump();
    env.pull();
    env.send(ClientMessage::AutomaticReply(AutomaticReply {
        attachment,
        epoch,
        ownership_generation: generation,
        bytes: b"still-old".to_vec(),
    }));
    assert!(env.control.automatic_input(&env.term).is_empty());
}

#[test]
fn a_rejected_input_is_reported_to_the_client() {
    let mut env = Env::new();
    let (attachment, _) = env.own();

    env.control.fail_half(FakeAttachmentRef(1), Half::Input);
    env.send(ClientMessage::Input(Input {
        attachment,
        bytes: vec![b'x'; 65_537],
    }));

    let messages = env.pull();
    let [DaemonMessage::Error(error)] = messages.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(error.attachment, Some(attachment));
    assert_eq!(error.request, None);
    assert_eq!(
        error.message,
        "the backend refused input for this attachment"
    );
    assert!(!error.message.contains("reopen queue"));

    while env.control.step() {}
    env.pump();
    assert!(env.control.typed_input(&env.term).is_empty());
}
