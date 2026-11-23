//! The attachment router: which panel stream owns which attachment, and where each daemon
//! message for an attachment goes.
//!
//! A panel stream asks for an attach or a resume and the router remembers which stream sent
//! each request. The daemon's reply carries the request ID, so the reply is delivered to that
//! stream and only when that request is still pending on it. A send that fails leaves no
//! pending entry, and a session's end clears every pending request and route.
//!
//! After `attached` or `resumed` the attachment is routed to its stream, and the attachment's
//! events go by the attachment ID in their cursor. A route stays until its stream forwards the
//! `ack` of the final event (`closed` or `lost`), or until the attachment's `gap` is delivered.
//!
//! The orphan boundary: Rust may send the final ack and detach only for an attachment whose
//! webview owner is gone (a reload, a closed window, or a closed panel stream), only to finish
//! it, and never for an attachment on an open panel stream. Otherwise Rust never resumes and
//! never acks, and it forwards acks untouched.
//!
//! Lock order is always the router, then [`Outbound`]. The sink is called under the router
//! lock, which keeps each stream's deliveries in order, and so it must never call back into
//! the router.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use combine_protocol::{
    Ack, Attach, AttachRequestOptions, AttachmentId, ClientMessage, Cursor, DaemonMessage, Detach,
    HostId, RequestId, Resume, TermId, MAX_AUTOMATIC_REPLY, MAX_INPUT_CHUNK,
};

use super::outbound::{NotConnected, Outbound, RequestKind};

/// Names one open panel stream. The app entry layer allocates these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamId(pub u64);

/// What a panel stream receives.
#[derive(Debug)]
pub enum StreamDelivery<'a> {
    /// Whether the stream's host has a live session. Sent when the stream opens and on each change.
    Connection {
        connected: bool,
    },
    Message(&'a DaemonMessage),
}

/// Where deliveries go. It is called under the router lock and must never call back into
/// [`Attachments`].
pub trait TerminalSink: Send + Sync {
    fn deliver(&self, stream: StreamId, event: StreamDelivery<'_>);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardError {
    NotConnected,
    UnknownStream,
    UnknownAttachment,
    TooLarge,
    Invalid,
}

impl From<NotConnected> for ForwardError {
    fn from(_: NotConnected) -> Self {
        ForwardError::NotConnected
    }
}

/// Who a request's reply, or an attachment's events, belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Stream(StreamId),
    /// The owning stream closed, so nobody receives the events. They are only finished.
    Orphan,
}

#[derive(Debug)]
struct Route {
    target: Target,
    detached: bool,
    /// The cursor of the `closed` or `lost` delivered for this attachment, once it has been.
    final_event: Option<Cursor>,
}

#[derive(Debug, Default)]
struct HostState {
    connected: bool,
    pending: BTreeMap<u64, Target>,
    routes: BTreeMap<u64, Route>,
    /// Counts the sessions started for this host, so a session's end can tell it was replaced.
    session: u64,
}

#[derive(Debug, Default)]
struct RouterState {
    hosts: BTreeMap<String, HostState>,
    streams: BTreeMap<StreamId, HostId>,
}

impl RouterState {
    fn host(&mut self, host_id: &HostId) -> &mut HostState {
        self.hosts.entry(host_id.0.clone()).or_default()
    }

    /// Turns everything owned by a stream in `closing` into orphans and detaches the attachments
    /// that are not detached yet.
    fn orphan(&mut self, outbound: &Outbound, closing: impl Fn(StreamId) -> bool) {
        for (host, state) in &mut self.hosts {
            let host_id = HostId(host.clone());
            let owned = |target: &Target| matches!(target, Target::Stream(s) if closing(*s));
            for target in state.pending.values_mut().filter(|t| owned(t)) {
                *target = Target::Orphan;
            }
            let mut finished = Vec::new();
            for (attachment, route) in state.routes.iter_mut().filter(|(_, r)| owned(&r.target)) {
                route.target = Target::Orphan;
                if route.detached {
                    continue;
                }
                if let Some(cursor) = route.final_event {
                    // The final event was delivered but never acked. The ack finishes the
                    // attachment on the daemon, and a detach would not.
                    let _ = outbound.send_untracked(&host_id, ClientMessage::Ack(Ack { cursor }));
                    finished.push(*attachment);
                } else {
                    let detach = ClientMessage::Detach(Detach {
                        attachment: AttachmentId(*attachment),
                    });
                    route.detached = outbound.send_untracked(&host_id, detach).is_ok();
                }
            }
            for attachment in finished {
                state.routes.remove(&attachment);
            }
        }
    }
}

pub struct Attachments {
    sink: Arc<dyn TerminalSink>,
    state: Mutex<RouterState>,
}

impl Attachments {
    pub fn new(sink: Arc<dyn TerminalSink>) -> Self {
        Attachments {
            sink,
            state: Mutex::new(RouterState::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, RouterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn deliver(&self, target: Target, message: &DaemonMessage) {
        if let Target::Stream(stream) = target {
            self.sink.deliver(stream, StreamDelivery::Message(message));
        }
    }

    /// Records the stream's host and tells the stream whether that host is connected.
    pub fn open_stream(&self, stream: StreamId, host_id: HostId) {
        let mut state = self.state();
        let connected = state.host(&host_id).connected;
        state.streams.insert(stream, host_id);
        self.sink
            .deliver(stream, StreamDelivery::Connection { connected });
    }

    /// Closes a stream. Whatever it still owns becomes an orphan, and is finished.
    pub fn close_stream(&self, outbound: &Outbound, stream: StreamId) {
        let mut state = self.state();
        if state.streams.remove(&stream).is_some() {
            state.orphan(outbound, |s| s == stream);
        }
    }

    /// Closes every stream: the webview that owned them reloaded or closed.
    pub fn close_all_streams(&self, outbound: &Outbound) {
        let mut state = self.state();
        state.streams.clear();
        state.orphan(outbound, |_| true);
    }

    /// Sends an `attach` for `stream`. Its reply is delivered to the stream only while the request
    /// is pending.
    pub fn attach(
        &self,
        outbound: &Outbound,
        stream: StreamId,
        terminal: TermId,
        options: AttachRequestOptions,
    ) -> Result<(), ForwardError> {
        self.request(outbound, stream, RequestKind::Attach, |request| {
            ClientMessage::Attach(Attach {
                request,
                terminal,
                options,
            })
        })
    }

    /// Sends a `resume` for `stream`, from the cursor the webview last applied.
    pub fn resume(
        &self,
        outbound: &Outbound,
        stream: StreamId,
        cursor: Cursor,
    ) -> Result<(), ForwardError> {
        self.request(outbound, stream, RequestKind::Resume, |request| {
            ClientMessage::Resume(Resume { request, cursor })
        })
    }

    fn request(
        &self,
        outbound: &Outbound,
        stream: StreamId,
        kind: RequestKind,
        build: impl FnOnce(RequestId) -> ClientMessage,
    ) -> Result<(), ForwardError> {
        let mut state = self.state();
        let host_id = state
            .streams
            .get(&stream)
            .cloned()
            .ok_or(ForwardError::UnknownStream)?;
        let host = state.host(&host_id);
        if !host.connected {
            return Err(ForwardError::NotConnected);
        }
        let request = outbound.send(&host_id, kind, build)?;
        host.pending.insert(request.0, Target::Stream(stream));
        Ok(())
    }

    /// Forwards a message from `stream` to its host. Only the messages an attached panel sends
    /// are accepted, and only for an attachment routed to this stream.
    pub fn forward(
        &self,
        outbound: &Outbound,
        stream: StreamId,
        message: ClientMessage,
    ) -> Result<(), ForwardError> {
        let attachment = match &message {
            ClientMessage::Detach(m) => m.attachment,
            ClientMessage::Ack(m) => m.cursor.attachment,
            ClientMessage::Resize(m) => m.attachment,
            ClientMessage::Focus(m) => m.attachment,
            ClientMessage::SetDisplay(m) => m.attachment,
            ClientMessage::RequestOwnership(m) => m.attachment,
            ClientMessage::Input(m) => m.attachment,
            ClientMessage::AutomaticReply(m) => m.attachment,
            _ => return Err(ForwardError::Invalid),
        };
        let mut state = self.state();
        let host_id = state
            .streams
            .get(&stream)
            .cloned()
            .ok_or(ForwardError::UnknownStream)?;
        let route = state
            .host(&host_id)
            .routes
            .get_mut(&attachment.0)
            .filter(|route| route.target == Target::Stream(stream))
            .ok_or(ForwardError::UnknownAttachment)?;
        match message {
            ClientMessage::Input(input) => {
                for chunk in input.bytes.chunks(MAX_INPUT_CHUNK) {
                    let chunk = combine_protocol::Input {
                        attachment,
                        bytes: chunk.to_vec(),
                    };
                    outbound.send_untracked(&host_id, ClientMessage::Input(chunk))?;
                }
            }
            ClientMessage::AutomaticReply(reply) => {
                if reply.bytes.len() > MAX_AUTOMATIC_REPLY {
                    return Err(ForwardError::TooLarge);
                }
                outbound.send_untracked(&host_id, ClientMessage::AutomaticReply(reply))?;
            }
            ClientMessage::Detach(detach) => {
                outbound.send_untracked(&host_id, ClientMessage::Detach(detach))?;
                route.detached = true;
            }
            ClientMessage::Ack(Ack { cursor }) => {
                outbound.send_untracked(&host_id, ClientMessage::Ack(Ack { cursor }))?;
                if route.final_event == Some(cursor) {
                    state.host(&host_id).routes.remove(&attachment.0);
                }
            }
            other => outbound.send_untracked(&host_id, other)?,
        }
        Ok(())
    }

    /// A session for `host_id` is registered: nothing from an earlier session survives. The
    /// returned number names this session for [`Attachments::session_ended_by`].
    pub fn session_started(&self, host_id: &HostId) -> u64 {
        self.set_connected(host_id, true, None).unwrap_or_default()
    }

    /// The session for `host_id` is gone, and so are its pending requests and attachments.
    pub fn session_ended(&self, host_id: &HostId) {
        self.set_connected(host_id, false, None);
    }

    /// Ends the session that `session_started` numbered `session`, unless a newer session for
    /// the host has started since: that session's state is not this one's to clear.
    pub fn session_ended_by(&self, host_id: &HostId, session: u64) {
        self.set_connected(host_id, false, Some(session));
    }

    fn set_connected(&self, host_id: &HostId, connected: bool, only: Option<u64>) -> Option<u64> {
        let mut state = self.state();
        let host = state.host(host_id);
        if only.is_some_and(|session| session != host.session) {
            return None;
        }
        if connected {
            host.session += 1;
        }
        let session = host.session;
        host.connected = connected;
        host.pending.clear();
        host.routes.clear();
        for (stream, _) in state.streams.iter().filter(|(_, host)| *host == host_id) {
            self.sink
                .deliver(*stream, StreamDelivery::Connection { connected });
        }
        Some(session)
    }

    /// Routes a daemon message. `None` means the router consumed it. Anything that is not an
    /// attachment's traffic comes back untouched.
    pub fn route(
        &self,
        outbound: &Outbound,
        host_id: &HostId,
        message: DaemonMessage,
    ) -> Option<DaemonMessage> {
        let mut state = self.state();
        let host = state.host(host_id);
        if self.dispatch(outbound, host_id, host, &message) {
            None
        } else {
            Some(message)
        }
    }

    /// Returns whether the router consumed `message`.
    fn dispatch(
        &self,
        outbound: &Outbound,
        host_id: &HostId,
        host: &mut HostState,
        message: &DaemonMessage,
    ) -> bool {
        match message {
            DaemonMessage::Attached(m) => {
                self.opened(outbound, host_id, host, m.request, m.attachment, message);
            }
            DaemonMessage::Resumed(m) => {
                self.opened(outbound, host_id, host, m.request, m.attachment, message);
            }
            DaemonMessage::Gap(gap) => match gap.request {
                Some(request) => {
                    if let Some(target) = host.pending.remove(&request.0) {
                        self.deliver(target, message);
                        host.routes.remove(&gap.attachment.0);
                    }
                }
                None => {
                    if let Some(route) = host.routes.remove(&gap.attachment.0) {
                        self.deliver(route.target, message);
                    }
                }
            },
            DaemonMessage::Ownership(m) => {
                if let Some(route) = host.routes.get(&m.attachment.0) {
                    self.deliver(route.target, message);
                }
            }
            DaemonMessage::Error(e) => {
                if let Some(request) = e.request {
                    match host.pending.remove(&request.0) {
                        Some(target) => self.deliver(target, message),
                        None => return false,
                    }
                } else if let Some(attachment) = e.attachment {
                    if let Some(route) = host.routes.get(&attachment.0) {
                        self.deliver(route.target, message);
                    }
                } else {
                    return false;
                }
            }
            _ => match message.cursor() {
                Some(cursor) => self.event(outbound, host_id, host, cursor, message),
                None => return false,
            },
        }
        true
    }

    fn opened(
        &self,
        outbound: &Outbound,
        host_id: &HostId,
        host: &mut HostState,
        request: RequestId,
        attachment: AttachmentId,
        message: &DaemonMessage,
    ) {
        let Some(target) = host.pending.remove(&request.0) else {
            return;
        };
        let mut route = Route {
            target,
            detached: false,
            final_event: None,
        };
        match target {
            Target::Stream(_) => self.deliver(target, message),
            Target::Orphan => {
                let detach = ClientMessage::Detach(Detach { attachment });
                route.detached = outbound.send_untracked(host_id, detach).is_ok();
            }
        }
        host.routes.insert(attachment.0, route);
    }

    fn event(
        &self,
        outbound: &Outbound,
        host_id: &HostId,
        host: &mut HostState,
        cursor: Cursor,
        message: &DaemonMessage,
    ) {
        let is_final = matches!(message, DaemonMessage::Closed(_) | DaemonMessage::Lost(_));
        let Some(route) = host.routes.get_mut(&cursor.attachment.0) else {
            return;
        };
        match route.target {
            Target::Stream(_) => {
                if is_final {
                    route.final_event = Some(cursor);
                }
                self.deliver(route.target, message);
            }
            Target::Orphan if is_final => {
                let _ = outbound.send_untracked(host_id, ClientMessage::Ack(Ack { cursor }));
                host.routes.remove(&cursor.attachment.0);
            }
            Target::Orphan => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use combine_protocol::{Attached, BackendGeneration, Fidelity};
    use combine_protocol::{
        AutomaticReply, CloseReason, Closed, Data, Display, Epoch, ErrorCode, ErrorMessage, Focus,
        Gap, Input, Lost, Ownership, OwnershipGeneration, Resumed, Seq, Started, Terminals,
    };

    use super::super::outbound::{Outgoing, Registration};
    use super::super::testing::{host_id, HOST_A};
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Seen {
        Connection(bool),
        Message(DaemonMessage),
    }

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<(StreamId, Seen)>>);

    impl TerminalSink for RecordingSink {
        fn deliver(&self, stream: StreamId, event: StreamDelivery<'_>) {
            let seen = match event {
                StreamDelivery::Connection { connected } => Seen::Connection(connected),
                StreamDelivery::Message(m) => Seen::Message(m.clone()),
            };
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((stream, seen));
        }
    }

    struct Rig {
        router: Attachments,
        sink: Arc<RecordingSink>,
        outbound: Arc<Outbound>,
        host: HostId,
        rx: Receiver<Outgoing>,
        registration: Option<Registration>,
    }

    const S1: StreamId = StreamId(1);
    const S2: StreamId = StreamId(2);

    fn connect(
        outbound: &Arc<Outbound>,
        router: &Attachments,
        host: &HostId,
    ) -> (Receiver<Outgoing>, Registration) {
        let (tx, rx) = mpsc::channel();
        let registration = outbound.register(host, tx);
        router.session_started(host);
        (rx, registration)
    }

    impl Rig {
        fn new() -> Rig {
            let sink = Arc::new(RecordingSink::default());
            let router = Attachments::new(sink.clone());
            let outbound = Arc::new(Outbound::default());
            let host = host_id(HOST_A);
            let (rx, registration) = connect(&outbound, &router, &host);
            Rig {
                router,
                sink,
                outbound,
                host,
                rx,
                registration: Some(registration),
            }
        }

        fn with_streams() -> Rig {
            let rig = Rig::new();
            rig.router.open_stream(S1, rig.host.clone());
            rig.router.open_stream(S2, rig.host.clone());
            rig.seen();
            rig
        }

        fn seen(&self) -> Vec<(StreamId, Seen)> {
            std::mem::take(&mut *self.sink.0.lock().expect("sink"))
        }

        fn sent(&self) -> Vec<ClientMessage> {
            self.rx
                .try_iter()
                .filter_map(|o| match o {
                    Outgoing::Message(m) => Some(m),
                    Outgoing::Stop => None,
                })
                .collect()
        }

        fn route(&self, message: DaemonMessage) -> Option<DaemonMessage> {
            self.router.route(&self.outbound, &self.host, message)
        }

        fn attach(&self, stream: StreamId) -> Result<(), ForwardError> {
            self.router
                .attach(&self.outbound, stream, TermId("t1".to_string()), options())
        }

        fn forward(&self, stream: StreamId, message: ClientMessage) -> Result<(), ForwardError> {
            self.router.forward(&self.outbound, stream, message)
        }

        /// Attaches `stream` and answers with `attached`, leaving the attachment routed to it.
        fn attached(&self, stream: StreamId, attachment: u64) {
            self.attach(stream).expect("attach");
            let request = match self.sent().pop() {
                Some(ClientMessage::Attach(a)) => a.request,
                other => panic!("expected an attach, got {other:?}"),
            };
            assert!(self
                .route(DaemonMessage::Attached(Attached {
                    request,
                    attachment: AttachmentId(attachment),
                }))
                .is_none());
            self.seen();
        }
    }

    fn options() -> AttachRequestOptions {
        AttachRequestOptions {
            cols: 80,
            rows: 24,
            display: Display {
                foreground: "#ffffff".to_string(),
                background: "#000000".to_string(),
                cursor_color: "#ffffff".to_string(),
                cell_width: 8,
                cell_height: 16,
            },
            observe: false,
            history: 1000,
        }
    }

    fn cursor(attachment: u64, seq: u64) -> Cursor {
        Cursor {
            attachment: AttachmentId(attachment),
            epoch: Epoch(1),
            seq: Seq(seq),
        }
    }

    fn started(attachment: u64) -> DaemonMessage {
        DaemonMessage::Started(Started {
            cursor: cursor(attachment, 0),
            backend_generation: BackendGeneration(1),
            cols: 80,
            rows: 24,
            fidelity: Fidelity::Exact,
            ordered_resizes: true,
        })
    }

    fn data(attachment: u64, seq: u64) -> DaemonMessage {
        DaemonMessage::Data(Data {
            cursor: cursor(attachment, seq),
            bytes: vec![b'x'],
        })
    }

    fn closed(attachment: u64, seq: u64) -> DaemonMessage {
        DaemonMessage::Closed(Closed {
            cursor: cursor(attachment, seq),
            reason: CloseReason::Detached,
        })
    }

    fn lost(attachment: u64, seq: u64) -> DaemonMessage {
        DaemonMessage::Lost(Lost {
            cursor: cursor(attachment, seq),
        })
    }

    fn error(request: Option<u64>, attachment: Option<u64>) -> DaemonMessage {
        DaemonMessage::Error(ErrorMessage {
            code: ErrorCode::Internal,
            message: "boom".to_string(),
            request: request.map(RequestId),
            attachment: attachment.map(AttachmentId),
        })
    }

    fn gap(attachment: u64, request: Option<u64>) -> DaemonMessage {
        DaemonMessage::Gap(Gap {
            attachment: AttachmentId(attachment),
            request: request.map(RequestId),
        })
    }

    fn attached_reply(request: u64, attachment: u64) -> DaemonMessage {
        DaemonMessage::Attached(Attached {
            request: RequestId(request),
            attachment: AttachmentId(attachment),
        })
    }

    #[test]
    fn attach_reply_routes_to_its_stream() {
        let rig = Rig::with_streams();
        rig.attach(S1).expect("attach");
        rig.attach(S2).expect("attach");
        let requests: Vec<_> = rig
            .sent()
            .into_iter()
            .map(|m| match m {
                ClientMessage::Attach(a) => a.request.0,
                other => panic!("expected an attach, got {other:?}"),
            })
            .collect();
        assert_eq!(requests, [1, 2]);
        assert_eq!(
            rig.outbound.kind_of(&rig.host, RequestId(1)),
            Some(RequestKind::Attach)
        );
        assert!(rig.route(attached_reply(2, 20)).is_none());
        assert_eq!(rig.seen(), [(S2, Seen::Message(attached_reply(2, 20)))]);
        assert!(rig.route(attached_reply(1, 10)).is_none());
        assert_eq!(rig.seen(), [(S1, Seen::Message(attached_reply(1, 10)))]);
        rig.forward(
            S1,
            ClientMessage::Focus(Focus {
                attachment: AttachmentId(10),
            }),
        )
        .expect("routed to S1");
        assert_eq!(
            rig.forward(
                S1,
                ClientMessage::Focus(Focus {
                    attachment: AttachmentId(20),
                }),
            ),
            Err(ForwardError::UnknownAttachment)
        );
    }

    #[test]
    fn close_stream_detaches_once_and_acks_orphan_final_event() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.router.close_stream(&rig.outbound, S1);
        assert_eq!(
            rig.sent(),
            [ClientMessage::Detach(Detach {
                attachment: AttachmentId(10)
            })]
        );
        rig.router.close_stream(&rig.outbound, S1);
        assert!(rig.sent().is_empty());
        assert!(rig.route(data(10, 1)).is_none());
        assert!(rig.sent().is_empty());
        assert!(rig.route(closed(10, 2)).is_none());
        assert_eq!(
            rig.sent(),
            [ClientMessage::Ack(Ack {
                cursor: cursor(10, 2)
            })]
        );
        assert!(rig.seen().is_empty());
        assert!(rig.route(lost(10, 3)).is_none());
        assert!(rig.sent().is_empty());
    }

    #[test]
    fn a_reply_to_an_attach_whose_stream_closed_is_detached_not_delivered() {
        let rig = Rig::with_streams();
        rig.attach(S1).expect("attach");
        rig.sent();
        rig.router.close_stream(&rig.outbound, S1);
        assert!(rig.route(attached_reply(1, 10)).is_none());
        assert!(rig.seen().is_empty());
        assert_eq!(
            rig.sent(),
            [ClientMessage::Detach(Detach {
                attachment: AttachmentId(10)
            })]
        );
    }

    #[test]
    fn acks_are_forwarded_untouched() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.route(data(10, 1));
        let ack = ClientMessage::Ack(Ack {
            cursor: cursor(10, 1),
        });
        rig.forward(S1, ack.clone()).expect("ack");
        rig.forward(S1, ack.clone()).expect("a repeated ack");
        assert_eq!(rig.sent(), [ack.clone(), ack]);
        assert!(rig.route(data(10, 2)).is_none());
        assert_eq!(rig.seen().len(), 2);
        assert!(rig.sent().is_empty());
    }

    #[test]
    fn stream_events_route_by_attachment_in_order() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.attached(S2, 20);
        for message in [started(10), data(20, 1), data(10, 1), data(10, 2)] {
            assert!(rig.route(message).is_none());
        }
        assert_eq!(
            rig.seen(),
            [
                (S1, Seen::Message(started(10))),
                (S2, Seen::Message(data(20, 1))),
                (S1, Seen::Message(data(10, 1))),
                (S1, Seen::Message(data(10, 2))),
            ]
        );
        assert!(rig.route(data(99, 1)).is_none());
        assert!(rig.seen().is_empty());
    }

    #[test]
    fn route_stays_until_the_final_event_is_acked() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.route(closed(10, 1));
        let ack = ClientMessage::Ack(Ack {
            cursor: cursor(10, 1),
        });
        rig.forward(S1, ack.clone()).expect("the final ack");
        assert_eq!(rig.forward(S1, ack), Err(ForwardError::UnknownAttachment));
    }

    #[test]
    fn resume_gap_routes_by_request_and_finishes_route() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.router
            .resume(&rig.outbound, S1, cursor(10, 5))
            .expect("resume");
        assert!(matches!(
            rig.sent().as_slice(),
            [ClientMessage::Resume(r)] if r.request == RequestId(2) && r.cursor == cursor(10, 5)
        ));
        assert_eq!(
            rig.outbound.kind_of(&rig.host, RequestId(2)),
            Some(RequestKind::Resume)
        );
        assert!(rig.route(gap(10, Some(2))).is_none());
        assert_eq!(rig.seen(), [(S1, Seen::Message(gap(10, Some(2))))]);
        assert_eq!(
            rig.forward(
                S1,
                ClientMessage::Focus(Focus {
                    attachment: AttachmentId(10)
                })
            ),
            Err(ForwardError::UnknownAttachment)
        );
    }

    #[test]
    fn resumed_reply_routes_to_its_stream() {
        let rig = Rig::with_streams();
        rig.router
            .resume(&rig.outbound, S2, cursor(10, 5))
            .expect("resume");
        rig.sent();
        let resumed = DaemonMessage::Resumed(Resumed {
            request: RequestId(1),
            attachment: AttachmentId(10),
            replay_through: None,
        });
        assert!(rig.route(resumed.clone()).is_none());
        assert_eq!(rig.seen(), [(S2, Seen::Message(resumed))]);
        assert!(rig.route(data(10, 6)).is_none());
        assert_eq!(rig.seen(), [(S2, Seen::Message(data(10, 6)))]);
    }

    #[test]
    fn in_order_gap_finishes_route() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.route(data(10, 1));
        assert!(rig.route(gap(10, None)).is_none());
        assert_eq!(
            rig.seen(),
            [
                (S1, Seen::Message(data(10, 1))),
                (S1, Seen::Message(gap(10, None)))
            ]
        );
        assert!(rig.route(data(10, 2)).is_none());
        assert!(rig.seen().is_empty());
    }

    #[test]
    fn error_routes_by_request_or_attachment() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.attach(S2).expect("attach");
        rig.sent();
        assert!(rig.route(error(None, Some(10))).is_none());
        assert_eq!(rig.seen(), [(S1, Seen::Message(error(None, Some(10))))]);
        assert!(rig.route(error(Some(2), None)).is_none());
        assert_eq!(rig.seen(), [(S2, Seen::Message(error(Some(2), None)))]);
        assert!(rig.route(error(Some(2), None)).is_some());
        let owner = DaemonMessage::Ownership(Ownership {
            attachment: AttachmentId(10),
            ownership_generation: OwnershipGeneration(1),
            owner: Some(AttachmentId(10)),
        });
        assert!(rig.route(owner.clone()).is_none());
        assert_eq!(rig.seen(), [(S1, Seen::Message(owner))]);
    }

    #[test]
    fn unrelated_messages_are_returned() {
        let rig = Rig::with_streams();
        let terminals = DaemonMessage::Terminals(Terminals {
            request: None,
            terminals: Vec::new(),
            spaces: Vec::new(),
        });
        assert_eq!(rig.route(terminals.clone()), Some(terminals));
        assert_eq!(rig.route(error(None, None)), Some(error(None, None)));
        assert_eq!(rig.route(error(Some(7), None)), Some(error(Some(7), None)));
        assert!(rig.seen().is_empty());
    }

    #[test]
    fn forward_rejects_attachment_of_another_stream() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        let focus = |attachment| {
            ClientMessage::Focus(Focus {
                attachment: AttachmentId(attachment),
            })
        };
        assert_eq!(
            rig.forward(S2, focus(10)),
            Err(ForwardError::UnknownAttachment)
        );
        assert_eq!(
            rig.forward(StreamId(9), focus(10)),
            Err(ForwardError::UnknownStream)
        );
        assert_eq!(
            rig.forward(
                S1,
                ClientMessage::WatchTerminals(combine_protocol::WatchTerminals {
                    request: RequestId(1)
                })
            ),
            Err(ForwardError::Invalid)
        );
        assert!(rig.sent().is_empty());
    }

    #[test]
    fn input_is_split_into_max_input_chunk_frames_in_order() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        let mut bytes = vec![1; MAX_INPUT_CHUNK];
        bytes.extend([2; 3]);
        rig.forward(
            S1,
            ClientMessage::Input(Input {
                attachment: AttachmentId(10),
                bytes,
            }),
        )
        .expect("input");
        let sent = rig.sent();
        let lengths: Vec<_> = sent
            .iter()
            .map(|m| match m {
                ClientMessage::Input(i) => (i.attachment.0, i.bytes.len(), i.bytes[0]),
                other => panic!("expected input, got {other:?}"),
            })
            .collect();
        assert_eq!(lengths, [(10, MAX_INPUT_CHUNK, 1), (10, 3, 2)]);
    }

    #[test]
    fn automatic_reply_over_the_limit_is_too_large() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        let reply = |len| {
            ClientMessage::AutomaticReply(AutomaticReply {
                attachment: AttachmentId(10),
                epoch: Epoch(1),
                ownership_generation: OwnershipGeneration(1),
                bytes: vec![0; len],
            })
        };
        assert_eq!(
            rig.forward(S1, reply(MAX_AUTOMATIC_REPLY + 1)),
            Err(ForwardError::TooLarge)
        );
        assert!(rig.sent().is_empty());
        rig.forward(S1, reply(MAX_AUTOMATIC_REPLY))
            .expect("at the limit");
        assert_eq!(rig.sent().len(), 1);
    }

    #[test]
    fn no_detach_or_ack_for_an_attachment_routed_to_an_open_stream() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.route(started(10));
        rig.route(data(10, 1));
        rig.route(closed(10, 2));
        rig.router.close_stream(&rig.outbound, S2);
        rig.router.open_stream(S2, rig.host.clone());
        assert!(rig.sent().is_empty());
        let sent_by_the_panel = ClientMessage::Detach(Detach {
            attachment: AttachmentId(10),
        });
        rig.forward(S1, sent_by_the_panel.clone()).expect("detach");
        rig.forward(S1, sent_by_the_panel.clone()).expect("detach");
        assert_eq!(rig.sent(), [sent_by_the_panel.clone(), sent_by_the_panel]);
        rig.router.close_stream(&rig.outbound, S1);
        assert!(rig.sent().is_empty());
    }

    #[test]
    fn close_all_streams_orphans_every_attachment() {
        let rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.attached(S2, 20);
        rig.router.close_all_streams(&rig.outbound);
        let mut detached: Vec<_> = rig
            .sent()
            .into_iter()
            .map(|m| match m {
                ClientMessage::Detach(d) => d.attachment.0,
                other => panic!("expected detach, got {other:?}"),
            })
            .collect();
        detached.sort_unstable();
        assert_eq!(detached, [10, 20]);
        assert_eq!(rig.attach(S1), Err(ForwardError::UnknownStream));
    }

    #[test]
    fn session_start_reports_connected() {
        let sink = Arc::new(RecordingSink::default());
        let router = Attachments::new(sink.clone());
        let outbound = Arc::new(Outbound::default());
        let host = host_id(HOST_A);
        router.open_stream(S1, host.clone());
        assert_eq!(
            router.attach(&outbound, S1, TermId("t".to_string()), options()),
            Err(ForwardError::NotConnected)
        );
        let (_rx, _registration) = connect(&outbound, &router, &host);
        router.open_stream(S2, host);
        let seen = std::mem::take(&mut *sink.0.lock().expect("sink"));
        assert_eq!(
            seen,
            [
                (S1, Seen::Connection(false)),
                (S1, Seen::Connection(true)),
                (S2, Seen::Connection(true)),
            ]
        );
    }

    #[test]
    fn session_end_clears_routes_and_reports_disconnected() {
        let mut rig = Rig::with_streams();
        rig.attached(S1, 10);
        rig.registration = None;
        rig.router.session_ended(&rig.host);
        assert_eq!(
            rig.seen(),
            [(S1, Seen::Connection(false)), (S2, Seen::Connection(false))]
        );
        assert_eq!(
            rig.forward(
                S1,
                ClientMessage::Focus(Focus {
                    attachment: AttachmentId(10)
                })
            ),
            Err(ForwardError::UnknownAttachment)
        );
        assert_eq!(rig.attach(S1), Err(ForwardError::NotConnected));
        assert!(rig.route(data(10, 1)).is_none());
        assert!(rig.seen().is_empty());
    }

    #[test]
    fn a_failed_send_leaves_no_pending_request() {
        let mut rig = Rig::with_streams();
        let (tx, rx) = mpsc::channel();
        drop(rx);
        rig.registration = Some(rig.outbound.register(&rig.host, tx));
        assert_eq!(rig.attach(S1), Err(ForwardError::NotConnected));
        assert!(rig.route(attached_reply(1, 10)).is_none());
        assert!(rig.seen().is_empty());
        assert!(rig
            .route(error(Some(1), None))
            .is_some_and(|m| m == error(Some(1), None)));
    }

    #[test]
    fn a_reply_from_an_earlier_session_is_not_delivered() {
        let mut rig = Rig::with_streams();
        rig.attach(S1).expect("attach");
        rig.registration = None;
        rig.router.session_ended(&rig.host);
        let (rx, registration) = connect(&rig.outbound, &rig.router, &rig.host);
        rig.rx = rx;
        rig.registration = Some(registration);
        rig.seen();
        assert!(rig.route(attached_reply(1, 10)).is_none());
        assert!(rig.route(gap(10, Some(1))).is_none());
        assert!(rig.seen().is_empty());
        assert_eq!(
            rig.forward(
                S1,
                ClientMessage::Focus(Focus {
                    attachment: AttachmentId(10)
                })
            ),
            Err(ForwardError::UnknownAttachment)
        );
    }

    #[test]
    fn a_reply_is_delivered_once_and_only_to_the_stream_that_asked() {
        let rig = Rig::with_streams();
        rig.attach(S1).expect("attach");
        assert!(rig.route(attached_reply(1, 10)).is_none());
        assert_eq!(rig.seen(), [(S1, Seen::Message(attached_reply(1, 10)))]);
        assert!(rig.route(attached_reply(1, 11)).is_none());
        assert!(rig.seen().is_empty());
        assert_eq!(
            rig.forward(
                S1,
                ClientMessage::Focus(Focus {
                    attachment: AttachmentId(11)
                })
            ),
            Err(ForwardError::UnknownAttachment)
        );
    }

    #[test]
    fn closing_a_stream_after_the_final_event_sends_the_final_ack_and_no_detach() {
        for final_event in [closed(10, 3), lost(10, 3)] {
            let rig = Rig::with_streams();
            rig.attached(S1, 10);
            assert!(rig.route(final_event).is_none());
            rig.seen();
            rig.router.close_stream(&rig.outbound, S1);
            assert_eq!(
                rig.sent(),
                [ClientMessage::Ack(Ack {
                    cursor: cursor(10, 3)
                })]
            );
            assert!(rig.route(data(10, 4)).is_none());
            assert!(rig.sent().is_empty());
            assert!(rig.router.state().host(&rig.host).routes.is_empty());
        }
    }
}
