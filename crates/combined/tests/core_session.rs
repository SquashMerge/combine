//! The core state machine: attachment lifecycle, ownership, replies, grace and the client matrix.

use combine_protocol::{
    Ack, Attach, AttachmentId, AutomaticReply, BackendGeneration, ClientMessage, CloseReason,
    Cursor, DaemonMessage, Detach, Epoch, ErrorCode, ErrorMessage, Fidelity, Focus, Gap, Hello,
    Input, Ownership, OwnershipGeneration, RequestId, RequestOwnership, Resize, Resume, Seq,
    SetDisplay, TermId,
};
use combined::core::{
    BackendCommand, ConfigError, ConnectionId, Core, CoreConfig, Effect, LogEnd, OwnershipState,
};
use combined::model::Millis;
use combined::ports::{AttachEvent, BackendError, Capabilities};

mod common;
use common::{display, request_options};

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

fn term(n: u32) -> TermId {
    TermId(format!("fake:{n}"))
}

struct Rig {
    core: Core,
    now: u64,
}

impl Rig {
    fn new() -> Rig {
        Rig::with(CoreConfig::default(), caps())
    }

    fn with(config: CoreConfig, capabilities: Capabilities) -> Rig {
        Rig {
            core: Core::new(config, capabilities).unwrap(),
            now: 0,
        }
    }

    fn now(&self) -> Millis {
        Millis(self.now)
    }

    fn send(&mut self, connection: ConnectionId, message: ClientMessage) {
        self.core.client_message(connection, message, self.now());
    }

    fn effects(&mut self) -> Vec<Effect> {
        self.core.take_effects()
    }

    fn commands(&mut self) -> Vec<BackendCommand> {
        self.effects()
            .into_iter()
            .map(|effect| match effect {
                Effect::Backend(command) => command,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    fn pull(&mut self, connection: ConnectionId) -> Vec<DaemonMessage> {
        self.core.pull(connection, usize::MAX, self.now())
    }

    fn attach_request(
        &mut self,
        connection: ConnectionId,
        request: u64,
        terminal: u32,
        observe: bool,
    ) -> AttachmentId {
        self.send(
            connection,
            ClientMessage::Attach(Attach {
                request: RequestId(request),
                terminal: term(terminal),
                options: request_options(80, 24, observe),
            }),
        );
        let commands = self.commands();
        let [BackendCommand::Attach { attachment, .. }] = commands.as_slice() else {
            panic!("expected one attach, got {commands:?}");
        };
        *attachment
    }

    /// Attaches and answers `attached`, leaving the `attached` and `ownership` messages queued.
    fn attach(
        &mut self,
        connection: ConnectionId,
        request: u64,
        terminal: u32,
        observe: bool,
    ) -> AttachmentId {
        let id = self.attach_request(connection, request, terminal, observe);
        self.core.attach_result(id, Ok(()), self.now());
        id
    }

    fn started(&mut self, id: AttachmentId, generation: u64) {
        self.core.backend_event(
            id,
            BackendGeneration(generation),
            AttachEvent::Started {
                cols: 80,
                rows: 24,
                fidelity: Fidelity::Exact,
                ordered_resizes: true,
            },
            self.now(),
        );
    }

    fn event(&mut self, id: AttachmentId, generation: u64, event: AttachEvent) {
        self.core
            .backend_event(id, BackendGeneration(generation), event, self.now());
    }

    /// An attachment that is live at generation 1, with everything queued so far taken.
    fn open(&mut self, connection: ConnectionId, terminal: u32, observe: bool) -> AttachmentId {
        let id = self.attach(connection, 1, terminal, observe);
        self.started(id, 1);
        self.pull(connection);
        self.effects();
        id
    }

    fn generation(&self, terminal: u32) -> OwnershipGeneration {
        self.core.ownership(&term(terminal)).unwrap().generation
    }

    fn owner(&self, terminal: u32) -> Option<AttachmentId> {
        self.core.ownership(&term(terminal)).unwrap().owner
    }

    fn request_ownership(&mut self, connection: ConnectionId, id: AttachmentId) {
        self.send(
            connection,
            ClientMessage::RequestOwnership(RequestOwnership { attachment: id }),
        );
    }

    fn focus(&mut self, connection: ConnectionId, id: AttachmentId) {
        self.send(connection, ClientMessage::Focus(Focus { attachment: id }));
    }

    fn input(&mut self, connection: ConnectionId, id: AttachmentId, bytes: &[u8]) {
        self.send(
            connection,
            ClientMessage::Input(Input {
                attachment: id,
                bytes: bytes.to_vec(),
            }),
        );
    }
}

fn error_of(message: &DaemonMessage) -> &ErrorMessage {
    match message {
        DaemonMessage::Error(error) => error,
        other => panic!("not an error: {other:?}"),
    }
}

fn names(messages: &[DaemonMessage]) -> Vec<&'static str> {
    messages.iter().map(|m| m.type_name()).collect()
}

fn ownership_msg(id: AttachmentId, generation: u64, owner: Option<AttachmentId>) -> DaemonMessage {
    DaemonMessage::Ownership(Ownership {
        attachment: id,
        ownership_generation: OwnershipGeneration(generation),
        owner,
    })
}

fn cursor(id: AttachmentId, epoch: u64, seq: u64) -> Cursor {
    Cursor {
        attachment: id,
        epoch: Epoch(epoch),
        seq: Seq(seq),
    }
}

#[test]
fn core_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Core>();
}

#[test]
fn config_too_small_is_rejected() {
    assert!(Core::new(CoreConfig::default(), caps()).is_ok());

    let reserve = |bytes| CoreConfig {
        credit_reserve: bytes,
        ..CoreConfig::default()
    };
    assert_eq!(
        Core::new(reserve(895), caps()).err(),
        Some(ConfigError::ReserveTooSmall { needed: 896 })
    );
    assert!(Core::new(reserve(896), caps()).is_ok());

    let events = |count| CoreConfig {
        log_max_events: count,
        ..CoreConfig::default()
    };
    assert_eq!(
        Core::new(events(13), caps()).err(),
        Some(ConfigError::TooFewEvents { needed: 14 })
    );
    assert!(Core::new(events(14), caps()).is_ok());

    let credit = Capabilities {
        byte_credit: true,
        initial_credit: 1000,
        ..caps()
    };
    let small_log = CoreConfig {
        log_max_bytes: 66_000,
        ..CoreConfig::default()
    };
    assert_eq!(
        Core::new(small_log.clone(), credit).err(),
        Some(ConfigError::LogTooSmall {
            needed: 1000 + 65_536 + 128,
            max_bytes: 66_000,
        })
    );
    assert!(Core::new(small_log, caps()).is_ok());

    let all_wrong = CoreConfig {
        credit_reserve: 0,
        log_max_events: 0,
        log_max_bytes: 0,
        ..CoreConfig::default()
    };
    assert!(matches!(
        Core::new(all_wrong, credit),
        Err(ConfigError::ReserveTooSmall { .. })
    ));
}

#[test]
fn attached_comes_before_started_and_ownership_follows() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    rig.send(
        c,
        ClientMessage::Attach(Attach {
            request: RequestId(11),
            terminal: term(1),
            options: request_options(80, 24, false),
        }),
    );
    let commands = rig.commands();
    let [BackendCommand::Attach {
        attachment,
        terminal,
        options,
    }] = commands.as_slice()
    else {
        panic!("{commands:?}");
    };
    let id = *attachment;
    assert_eq!(*terminal, term(1));
    assert_eq!((options.cols, options.rows), (80, 24));
    assert_eq!(options.display, display());
    assert!(!options.observe && !options.resumed);
    assert!(rig.pull(c).is_empty());
    assert!(!rig.core.has_output(c));

    rig.core.attach_result(id, Ok(()), rig.now());
    assert!(rig.core.has_output(c));
    rig.started(id, 1);
    let messages = rig.pull(c);
    assert_eq!(names(&messages), ["attached", "ownership", "started"]);
    let DaemonMessage::Attached(attached) = &messages[0] else {
        panic!()
    };
    assert_eq!((attached.request, attached.attachment), (RequestId(11), id));
    assert_eq!(messages[1], ownership_msg(id, rig.generation(1).0, None));
    assert_eq!(messages[2].cursor(), Some(cursor(id, 1, 0)));
    assert_eq!(
        rig.core.ownership(&term(1)),
        Some(OwnershipState {
            generation: rig.generation(1),
            owner: None
        })
    );
}

#[test]
fn attach_failure_answers_the_request() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.attach_request(c, 4, 1, false);
    rig.core
        .attach_result(id, Err(BackendError::Failed("boom".into())), rig.now());
    let messages = rig.pull(c);
    assert_eq!(messages.len(), 1);
    let error = error_of(&messages[0]);
    assert_eq!(error.code, ErrorCode::AttachFailed);
    assert_eq!(error.request, Some(RequestId(4)));
    assert_eq!(error.attachment, None);
    assert!(rig.core.attachment_stats(id).is_none());
    assert!(rig.effects().is_empty());
}

#[test]
fn remote_terminal_is_unknown_terminal() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    for (request, failure) in [
        (1, BackendError::RemoteTerminal(term(1))),
        (2, BackendError::UnknownTerminal(term(1))),
    ] {
        let id = rig.attach_request(c, request, 1, false);
        rig.core.attach_result(id, Err(failure), rig.now());
        let messages = rig.pull(c);
        let error = error_of(&messages[0]);
        assert_eq!(error.code, ErrorCode::UnknownTerminal);
        assert_eq!(error.message, "unknown terminal");
        assert_eq!(error.request, Some(RequestId(request)));
    }
}

#[test]
fn zero_size_attach_is_invalid_request() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let before = rig.core.high_water();
    for (cols, rows) in [(0, 24), (80, 0), (0, 0)] {
        rig.send(
            c,
            ClientMessage::Attach(Attach {
                request: RequestId(3),
                terminal: term(1),
                options: request_options(cols, rows, false),
            }),
        );
        let messages = rig.pull(c);
        assert_eq!(messages.len(), 1);
        let error = error_of(&messages[0]);
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.request, Some(RequestId(3)));
        assert_eq!(error.attachment, None);
        assert!(rig.effects().is_empty());
    }
    assert_eq!(rig.core.high_water(), before);
}

#[test]
fn hello_after_handshake_is_invalid_request() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    rig.send(
        c,
        ClientMessage::Hello(Hello {
            protocol_version: 1,
        }),
    );
    let messages = rig.pull(c);
    assert_eq!(messages.len(), 1);
    let error = error_of(&messages[0]);
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!((error.request, error.attachment), (None, None));
    assert!(rig.effects().is_empty());
}

#[test]
fn error_messages_carry_no_peer_text() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let a = rig.open(c, 2, false);
    let watcher = rig.open(c, 3, true);
    let secret = TermId("fake:secret-terminal".to_string());
    rig.send(
        c,
        ClientMessage::Attach(Attach {
            request: RequestId(1),
            terminal: secret.clone(),
            options: request_options(80, 24, false),
        }),
    );
    let commands = rig.commands();
    let BackendCommand::Attach { attachment, .. } = &commands[0] else {
        panic!()
    };
    rig.core.attach_result(
        *attachment,
        Err(BackendError::Failed("secret-detail".into())),
        rig.now(),
    );
    rig.send(
        c,
        ClientMessage::Resize(Resize {
            attachment: a,
            cols: 0,
            rows: 0,
            ownership_generation: OwnershipGeneration(0),
        }),
    );
    rig.send(
        c,
        ClientMessage::Resize(Resize {
            attachment: AttachmentId(999),
            cols: 1,
            rows: 1,
            ownership_generation: OwnershipGeneration(0),
        }),
    );
    rig.send(
        c,
        ClientMessage::Ack(Ack {
            cursor: cursor(a, 1, 50),
        }),
    );
    rig.send(
        c,
        ClientMessage::Hello(Hello {
            protocol_version: 1,
        }),
    );
    rig.input(c, watcher, b"secret-input");
    let errors: Vec<_> = rig
        .pull(c)
        .into_iter()
        .filter(|m| matches!(m, DaemonMessage::Error(_)))
        .collect();
    assert_eq!(errors.len(), 6);
    for message in &errors {
        let error = error_of(message);
        assert!(!error.message.contains("secret"), "{}", error.message);
        assert!(!error.message.contains("fake:"), "{}", error.message);
        assert!(!error.message.is_empty());
    }
}

#[test]
fn started_opens_a_new_epoch_and_older_generations_are_dropped() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.attach(c, 1, 1, false);
    rig.started(id, 1);
    rig.event(id, 1, AttachEvent::Data(b"a".to_vec()));
    rig.started(id, 3);
    rig.event(id, 1, AttachEvent::Data(b"late".to_vec()));
    rig.started(id, 2);
    rig.started(id, 3);
    rig.event(id, 3, AttachEvent::Data(b"b".to_vec()));
    rig.event(id, 2, AttachEvent::Resized { cols: 1, rows: 1 });
    let cursors: Vec<_> = rig.pull(c).iter().filter_map(|m| m.cursor()).collect();
    assert_eq!(
        cursors,
        [
            cursor(id, 1, 0),
            cursor(id, 1, 1),
            cursor(id, 2, 0),
            cursor(id, 2, 1)
        ]
    );
}

#[test]
fn events_before_the_first_started_are_dropped() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.attach(c, 1, 1, false);
    rig.event(id, 1, AttachEvent::Data(b"early".to_vec()));
    rig.event(id, 1, AttachEvent::Resized { cols: 1, rows: 1 });
    rig.event(id, 1, AttachEvent::Closed(CloseReason::Exited));
    rig.event(id, 1, AttachEvent::Lost);
    assert_eq!(names(&rig.pull(c)), ["attached", "ownership"]);
    rig.started(id, 1);
    let messages = rig.pull(c);
    assert_eq!(names(&messages), ["started"]);
    assert_eq!(messages[0].cursor(), Some(cursor(id, 1, 0)));
}

fn assert_final(kind: &str, event: AttachEvent) {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.event(id, 1, event);
    assert_eq!(
        rig.commands(),
        [BackendCommand::Detach { attachment: id }],
        "{kind}"
    );
    rig.event(id, 1, AttachEvent::Data(b"after".to_vec()));
    rig.started(id, 2);
    rig.event(id, 2, AttachEvent::Data(b"after".to_vec()));
    assert!(rig.effects().is_empty(), "{kind}");
    let stats = rig.core.attachment_stats(id).unwrap();
    assert_eq!(stats.ended, Some(LogEnd::Final), "{kind}");
    let messages = rig.pull(c);
    assert_eq!(names(&messages), [kind], "{kind}");
    let last = messages[0].cursor().unwrap();
    assert_eq!(last, cursor(id, 1, 1));
    rig.send(c, ClientMessage::Ack(Ack { cursor: last }));
    assert!(rig.core.attachment_stats(id).is_none(), "{kind}");
}

#[test]
fn closed_is_final() {
    assert_final("closed", AttachEvent::Closed(CloseReason::Exited));
}

#[test]
fn lost_is_final() {
    assert_final("lost", AttachEvent::Lost);
}

#[test]
fn stream_end_without_final_event_is_lost() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.event(id, 1, AttachEvent::Data(b"x".to_vec()));
    rig.core.backend_stream_ended(id, rig.now());
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
    assert_eq!(names(&rig.pull(c)), ["data", "lost"]);

    let id = rig.attach(c, 2, 2, false);
    rig.core.backend_stream_ended(id, rig.now());
    let messages = rig.pull(c);
    assert_eq!(names(&messages), ["attached", "ownership", "gap"]);
    assert_eq!(
        messages[2],
        DaemonMessage::Gap(Gap {
            attachment: id,
            request: None
        })
    );
    assert!(rig.core.attachment_stats(id).is_none());
}

#[test]
fn attachment_ids_start_above_id_base_and_never_repeat() {
    let config = CoreConfig {
        id_base: 100,
        ..CoreConfig::default()
    };
    let mut rig = Rig::with(config, caps());
    assert_eq!(rig.core.high_water(), 99);
    let c = rig.core.connect();
    let mut seen = Vec::new();
    for round in 0..10 {
        let id = rig.attach(c, round, 1, false);
        assert!(id.0 >= 100);
        assert!(seen.iter().all(|earlier| *earlier < id));
        seen.push(id);
        rig.started(id, 1);
        rig.send(c, ClientMessage::Detach(Detach { attachment: id }));
        rig.pull(c);
        rig.effects();
    }
    let high = rig.core.high_water();
    assert!(high >= seen.last().unwrap().0);

    let next = CoreConfig {
        id_base: high + 1,
        ..CoreConfig::default()
    };
    let mut second = Rig::with(next, caps());
    let c = second.core.connect();
    let id = second.attach(c, 1, 1, false);
    assert!(id.0 > high);
}

#[test]
fn results_for_finished_attachments_release_the_backend() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let ghost = AttachmentId(77);
    rig.core.attach_result(ghost, Ok(()), rig.now());
    assert_eq!(
        rig.commands(),
        [BackendCommand::Detach { attachment: ghost }]
    );
    rig.core
        .attach_result(ghost, Err(BackendError::Closed), rig.now());
    assert!(rig.effects().is_empty());
    rig.event(ghost, 1, AttachEvent::Data(b"x".to_vec()));
    assert_eq!(
        rig.commands(),
        [BackendCommand::Detach { attachment: ghost }]
    );

    let id = rig.open(c, 1, false);
    rig.event(id, 1, AttachEvent::Lost);
    rig.effects();
    rig.send(
        c,
        ClientMessage::Ack(Ack {
            cursor: cursor(id, 1, 1),
        }),
    );
    rig.pull(c);
    rig.send(
        c,
        ClientMessage::Ack(Ack {
            cursor: cursor(id, 1, 1),
        }),
    );
    rig.pull(c);
    rig.event(id, 1, AttachEvent::Data(b"x".to_vec()));
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);

    let pending = rig.attach_request(c, 9, 2, false);
    rig.core.disconnect(c, rig.now());
    rig.core.attach_result(pending, Ok(()), rig.now());
    assert_eq!(
        rig.commands(),
        [BackendCommand::Detach {
            attachment: pending
        }]
    );
}

#[test]
fn detach_before_started_ends_with_gap() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.attach(c, 1, 1, false);
    rig.effects();
    rig.send(c, ClientMessage::Detach(Detach { attachment: id }));
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
    let messages = rig.pull(c);
    assert_eq!(names(&messages), ["attached", "ownership", "gap"]);
    assert!(rig.core.attachment_stats(id).is_none());
    assert!(rig.core.ownership(&term(1)).is_none());
}

#[test]
fn detach_of_a_live_attachment_closes_it() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.send(c, ClientMessage::Detach(Detach { attachment: id }));
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
    let messages = rig.pull(c);
    let DaemonMessage::Closed(closed) = &messages[0] else {
        panic!("{messages:?}")
    };
    assert_eq!(closed.reason, CloseReason::Detached);
    rig.send(
        c,
        ClientMessage::Ack(Ack {
            cursor: closed.cursor,
        }),
    );
    assert!(rig.core.attachment_stats(id).is_none());
    rig.send(c, ClientMessage::Detach(Detach { attachment: id }));
    let messages = rig.pull(c);
    assert_eq!(error_of(&messages[0]).code, ErrorCode::UnknownAttachment);
}

#[test]
fn disconnect_abandons_opening_attachments() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.attach_request(c, 1, 1, false);
    assert!(rig.core.attachment_stats(id).is_some());
    rig.core.disconnect(c, rig.now());
    assert!(rig.core.attachment_stats(id).is_none());
    rig.core.attach_result(id, Ok(()), rig.now());
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
    assert!(rig.core.ownership(&term(1)).is_none());
}

#[test]
fn grace_keeps_the_attachment_then_finishes_it() {
    let config = CoreConfig {
        grace: Millis(1000),
        ..CoreConfig::default()
    };
    let mut rig = Rig::with(config, caps());
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.now = 100;
    rig.core.disconnect(c, rig.now());
    assert!(rig.core.attachment_stats(id).is_some());
    assert!(rig.effects().is_empty());
    rig.core.tick(Millis(1099));
    assert!(rig.core.attachment_stats(id).is_some());
    rig.core.tick(Millis(1100));
    assert!(rig.core.attachment_stats(id).is_none());
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
    assert!(rig.core.ownership(&term(1)).is_none());

    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.event(id, 1, AttachEvent::Lost);
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
    rig.core.disconnect(c, Millis(2000));
    rig.core.tick(Millis(2999));
    assert!(rig.core.attachment_stats(id).is_some());
    rig.core.tick(Millis(3000));
    assert!(rig.core.attachment_stats(id).is_none());
    assert!(rig.effects().is_empty());
}

#[test]
fn gap_timeout_disconnects_a_stuck_client() {
    let config = CoreConfig {
        log_max_bytes: 8192,
        credit_reserve: 1024,
        gap_timeout: Millis(1000),
        ..CoreConfig::default()
    };
    let mut rig = Rig::with(config.clone(), caps());
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.now = 50;
    rig.event(id, 1, AttachEvent::Data(vec![0; 9000]));
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
    rig.core.tick(Millis(1049));
    assert!(rig.effects().is_empty());
    rig.core.tick(Millis(1050));
    assert_eq!(rig.effects(), [Effect::Disconnect(c)]);
    assert!(rig.core.attachment_stats(id).is_none());
    assert!(!rig.core.has_output(c));
    rig.core.tick(Millis(5000));
    assert!(rig.effects().is_empty());

    let mut rig = Rig::with(config, caps());
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.event(id, 1, AttachEvent::Data(vec![0; 9000]));
    rig.effects();
    let messages = rig.pull(c);
    assert!(matches!(messages.last(), Some(DaemonMessage::Gap(_))));
    rig.core.tick(Millis(10_000));
    assert!(rig.effects().is_empty());
    assert!(rig.core.attachment_stats(id).is_none());
}

#[test]
fn control_queue_overflow_disconnects() {
    let config = CoreConfig {
        max_control_messages: 3,
        ..CoreConfig::default()
    };
    let mut rig = Rig::with(config, caps());
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    let bad = || {
        ClientMessage::Attach(Attach {
            request: RequestId(1),
            terminal: term(1),
            options: request_options(0, 0, false),
        })
    };
    for _ in 0..3 {
        rig.send(c, bad());
    }
    assert!(rig.effects().is_empty());
    rig.send(c, bad());
    assert_eq!(rig.effects(), [Effect::Disconnect(c)]);
    assert!(rig.pull(c).is_empty());
    assert!(!rig.core.has_output(c));
    assert!(rig.core.attachment_stats(id).is_some());
    rig.send(c, bad());
    assert!(rig.effects().is_empty());
}

#[test]
fn overflow_during_announce_never_leaves_a_stale_owner() {
    let config = CoreConfig {
        max_control_messages: 3,
        ..CoreConfig::default()
    };
    let mut rig = Rig::with(config, caps());
    let (x, y, z) = (rig.core.connect(), rig.core.connect(), rig.core.connect());
    let a = rig.open(x, 1, false);
    let b = rig.open(y, 1, false);
    let c = rig.open(z, 1, false);
    rig.focus(z, c);
    rig.effects();
    let bad = || {
        ClientMessage::Attach(Attach {
            request: RequestId(9),
            terminal: term(1),
            options: request_options(0, 0, false),
        })
    };
    for _ in 0..3 {
        rig.send(y, bad());
    }
    assert!(rig.effects().is_empty());

    rig.input(y, b, b"x");

    assert_eq!(rig.owner(1), Some(c));
    assert!(rig.effects().contains(&Effect::Disconnect(y)));
    assert!(rig.pull(y).is_empty());
    let current = rig.core.ownership(&term(1)).unwrap();
    let last_notice = |messages: Vec<DaemonMessage>| {
        messages
            .into_iter()
            .rev()
            .find_map(|message| match message {
                DaemonMessage::Ownership(ownership) => Some(ownership),
                _ => None,
            })
            .unwrap()
    };
    let heard_a = last_notice(rig.pull(x));
    let heard_c = last_notice(rig.pull(z));
    for (heard, id) in [(heard_a, a), (heard_c, c)] {
        assert_eq!(heard.attachment, id);
        assert_eq!(heard.ownership_generation, current.generation);
        assert_eq!(heard.owner, current.owner);
    }

    rig.send(
        z,
        ClientMessage::Resize(Resize {
            attachment: c,
            cols: 100,
            rows: 30,
            ownership_generation: current.generation,
        }),
    );
    assert!(rig.pull(z).is_empty());
    assert_eq!(
        rig.commands().last(),
        Some(&BackendCommand::Resize {
            attachment: c,
            cols: 100,
            rows: 30
        })
    );
}

#[test]
fn resume_on_a_new_connection_moves_the_attachment() {
    let mut rig = Rig::new();
    let c1 = rig.core.connect();
    let id = rig.open(c1, 1, false);
    rig.event(id, 1, AttachEvent::Data(b"hello".to_vec()));
    rig.event(id, 1, AttachEvent::Resized { cols: 5, rows: 5 });
    let sent = rig.pull(c1);
    assert_eq!(names(&sent), ["data", "resized"]);
    rig.send(
        c1,
        ClientMessage::Ack(Ack {
            cursor: cursor(id, 1, 1),
        }),
    );
    rig.event(id, 1, AttachEvent::Data(b"more".to_vec()));
    rig.core.disconnect(c1, rig.now());

    let c2 = rig.core.connect();
    rig.send(
        c2,
        ClientMessage::Resume(Resume {
            request: RequestId(9),
            cursor: cursor(id, 1, 1),
        }),
    );
    let messages = rig.pull(c2);
    assert_eq!(
        names(&messages),
        ["resumed", "ownership", "resized", "data"]
    );
    let DaemonMessage::Resumed(resumed) = &messages[0] else {
        panic!()
    };
    assert_eq!(resumed.request, RequestId(9));
    assert_eq!(resumed.attachment, id);
    assert_eq!(resumed.replay_through, Some(cursor(id, 1, 3)));
    assert_eq!(messages[2].cursor(), Some(cursor(id, 1, 2)));

    rig.event(id, 1, AttachEvent::Resized { cols: 6, rows: 6 });
    assert_eq!(names(&rig.pull(c2)), ["resized"]);

    let c3 = rig.core.connect();
    rig.send(
        c3,
        ClientMessage::Resume(Resume {
            request: RequestId(10),
            cursor: cursor(id, 1, 1),
        }),
    );
    assert_eq!(
        names(&rig.pull(c3)),
        ["resumed", "ownership", "resized", "data", "resized"]
    );
    rig.event(id, 1, AttachEvent::Resized { cols: 7, rows: 7 });
    assert!(rig.pull(c2).is_empty());
    assert_eq!(names(&rig.pull(c3)), ["resized"]);
    rig.core.tick(Millis(1_000_000));
    assert!(rig.core.attachment_stats(id).is_some());
}

#[test]
fn resume_of_unknown_attachment_is_gap_with_request() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    rig.send(
        c,
        ClientMessage::Resume(Resume {
            request: RequestId(5),
            cursor: cursor(AttachmentId(99), 1, 0),
        }),
    );
    assert_eq!(
        rig.pull(c),
        [DaemonMessage::Gap(Gap {
            attachment: AttachmentId(99),
            request: Some(RequestId(5))
        })]
    );
    assert!(rig.effects().is_empty());

    let id = rig.open(c, 1, false);
    rig.event(id, 1, AttachEvent::Data(b"x".to_vec()));
    rig.pull(c);
    rig.send(
        c,
        ClientMessage::Resume(Resume {
            request: RequestId(6),
            cursor: cursor(id, 1, 7),
        }),
    );
    assert_eq!(
        rig.pull(c),
        [DaemonMessage::Gap(Gap {
            attachment: id,
            request: Some(RequestId(6))
        })]
    );
    assert!(rig.core.attachment_stats(id).is_none());
    assert_eq!(rig.commands(), [BackendCommand::Detach { attachment: id }]);
}

#[test]
fn pull_puts_control_before_stream_and_round_robins() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let ids: Vec<_> = (1..=3)
        .map(|n| {
            let id = rig.attach(c, n as u64, n, false);
            rig.started(id, 1);
            for _ in 0..3 {
                rig.event(id, 1, AttachEvent::Resized { cols: 9, rows: 9 });
            }
            id
        })
        .collect();
    rig.send(
        c,
        ClientMessage::Attach(Attach {
            request: RequestId(1),
            terminal: term(1),
            options: request_options(0, 1, false),
        }),
    );
    let first = rig.core.pull(c, 0, rig.now());
    assert_eq!(
        names(&first),
        [
            "attached",
            "ownership",
            "attached",
            "ownership",
            "attached",
            "ownership",
            "error",
            "started"
        ]
    );
    assert_eq!(first[7].cursor(), Some(cursor(ids[0], 1, 0)));
    let attachment_of = |batch: &[DaemonMessage]| -> Vec<u64> {
        batch
            .iter()
            .map(|m| m.stream_attachment().unwrap().0)
            .collect()
    };
    let next = rig.core.pull(c, 0, rig.now());
    assert_eq!(attachment_of(&next), [ids[1].0]);
    let next = rig.core.pull(c, 0, rig.now());
    assert_eq!(attachment_of(&next), [ids[2].0]);
    let next = rig.core.pull(c, 0, rig.now());
    assert_eq!(attachment_of(&next), [ids[0].0]);
    assert_eq!(next[0].cursor(), Some(cursor(ids[0], 1, 1)));

    let two = rig.core.pull(c, 128, rig.now());
    assert_eq!(attachment_of(&two), [ids[1].0, ids[2].0]);
    let rest = rig.pull(c);
    assert_eq!(rest.len(), 12 - 6);
    assert_eq!(
        attachment_of(&rest),
        [ids[0], ids[1], ids[2], ids[0], ids[1], ids[2]].map(|id| id.0)
    );
    assert!(!rig.core.has_output(c));
}

#[test]
fn stale_or_future_resize_is_stale_generation() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.request_ownership(c, id);
    rig.effects();
    rig.pull(c);
    let current = rig.generation(1);
    let resize = |generation: OwnershipGeneration, cols, rows| {
        ClientMessage::Resize(Resize {
            attachment: id,
            cols,
            rows,
            ownership_generation: generation,
        })
    };
    for generation in [
        OwnershipGeneration(current.0 - 1),
        OwnershipGeneration(current.0 + 1),
    ] {
        rig.send(c, resize(generation, 100, 30));
        let messages = rig.pull(c);
        let error = error_of(&messages[0]);
        assert_eq!(error.code, ErrorCode::StaleGeneration);
        assert_eq!(error.attachment, Some(id));
        assert!(rig.effects().is_empty());
    }
    rig.send(c, resize(current, 0, 30));
    assert_eq!(error_of(&rig.pull(c)[0]).code, ErrorCode::InvalidRequest);
    rig.send(c, resize(current, 100, 30));
    assert!(rig.pull(c).is_empty());
    assert_eq!(
        rig.commands(),
        [BackendCommand::Resize {
            attachment: id,
            cols: 100,
            rows: 30
        }]
    );

    rig.send(
        c,
        ClientMessage::Resize(Resize {
            attachment: AttachmentId(999),
            cols: 1,
            rows: 1,
            ownership_generation: current,
        }),
    );
    assert_eq!(error_of(&rig.pull(c)[0]).code, ErrorCode::UnknownAttachment);
}

#[test]
fn watch_only_errors() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.open(c, 1, true);
    let stale = OwnershipGeneration(0);
    rig.input(c, id, b"x");
    rig.request_ownership(c, id);
    rig.send(
        c,
        ClientMessage::Resize(Resize {
            attachment: id,
            cols: 0,
            rows: 0,
            ownership_generation: stale,
        }),
    );
    let messages = rig.pull(c);
    assert_eq!(messages.len(), 3);
    for message in &messages {
        let error = error_of(message);
        assert_eq!(error.code, ErrorCode::WatchOnly);
        assert_eq!(error.attachment, Some(id));
    }
    rig.focus(c, id);
    rig.send(
        c,
        ClientMessage::SetDisplay(SetDisplay {
            attachment: id,
            display: display(),
        }),
    );
    assert!(rig.pull(c).is_empty());
    assert!(rig.effects().is_empty());
    assert_eq!(rig.owner(1), None);
}

#[test]
fn non_owner_input_grants_then_forwards() {
    let mut rig = Rig::new();
    let (c1, c2) = (rig.core.connect(), rig.core.connect());
    let a = rig.open(c1, 1, false);
    let b = rig.open(c2, 1, false);
    let start = rig.generation(1);

    rig.input(c1, a, b"hi");
    let granted = rig.generation(1);
    assert!(granted > start);
    assert_eq!(rig.owner(1), Some(a));
    assert_eq!(
        rig.commands(),
        [
            BackendCommand::SetDisplay {
                attachment: a,
                display: display()
            },
            BackendCommand::Resize {
                attachment: a,
                cols: 80,
                rows: 24
            },
            BackendCommand::Focus { attachment: a },
            BackendCommand::Write {
                attachment: a,
                bytes: b"hi".to_vec()
            },
        ]
    );
    assert_eq!(rig.pull(c1), [ownership_msg(a, granted.0, Some(a))]);
    assert_eq!(rig.pull(c2), [ownership_msg(b, granted.0, Some(a))]);

    rig.input(c1, a, b"more");
    rig.input(c1, a, b"");
    assert_eq!(
        rig.commands(),
        [BackendCommand::Write {
            attachment: a,
            bytes: b"more".to_vec()
        }]
    );
    assert_eq!(rig.generation(1), granted);

    rig.input(c2, b, b"mine");
    assert_eq!(rig.owner(1), Some(b));
    let commands = rig.commands();
    assert_eq!(commands.len(), 4);
    assert_eq!(
        commands[3],
        BackendCommand::Write {
            attachment: b,
            bytes: b"mine".to_vec()
        }
    );
    let second = rig.generation(1);
    assert!(second > granted);
    assert_eq!(rig.pull(c1), [ownership_msg(a, second.0, Some(b))]);
    assert_eq!(rig.pull(c2), [ownership_msg(b, second.0, Some(b))]);
}

#[test]
fn grants_skip_set_display_without_display_queries() {
    let mut rig = Rig::with(
        CoreConfig::default(),
        Capabilities {
            display_queries: false,
            ..caps()
        },
    );
    let c = rig.core.connect();
    let a = rig.open(c, 1, false);
    rig.send(
        c,
        ClientMessage::SetDisplay(SetDisplay {
            attachment: a,
            display: display(),
        }),
    );
    rig.input(c, a, b"x");
    assert_eq!(
        rig.commands(),
        [
            BackendCommand::Resize {
                attachment: a,
                cols: 80,
                rows: 24
            },
            BackendCommand::Focus { attachment: a },
            BackendCommand::Write {
                attachment: a,
                bytes: b"x".to_vec()
            },
        ]
    );
}

#[test]
fn hand_off_to_most_recently_focused_or_none() {
    let mut rig = Rig::new();
    let conns: Vec<_> = (0..5).map(|_| rig.core.connect()).collect();
    let [ca, cb, cc, cd, ce] = conns.as_slice() else {
        panic!()
    };
    let a = rig.open(*ca, 1, false);
    let b = rig.open(*cb, 1, false);
    let c = rig.open(*cc, 1, false);
    let d = rig.open(*cd, 1, true);
    let e = rig.open(*ce, 1, false);
    rig.request_ownership(*ca, a);
    rig.focus(*cb, b);
    rig.focus(*cc, c);
    rig.focus(*cd, d);
    rig.effects();
    let owned = rig.generation(1);

    rig.send(*ca, ClientMessage::Detach(Detach { attachment: a }));
    assert_eq!(rig.owner(1), Some(c));
    let after_a = rig.generation(1);
    assert!(after_a > owned);
    let commands = rig.commands();
    assert!(commands.contains(&BackendCommand::Focus { attachment: c }));
    assert!(commands.contains(&BackendCommand::Resize {
        attachment: c,
        cols: 80,
        rows: 24
    }));
    assert!(commands.contains(&BackendCommand::SetDisplay {
        attachment: c,
        display: display()
    }));
    assert_eq!(
        rig.pull(*cc).last(),
        Some(&ownership_msg(c, after_a.0, Some(c)))
    );
    assert_eq!(
        rig.pull(*cd).last(),
        Some(&ownership_msg(d, after_a.0, Some(c)))
    );

    rig.send(*cc, ClientMessage::Detach(Detach { attachment: c }));
    assert_eq!(rig.owner(1), Some(b));
    rig.send(*cb, ClientMessage::Detach(Detach { attachment: b }));
    assert_eq!(rig.owner(1), None);
    assert!(rig.generation(1) > after_a);
    let last = rig.pull(*ce).pop().unwrap();
    assert_eq!(last, ownership_msg(e, rig.generation(1).0, None));
    rig.effects();
    rig.input(*ce, e, b"x");
    assert_eq!(rig.owner(1), Some(e));
}

#[test]
fn connection_drop_hands_off_ownership() {
    let mut rig = Rig::new();
    let (c1, c2, c3) = (rig.core.connect(), rig.core.connect(), rig.core.connect());
    let a = rig.open(c1, 1, false);
    let b = rig.open(c2, 1, false);
    rig.request_ownership(c1, a);
    rig.focus(c2, b);
    rig.effects();
    rig.pull(c2);
    let owned = rig.generation(1);

    rig.core.disconnect(c1, rig.now());
    assert_eq!(rig.owner(1), Some(b));
    let handed = rig.generation(1);
    assert!(handed > owned);
    assert_eq!(
        rig.commands(),
        [
            BackendCommand::SetDisplay {
                attachment: b,
                display: display()
            },
            BackendCommand::Resize {
                attachment: b,
                cols: 80,
                rows: 24
            },
            BackendCommand::Focus { attachment: b },
        ]
    );
    assert_eq!(rig.pull(c2), [ownership_msg(b, handed.0, Some(b))]);

    rig.core.disconnect(c2, rig.now());
    assert_eq!(rig.owner(1), None);
    assert!(rig.generation(1) > handed);

    rig.send(
        c3,
        ClientMessage::Resume(Resume {
            request: RequestId(2),
            cursor: cursor(a, 1, 0),
        }),
    );
    let messages = rig.pull(c3);
    assert_eq!(names(&messages), ["resumed", "ownership"]);
    assert_eq!(rig.owner(1), None);
    assert!(rig.effects().is_empty());
}

#[test]
fn owner_rebuild_reasserts_focus_only() {
    let mut rig = Rig::new();
    let (c1, c2) = (rig.core.connect(), rig.core.connect());
    let a = rig.open(c1, 1, false);
    let b = rig.open(c2, 1, false);
    rig.request_ownership(c1, a);
    rig.effects();
    rig.started(b, 2);
    assert!(rig.effects().is_empty());
    rig.started(a, 2);
    assert_eq!(rig.commands(), [BackendCommand::Focus { attachment: a }]);
    rig.pull(c1);
    rig.started(a, 3);
    assert_eq!(rig.commands(), [BackendCommand::Focus { attachment: a }]);
}

#[test]
fn non_owner_requests_are_stored_not_forwarded() {
    let mut rig = Rig::new();
    let (c1, c2) = (rig.core.connect(), rig.core.connect());
    let a = rig.open(c1, 1, false);
    let b = rig.open(c2, 1, false);
    rig.request_ownership(c1, a);
    rig.effects();
    let generation = rig.generation(1);
    let other = combine_protocol::Display {
        foreground: "#000000".into(),
        ..display()
    };
    rig.send(
        c2,
        ClientMessage::Resize(Resize {
            attachment: b,
            cols: 132,
            rows: 50,
            ownership_generation: generation,
        }),
    );
    rig.focus(c2, b);
    rig.send(
        c2,
        ClientMessage::SetDisplay(SetDisplay {
            attachment: b,
            display: other.clone(),
        }),
    );
    assert!(rig.effects().is_empty());
    assert_eq!(rig.owner(1), Some(a));

    rig.request_ownership(c2, b);
    assert_eq!(
        rig.commands(),
        [
            BackendCommand::SetDisplay {
                attachment: b,
                display: other
            },
            BackendCommand::Resize {
                attachment: b,
                cols: 132,
                rows: 50
            },
            BackendCommand::Focus { attachment: b },
        ]
    );
    rig.request_ownership(c2, b);
    assert!(rig.effects().is_empty());
}

#[test]
fn owner_requests_are_forwarded() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let a = rig.open(c, 1, false);
    rig.request_ownership(c, a);
    rig.effects();
    rig.focus(c, a);
    let other = combine_protocol::Display {
        cell_width: 9,
        ..display()
    };
    rig.send(
        c,
        ClientMessage::SetDisplay(SetDisplay {
            attachment: a,
            display: other.clone(),
        }),
    );
    assert_eq!(
        rig.commands(),
        [
            BackendCommand::Focus { attachment: a },
            BackendCommand::SetDisplay {
                attachment: a,
                display: other
            }
        ]
    );
}

#[test]
fn automatic_reply_fence() {
    fn reply(id: AttachmentId, epoch: u64, generation: OwnershipGeneration) -> ClientMessage {
        ClientMessage::AutomaticReply(AutomaticReply {
            attachment: id,
            epoch: Epoch(epoch),
            ownership_generation: generation,
            bytes: b"\x1b[0n".to_vec(),
        })
    }
    let mut rig = Rig::new();
    let (c1, c2) = (rig.core.connect(), rig.core.connect());
    let owner = rig.open(c1, 1, false);
    let other = rig.open(c2, 1, false);
    let watcher = rig.open(c2, 1, true);
    rig.request_ownership(c1, owner);
    rig.effects();
    rig.pull(c1);
    let generation = rig.generation(1);
    let opening = rig.attach(c2, 5, 2, false);
    rig.pull(c2);
    rig.effects();

    let drops = [
        ("another connection", c2, reply(owner, 1, generation)),
        ("watch-only", c2, reply(watcher, 1, generation)),
        ("not live", c2, reply(opening, 1, rig.generation(2))),
        ("wrong epoch", c1, reply(owner, 2, generation)),
        (
            "stale generation",
            c1,
            reply(owner, 1, OwnershipGeneration(generation.0 - 1)),
        ),
        ("not the owner", c2, reply(other, 1, generation)),
    ];
    for (row, connection, message) in drops {
        rig.send(connection, message);
        assert!(rig.effects().is_empty(), "{row}");
        assert!(rig.pull(connection).is_empty(), "{row}");
        assert_eq!(rig.owner(1), Some(owner), "{row}");
        assert_eq!(rig.generation(1), generation, "{row}");
    }

    let mut quiet = Rig::with(
        CoreConfig::default(),
        Capabilities {
            automatic_replies: false,
            ..caps()
        },
    );
    let c = quiet.core.connect();
    let a = quiet.open(c, 1, false);
    quiet.request_ownership(c, a);
    quiet.effects();
    quiet.pull(c);
    quiet.send(c, reply(a, 1, quiet.generation(1)));
    assert!(quiet.effects().is_empty());
    assert!(quiet.pull(c).is_empty());

    rig.send(c1, reply(owner, 1, generation));
    assert_eq!(
        rig.commands(),
        [BackendCommand::WriteAutomatic {
            attachment: owner,
            bytes: b"\x1b[0n".to_vec()
        }]
    );
    assert!(rig.pull(c1).is_empty());
    assert_eq!(rig.generation(1), generation);

    rig.event(owner, 1, AttachEvent::Lost);
    rig.effects();
    rig.send(c1, reply(owner, 1, rig.generation(1)));
    assert!(rig.effects().is_empty());
}

#[test]
fn credit_flows_to_the_backend() {
    let config = CoreConfig {
        log_max_bytes: 8192,
        credit_reserve: 1024,
        credit_min_grant: 64,
        ..CoreConfig::default()
    };
    let credit = Capabilities {
        byte_credit: true,
        initial_credit: 1000,
        ..caps()
    };
    let mut rig = Rig::with(config, credit);
    let c = rig.core.connect();
    let id = rig.attach(c, 1, 1, false);
    rig.effects();
    rig.started(id, 1);
    assert_eq!(
        rig.commands(),
        [BackendCommand::Credit {
            attachment: id,
            bytes: 8192 - 64 - 1024 - 64 - 1000
        }]
    );
    rig.event(id, 1, AttachEvent::Data(vec![0; 500]));
    assert!(rig.effects().is_empty());
    rig.pull(c);
    rig.send(
        c,
        ClientMessage::Ack(Ack {
            cursor: cursor(id, 1, 1),
        }),
    );
    assert_eq!(
        rig.commands(),
        [BackendCommand::Credit {
            attachment: id,
            bytes: 564
        }]
    );
    let stats = rig.core.attachment_stats(id).unwrap();
    assert!(stats.peak_accounted_bytes <= 8192);
}

#[test]
fn unsent_ack_is_invalid_request() {
    let mut rig = Rig::new();
    let c = rig.core.connect();
    let id = rig.open(c, 1, false);
    rig.event(id, 1, AttachEvent::Data(b"x".to_vec()));
    rig.send(
        c,
        ClientMessage::Ack(Ack {
            cursor: cursor(id, 1, 1),
        }),
    );
    let messages = rig.pull(c);
    assert_eq!(names(&messages), ["error", "data"]);
    assert_eq!(error_of(&messages[0]).code, ErrorCode::InvalidRequest);
}

#[test]
fn resume_with_nothing_to_replay_grants_credit_back() {
    let config = CoreConfig {
        log_max_bytes: 8192,
        credit_reserve: 1024,
        credit_min_grant: 64,
        ..CoreConfig::default()
    };
    let credit = Capabilities {
        byte_credit: true,
        initial_credit: 1000,
        ..caps()
    };
    let mut rig = Rig::with(config, credit);
    let c1 = rig.core.connect();
    let id = rig.attach(c1, 1, 1, false);
    rig.started(id, 1);
    rig.effects();
    rig.pull(c1);
    // The backend spends every byte of credit: 1000 initial plus the 6040 granted.
    rig.event(id, 1, AttachEvent::Data(vec![0; 7040]));
    assert_eq!(rig.core.attachment_stats(id).unwrap().outstanding_credit, 0);
    let sent = rig.pull(c1);
    let last = sent.last().and_then(|m| m.cursor()).unwrap();
    assert!(rig.effects().is_empty());
    rig.core.disconnect(c1, rig.now());

    let c2 = rig.core.connect();
    rig.send(
        c2,
        ClientMessage::Resume(Resume {
            request: RequestId(2),
            cursor: last,
        }),
    );
    let messages = rig.pull(c2);
    let DaemonMessage::Resumed(resumed) = &messages[0] else {
        panic!("{messages:?}")
    };
    assert_eq!(resumed.replay_through, None);
    let commands = rig.commands();
    let [BackendCommand::Credit { attachment, bytes }] = commands.as_slice() else {
        panic!("{commands:?}");
    };
    assert_eq!(*attachment, id);
    assert!(*bytes >= 64);
}

#[test]
fn disconnect_hands_ownership_to_a_member_on_another_connection() {
    let mut rig = Rig::new();
    let (c1, c2) = (rig.core.connect(), rig.core.connect());
    let x = rig.open(c1, 1, false);
    let y = rig.open(c1, 1, false);
    let z = rig.open(c2, 1, false);
    rig.request_ownership(c1, x);
    rig.focus(c2, z);
    rig.focus(c1, y);
    rig.effects();
    rig.pull(c2);
    let before = rig.generation(1);

    rig.core.disconnect(c1, rig.now());
    assert_eq!(rig.owner(1), Some(z));
    assert_eq!(rig.generation(1).0, before.0 + 1);
    assert_eq!(
        rig.commands(),
        [
            BackendCommand::SetDisplay {
                attachment: z,
                display: display()
            },
            BackendCommand::Resize {
                attachment: z,
                cols: 80,
                rows: 24
            },
            BackendCommand::Focus { attachment: z },
        ]
    );
    assert_eq!(
        rig.pull(c2),
        [ownership_msg(z, rig.generation(1).0, Some(z))]
    );
    let _ = y;
}

#[test]
fn disconnect_without_another_member_leaves_no_owner_and_no_commands() {
    let mut rig = Rig::new();
    let c1 = rig.core.connect();
    let x = rig.open(c1, 1, false);
    let y = rig.open(c1, 1, false);
    rig.request_ownership(c1, x);
    rig.focus(c1, y);
    rig.effects();
    let before = rig.generation(1);

    rig.core.disconnect(c1, rig.now());
    assert_eq!(rig.owner(1), None);
    assert_eq!(rig.generation(1).0, before.0 + 1);
    assert!(rig.effects().is_empty());
}
