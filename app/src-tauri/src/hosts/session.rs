//! The live session phase: subscribes to the host's terminals, publishes them as spaces, and
//! runs the writer that the outbound channel feeds.

use std::io::Write;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

use combine_protocol::{
    read_message, write_message, ClientMessage, DaemonMessage, ErrorCode, HostId, LayoutSnapshot,
    Panels, ReadError, RequestId, WatchTerminals,
};

use super::attachments::Attachments;
use super::outbound::{Outbound, Outgoing, RequestKind};
use super::panels::PanelReply;
use super::spawn::SpawnReply;
use super::supervisor::{SessionEnd, SessionIo, SessionPhase};
use super::terminals::{backend_view, space_views};
use super::text::display_text;

/// Where a host's layout traffic goes, with the panel list that rides the layout watch. The
/// methods mirror the layout and panel methods of [`HostEvents`](super::supervisor::HostEvents).
pub trait LayoutSink: Send + Sync {
    fn layout(&self, host_id: &HostId, request: Option<RequestId>, snapshot: LayoutSnapshot);
    fn layout_error(&self, host_id: &HostId, request: RequestId, code: ErrorCode, message: String);
    fn layout_disconnected(&self, host_id: &HostId);
    fn panels(&self, _host_id: &HostId, _panels: Panels) {}
    fn panel_reply(&self, _host_id: &HostId, _reply: PanelReply) {}
}

pub struct LivePhase {
    outbound: Arc<Outbound>,
    attachments: Arc<Attachments>,
}

impl LivePhase {
    pub fn new(outbound: Arc<Outbound>, attachments: Arc<Attachments>) -> Self {
        LivePhase {
            outbound,
            attachments,
        }
    }
}

/// Writes queued messages until `Stop` or a write error.
fn writer_loop(writer: &mut (dyn Write + Send), rx: Receiver<Outgoing>) {
    while let Ok(Outgoing::Message(message)) = rx.recv() {
        if write_message(&mut *writer, &message).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

impl SessionPhase for LivePhase {
    fn run(&mut self, io: SessionIo<'_>) -> SessionEnd {
        let SessionIo {
            host_id,
            reader,
            writer,
            events,
        } = io;
        let (tx, rx) = mpsc::channel();
        let registration = self.outbound.register(host_id, tx.clone());
        // Panel streams stay disconnected until the first `terminals` reply names the daemon's
        // backend instance: a pinned panel must not attach or resume before it knows the
        // session's instance, whatever the registry still shows from the last session.
        let mut session: Option<u64> = None;
        thread::scope(|scope| {
            let writer_thread = scope.spawn(move || writer_loop(writer, rx));
            let sent = self
                .outbound
                .send(host_id, RequestKind::Terminals, |request| {
                    ClientMessage::WatchTerminals(WatchTerminals { request })
                });
            let end = if sent.is_err() {
                SessionEnd::Io("the writer stopped".to_string())
            } else {
                loop {
                    let message = match read_message::<DaemonMessage, _>(&mut *reader) {
                        Ok(None) => break SessionEnd::Closed,
                        Ok(Some(message)) => message,
                        Err(ReadError::Io(e)) => break SessionEnd::Io(e.to_string()),
                        Err(ReadError::Decode(e)) => break SessionEnd::Protocol(e.to_string()),
                    };
                    let Some(message) = self.attachments.route(&self.outbound, host_id, message)
                    else {
                        continue;
                    };
                    match message {
                        DaemonMessage::Terminals(list) => {
                            if session.is_none() {
                                let instance =
                                    list.backend.as_ref().and_then(|b| b.instance.clone());
                                session = Some(self.attachments.session_started(host_id, instance));
                            }
                            events.spaces(
                                host_id,
                                space_views(host_id, &list),
                                backend_view(&list),
                            );
                        }
                        DaemonMessage::Layout(reply) => {
                            events.layout(host_id, reply.request, reply.layout);
                        }
                        DaemonMessage::Panels(panels) => events.panels(host_id, panels),
                        DaemonMessage::Panel(panel) => {
                            events.panel_reply(host_id, PanelReply::Panel(panel));
                        }
                        DaemonMessage::PanelClosed(closed) => {
                            events.panel_reply(host_id, PanelReply::Closed(closed));
                        }
                        DaemonMessage::PanelPinned(pinned) => {
                            events.panel_reply(host_id, PanelReply::Pinned(pinned));
                        }
                        DaemonMessage::Templates(templates) => {
                            events.spawn_reply(host_id, SpawnReply::Templates(templates));
                        }
                        DaemonMessage::Spawned(spawned) => {
                            events.spawn_reply(host_id, SpawnReply::Spawned(spawned));
                        }
                        DaemonMessage::Error(error) => {
                            if let Some(request) = error.request {
                                match self.outbound.kind_of(host_id, request) {
                                    Some(RequestKind::Layout) => events.layout_error(
                                        host_id,
                                        request,
                                        error.code,
                                        display_text(error.trimmed_message()),
                                    ),
                                    Some(RequestKind::Panel) => events.panel_reply(
                                        host_id,
                                        PanelReply::Error {
                                            request,
                                            code: error.code,
                                            message: display_text(error.trimmed_message()),
                                        },
                                    ),
                                    Some(RequestKind::Spawn) => events.spawn_reply(
                                        host_id,
                                        SpawnReply::Error {
                                            request,
                                            code: error.code,
                                            message: display_text(error.trimmed_message()),
                                        },
                                    ),
                                    // The list can't be had, so no instance is known; panels
                                    // that follow the active terminal may still attach.
                                    Some(RequestKind::Terminals) => {
                                        if session.is_none() {
                                            session = Some(
                                                self.attachments.session_started(host_id, None),
                                            );
                                        }
                                    }
                                    Some(RequestKind::Attach | RequestKind::Resume) | None => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            };
            drop(registration);
            if let Some(session) = session {
                self.attachments.session_ended_by(host_id, session);
            }
            events.layout_disconnected(host_id);
            events.panel_reply(host_id, PanelReply::Disconnected);
            events.spawn_reply(host_id, SpawnReply::Disconnected);
            let _ = tx.send(Outgoing::Stop);
            let _ = writer_thread.join();
            end
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;
    use std::time::Instant;

    use combine_protocol::{
        Ack, AttachRequestOptions, Attached, AttachmentId, BackendGeneration, BackendInfo,
        CloseReason, Closed, Cursor, Data, Display, Epoch, ErrorCode, ErrorMessage, Fidelity, Gap,
        HostId, Layout, LayoutNode, LayoutSnapshot, ListTemplates, MutateLayout, RequestId, Seq,
        SpaceInfo, SpaceKind, Spawned, Started, TemplateInfo, Templates, TermId, Terminals,
        WatchLayout,
    };
    use combine_protocol::{
        CreatedBy, GetPanel, Panel, PanelClosed, PanelLifetime, PanelPinned, PanelState,
        PanelSummary, Renderer,
    };

    use super::super::attachments::StreamId;
    use super::super::model::{BackendView, ConfigStatus, ConnectionState, HostView, SpaceView};
    use super::super::outbound::NotConnected;
    use super::super::registry::HostRegistry;
    use super::super::spawn::SpawnSink;
    use super::super::supervisor::HostEvents;
    use super::super::testing::{
        host_id, recv, Delivered, LayoutEvent, RecordingEvents, RecordingLayoutSink,
        RecordingSpawnSink, RecordingTerminalSink, HOST_A, WAIT,
    };
    use super::*;

    struct Rig {
        outbound: Arc<Outbound>,
        attachments: Arc<Attachments>,
        terminals: Arc<RecordingTerminalSink>,
        daemon: UnixStream,
        ended: mpsc::Receiver<SessionEnd>,
    }

    fn watch_request(message: ClientMessage) -> RequestId {
        match message {
            ClientMessage::WatchTerminals(w) => w.request,
            other => panic!("expected watch_terminals, got {other:?}"),
        }
    }

    fn read_client(daemon: &mut UnixStream) -> ClientMessage {
        read_message::<ClientMessage, _>(daemon)
            .expect("read")
            .expect("a message")
    }

    fn start(events: Arc<dyn HostEvents>) -> Rig {
        let terminals = RecordingTerminalSink::new();
        let attachments = Arc::new(Attachments::new(terminals.clone()));
        start_on(
            events,
            Arc::new(Outbound::default()),
            attachments,
            terminals,
        )
    }

    /// Starts a session over the given shared state, as a reconnect or a second session would.
    fn start_on(
        events: Arc<dyn HostEvents>,
        outbound: Arc<Outbound>,
        attachments: Arc<Attachments>,
        terminals: Arc<RecordingTerminalSink>,
    ) -> Rig {
        let (client, daemon) = UnixStream::pair().expect("socket pair");
        daemon.set_read_timeout(Some(WAIT)).expect("read timeout");
        let (done, ended) = mpsc::channel();
        let mut phase = LivePhase::new(Arc::clone(&outbound), Arc::clone(&attachments));
        thread::spawn(move || {
            let id = host_id(HOST_A);
            let mut reader = client.try_clone().expect("clone");
            let mut writer = client;
            let end = phase.run(SessionIo {
                host_id: &id,
                reader: &mut reader,
                writer: &mut writer,
                events: &*events,
            });
            let _ = done.send(end);
        });
        Rig {
            outbound,
            attachments,
            terminals,
            daemon,
            ended,
        }
    }

    fn list() -> Terminals {
        Terminals {
            request: Some(RequestId(1)),
            terminals: Vec::new(),
            spaces: vec![SpaceInfo {
                id: "/work".to_string(),
                kind: SpaceKind::Folder,
                terminals: Vec::<TermId>::new(),
            }],
            backend: None,
        }
    }

    fn end_session(rig: Rig) -> SessionEnd {
        rig.daemon.shutdown(Shutdown::Both).expect("close");
        rig.ended
            .recv_timeout(WAIT)
            .expect("the session ends and joins its writer")
    }

    #[test]
    fn live_phase_sends_watch_terminals_first() {
        let (events, published) = RecordingEvents::new();
        let mut rig = start(events);
        let request = watch_request(read_client(&mut rig.daemon));
        assert!(published.try_recv().is_err());
        assert_eq!(request, RequestId(1));
        assert_eq!(
            rig.outbound.kind_of(&host_id(HOST_A), request),
            Some(RequestKind::Terminals)
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    struct RegistryEvents(Mutex<HostRegistry>);

    impl HostEvents for RegistryEvents {
        fn connection(&self, _name: &str, _state: ConnectionState) {}
        fn host_id(&self, _name: &str, _host_id: &HostId) {}
        fn spaces(&self, host_id: &HostId, spaces: Vec<SpaceView>, backend: Option<BackendView>) {
            self.0
                .lock()
                .expect("registry")
                .replace_spaces(host_id, spaces, backend);
        }
    }

    #[test]
    fn terminals_publish_spaces_into_snapshot() {
        let registry = HostRegistry::new(
            ConfigStatus {
                path: "hosts.json".to_string(),
                error: None,
            },
            vec![HostView {
                name: "mini".to_string(),
                host_id: Some(host_id(HOST_A)),
                connection: ConnectionState::Connected,
                spaces: Vec::new(),
                backend: None,
            }],
        );
        let events = Arc::new(RegistryEvents(Mutex::new(registry)));
        let mut rig = start(events.clone());
        read_client(&mut rig.daemon);
        write_message(&mut rig.daemon, &DaemonMessage::Terminals(list())).expect("write");
        let deadline = Instant::now() + WAIT;
        let spaces = loop {
            let spaces = events.0.lock().expect("registry").snapshot().hosts[0]
                .spaces
                .clone();
            if !spaces.is_empty() || Instant::now() > deadline {
                break spaces;
            }
            thread::yield_now();
        };
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].key.path, "/work");
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn outbound_send_reaches_writer_while_reader_blocked() {
        let (events, _rx) = RecordingEvents::new();
        let mut rig = start(events);
        read_client(&mut rig.daemon);
        let sent = rig
            .outbound
            .send(&host_id(HOST_A), RequestKind::Terminals, |request| {
                ClientMessage::WatchTerminals(WatchTerminals { request })
            })
            .expect("registered");
        assert_eq!(watch_request(read_client(&mut rig.daemon)), sent);
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn send_without_session_is_not_connected() {
        let outbound = Outbound::default();
        let result = outbound.send(&host_id(HOST_A), RequestKind::Terminals, |request| {
            ClientMessage::WatchTerminals(WatchTerminals { request })
        });
        assert_eq!(result, Err(NotConnected));
    }

    #[test]
    fn session_end_unregisters_and_joins_writer() {
        let (events, _rx) = RecordingEvents::new();
        let mut rig = start(events);
        read_client(&mut rig.daemon);
        let outbound = Arc::clone(&rig.outbound);
        assert_eq!(end_session(rig), SessionEnd::Closed);
        let result = outbound.send(&host_id(HOST_A), RequestKind::Terminals, |request| {
            ClientMessage::WatchTerminals(WatchTerminals { request })
        });
        assert_eq!(result, Err(NotConnected));
    }

    #[test]
    fn stale_registration_drop_keeps_newer_session() {
        let outbound = Arc::new(Outbound::default());
        let host = host_id(HOST_A);
        let (old_tx, _old_rx) = mpsc::channel();
        let (new_tx, new_rx) = mpsc::channel();
        let old = outbound.register(&host, old_tx);
        let _new = outbound.register(&host, new_tx);
        drop(old);
        let request = outbound
            .send(&host, RequestKind::Terminals, |request| {
                ClientMessage::WatchTerminals(WatchTerminals { request })
            })
            .expect("the newer session is still registered");
        assert_eq!(
            new_rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::WatchTerminals(
                WatchTerminals { request }
            )))
        );
    }

    struct LayoutEvents(Arc<RecordingLayoutSink>);

    impl HostEvents for LayoutEvents {
        fn connection(&self, _name: &str, _state: ConnectionState) {}
        fn host_id(&self, _name: &str, _host_id: &HostId) {}
        fn spaces(
            &self,
            _host_id: &HostId,
            _spaces: Vec<SpaceView>,
            _backend: Option<BackendView>,
        ) {
        }
        fn layout(&self, host_id: &HostId, request: Option<RequestId>, snapshot: LayoutSnapshot) {
            self.0.layout(host_id, request, snapshot);
        }
        fn layout_error(
            &self,
            host_id: &HostId,
            request: RequestId,
            code: ErrorCode,
            message: String,
        ) {
            self.0.layout_error(host_id, request, code, message);
        }
        fn layout_disconnected(&self, host_id: &HostId) {
            self.0.layout_disconnected(host_id);
        }
        fn panels(&self, host_id: &HostId, panels: Panels) {
            self.0.panels(host_id, panels);
        }
        fn panel_reply(&self, host_id: &HostId, reply: PanelReply) {
            self.0.panel_reply(host_id, reply);
        }
    }

    fn layout_rig() -> (Rig, mpsc::Receiver<LayoutEvent>) {
        let (sink, received) = RecordingLayoutSink::new();
        let mut rig = start(Arc::new(LayoutEvents(sink)));
        read_client(&mut rig.daemon);
        (rig, received)
    }

    fn snapshot(revision: u64) -> LayoutSnapshot {
        LayoutSnapshot {
            space: "/work".to_string(),
            layout_revision: revision,
            root: LayoutNode::Tile {
                id: "t1".to_string(),
                panels: vec!["p1".to_string()],
                active: Some("p1".to_string()),
            },
            read_only: false,
            error: None,
            dropped: None,
        }
    }

    #[test]
    fn layout_push_reaches_layout_sink() {
        let (mut rig, received) = layout_rig();
        let pushed = Layout {
            request: None,
            layout: snapshot(3),
        };
        write_message(&mut rig.daemon, &DaemonMessage::Layout(pushed)).expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::Layout(host_id(HOST_A), None, snapshot(3))
        );
        let replied = rig
            .outbound
            .send(&host_id(HOST_A), RequestKind::Layout, |request| {
                ClientMessage::MutateLayout(MutateLayout {
                    request,
                    space: "/work".to_string(),
                    base_layout_revision: 3,
                    ops: Vec::new(),
                })
            })
            .expect("registered");
        read_client(&mut rig.daemon);
        let reply = Layout {
            request: Some(replied),
            layout: snapshot(4),
        };
        write_message(&mut rig.daemon, &DaemonMessage::Layout(reply)).expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::Layout(host_id(HOST_A), Some(replied), snapshot(4))
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    fn error_for(request: RequestId, code: ErrorCode, message: &str) -> DaemonMessage {
        DaemonMessage::Error(ErrorMessage {
            code,
            message: message.to_string(),
            request: Some(request),
            attachment: None,
        })
    }

    #[test]
    fn layout_error_routed_by_request_kind() {
        let (mut rig, received) = layout_rig();
        let host = host_id(HOST_A);
        let watch = rig
            .outbound
            .send(&host, RequestKind::Layout, |request| {
                ClientMessage::WatchLayout(WatchLayout {
                    request,
                    space: "/nope".to_string(),
                })
            })
            .expect("registered");
        read_client(&mut rig.daemon);
        write_message(
            &mut rig.daemon,
            &error_for(
                RequestId(1),
                ErrorCode::InvalidRequest,
                "not a layout error",
            ),
        )
        .expect("write");
        write_message(
            &mut rig.daemon,
            &error_for(watch, ErrorCode::UnknownSpace, "no such\nspace"),
        )
        .expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::Error(
                host,
                watch,
                ErrorCode::UnknownSpace,
                "no such space".to_string()
            )
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
        assert_eq!(recv(&received), LayoutEvent::Disconnected(host_id(HOST_A)));
    }

    #[test]
    fn unrouted_errors_do_not_reach_layout_error() {
        let (mut rig, received) = layout_rig();
        let host = host_id(HOST_A);
        let watch = rig
            .outbound
            .send(&host, RequestKind::Layout, |request| {
                ClientMessage::WatchLayout(WatchLayout {
                    request,
                    space: "/work".to_string(),
                })
            })
            .expect("registered");
        read_client(&mut rig.daemon);
        write_message(
            &mut rig.daemon,
            &error_for(RequestId(99), ErrorCode::UnknownSpace, "never sent"),
        )
        .expect("write");
        write_message(
            &mut rig.daemon,
            &DaemonMessage::Error(ErrorMessage {
                code: ErrorCode::Internal,
                message: "no request".to_string(),
                request: None,
                attachment: None,
            }),
        )
        .expect("write");
        write_message(
            &mut rig.daemon,
            &error_for(watch, ErrorCode::StaleLayout, "sentinel"),
        )
        .expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::Error(host, watch, ErrorCode::StaleLayout, "sentinel".to_string())
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    struct SpawnEvents(Arc<RecordingSpawnSink>);

    impl HostEvents for SpawnEvents {
        fn connection(&self, _name: &str, _state: ConnectionState) {}
        fn host_id(&self, _name: &str, _host_id: &HostId) {}
        fn spaces(
            &self,
            _host_id: &HostId,
            _spaces: Vec<SpaceView>,
            _backend: Option<BackendView>,
        ) {
        }
        fn spawn_reply(&self, host_id: &HostId, reply: SpawnReply) {
            self.0.spawn_reply(host_id, reply);
        }
    }

    /// Routes spawn replies to one sink and layout traffic to another.
    struct BothEvents(SpawnEvents, LayoutEvents);

    impl HostEvents for BothEvents {
        fn connection(&self, _name: &str, _state: ConnectionState) {}
        fn host_id(&self, _name: &str, _host_id: &HostId) {}
        fn spaces(
            &self,
            _host_id: &HostId,
            _spaces: Vec<SpaceView>,
            _backend: Option<BackendView>,
        ) {
        }
        fn layout_error(
            &self,
            host_id: &HostId,
            request: RequestId,
            code: ErrorCode,
            message: String,
        ) {
            self.1.layout_error(host_id, request, code, message);
        }
        fn spawn_reply(&self, host_id: &HostId, reply: SpawnReply) {
            self.0.spawn_reply(host_id, reply);
        }
    }

    fn spawn_rig() -> (Rig, mpsc::Receiver<(HostId, SpawnReply)>) {
        let (sink, received) = RecordingSpawnSink::new();
        let mut rig = start(Arc::new(SpawnEvents(sink)));
        read_client(&mut rig.daemon);
        (rig, received)
    }

    #[test]
    fn spawn_replies_reach_spawn_sink() {
        let (mut rig, received) = spawn_rig();
        let templates = Templates {
            request: RequestId(2),
            space: "/work".to_string(),
            templates: vec![TemplateInfo {
                panel: "claude".to_string(),
                title: None,
                slot: "claude".to_string(),
                argv: vec!["claude".to_string()],
                env: BTreeMap::new(),
                cwd: String::new(),
                hash: "0".repeat(64),
            }],
            skipped: Vec::new(),
            error: None,
        };
        let spawned = Spawned {
            request: RequestId(3),
            space: "/work".to_string(),
            terminal: TermId("t12".to_string()),
        };
        write_message(
            &mut rig.daemon,
            &DaemonMessage::Templates(templates.clone()),
        )
        .expect("write");
        write_message(&mut rig.daemon, &DaemonMessage::Spawned(spawned.clone())).expect("write");
        let host = host_id(HOST_A);
        assert_eq!(
            recv(&received),
            (host.clone(), SpawnReply::Templates(templates))
        );
        assert_eq!(recv(&received), (host, SpawnReply::Spawned(spawned)));
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn spawn_error_routed_by_request_kind() {
        let (spawn_sink, spawn_received) = RecordingSpawnSink::new();
        let (layout_sink, layout_received) = RecordingLayoutSink::new();
        let mut rig = start(Arc::new(BothEvents(
            SpawnEvents(spawn_sink),
            LayoutEvents(layout_sink),
        )));
        read_client(&mut rig.daemon);
        let host = host_id(HOST_A);
        let layout = rig
            .outbound
            .send(&host, RequestKind::Layout, |request| {
                ClientMessage::WatchLayout(WatchLayout {
                    request,
                    space: "/work".to_string(),
                })
            })
            .expect("registered");
        let spawn = rig
            .outbound
            .send(&host, RequestKind::Spawn, |request| {
                ClientMessage::ListTemplates(ListTemplates {
                    request,
                    space: "/work".to_string(),
                })
            })
            .expect("registered");
        read_client(&mut rig.daemon);
        read_client(&mut rig.daemon);
        write_message(
            &mut rig.daemon,
            &error_for(RequestId(1), ErrorCode::Internal, "the watch"),
        )
        .expect("write");
        write_message(
            &mut rig.daemon,
            &error_for(RequestId(99), ErrorCode::Internal, "never sent"),
        )
        .expect("write");
        write_message(
            &mut rig.daemon,
            &error_for(spawn, ErrorCode::CwdRefused, "no\ngo"),
        )
        .expect("write");
        write_message(
            &mut rig.daemon,
            &error_for(layout, ErrorCode::UnknownSpace, "no such space"),
        )
        .expect("write");
        assert_eq!(
            recv(&spawn_received),
            (
                host.clone(),
                SpawnReply::Error {
                    request: spawn,
                    code: ErrorCode::CwdRefused,
                    message: "no go".to_string(),
                }
            )
        );
        assert_eq!(
            recv(&layout_received),
            LayoutEvent::Error(
                host,
                layout,
                ErrorCode::UnknownSpace,
                "no such space".to_string()
            )
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
        assert_eq!(
            recv(&spawn_received),
            (host_id(HOST_A), SpawnReply::Disconnected)
        );
        assert!(spawn_received.try_recv().is_err());
        assert!(layout_received.try_recv().is_err());
    }

    #[test]
    fn session_end_reports_spawn_disconnected() {
        let (rig, received) = spawn_rig();
        assert!(received.try_recv().is_err());
        assert_eq!(end_session(rig), SessionEnd::Closed);
        assert_eq!(recv(&received), (host_id(HOST_A), SpawnReply::Disconnected));
    }

    fn panel_content(request: RequestId) -> Panel {
        Panel {
            request,
            space: "/work".to_string(),
            id: "notes".to_string(),
            revision: 12,
            renderer: Renderer::Markdown,
            source: "# Notes\n".to_string(),
            state: PanelState::Validated,
            error: None,
        }
    }

    #[test]
    fn panels_and_panel_replies_route_to_the_events() {
        let (mut rig, received) = layout_rig();
        let host = host_id(HOST_A);
        let list = Panels {
            request: Some(RequestId(7)),
            space: "/work".to_string(),
            panels: vec![PanelSummary {
                id: "notes".to_string(),
                title: None,
                renderer: Renderer::Markdown,
                revision: 12,
                state: PanelState::Validated,
                error: None,
                created_by: Some(CreatedBy::User),
                lifetime: Some(PanelLifetime::Scratch),
            }],
            skipped: Vec::new(),
            error: None,
        };
        write_message(&mut rig.daemon, &DaemonMessage::Panels(list.clone())).expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::Panels(host.clone(), list.clone())
        );
        let push = Panels {
            request: None,
            ..list
        };
        write_message(&mut rig.daemon, &DaemonMessage::Panels(push.clone())).expect("write");
        assert_eq!(recv(&received), LayoutEvent::Panels(host.clone(), push));
        let fetched = rig
            .outbound
            .send(&host, RequestKind::Panel, |request| {
                ClientMessage::GetPanel(GetPanel {
                    request,
                    space: "/work".to_string(),
                    panel: "gone".to_string(),
                })
            })
            .expect("registered");
        read_client(&mut rig.daemon);
        let panel = panel_content(RequestId(9));
        write_message(&mut rig.daemon, &DaemonMessage::Panel(panel.clone())).expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::PanelReply(host.clone(), PanelReply::Panel(panel))
        );
        write_message(
            &mut rig.daemon,
            &error_for(fetched, ErrorCode::UnknownPanel, "the space\nhas no panel"),
        )
        .expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::PanelReply(
                host,
                PanelReply::Error {
                    request: fetched,
                    code: ErrorCode::UnknownPanel,
                    message: "the space has no panel".to_string(),
                }
            )
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn panel_closed_and_panel_pinned_reach_the_panel_sink() {
        let (mut rig, received) = layout_rig();
        let host = host_id(HOST_A);
        let closed = PanelClosed {
            request: RequestId(11),
            space: "/work".to_string(),
            panel: "notes".to_string(),
        };
        write_message(&mut rig.daemon, &DaemonMessage::PanelClosed(closed.clone())).expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::PanelReply(host.clone(), PanelReply::Closed(closed))
        );
        let pinned = PanelPinned {
            request: RequestId(12),
            space: "/work".to_string(),
            panel: "notes".to_string(),
        };
        write_message(&mut rig.daemon, &DaemonMessage::PanelPinned(pinned.clone())).expect("write");
        assert_eq!(
            recv(&received),
            LayoutEvent::PanelReply(host, PanelReply::Pinned(pinned))
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn a_session_end_sends_layout_disconnected_then_panel_disconnected() {
        let (rig, received) = layout_rig();
        assert_eq!(end_session(rig), SessionEnd::Closed);
        assert_eq!(recv(&received), LayoutEvent::Disconnected(host_id(HOST_A)));
        assert_eq!(
            recv(&received),
            LayoutEvent::PanelReply(host_id(HOST_A), PanelReply::Disconnected)
        );
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn session_end_reports_layout_disconnected() {
        let (rig, received) = layout_rig();
        assert!(received.try_recv().is_err());
        assert_eq!(end_session(rig), SessionEnd::Closed);
        assert_eq!(recv(&received), LayoutEvent::Disconnected(host_id(HOST_A)));
    }

    const STREAM: StreamId = StreamId(1);

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

    fn output(attachment: u64, seq: u64, bytes: &[u8]) -> DaemonMessage {
        DaemonMessage::Data(Data {
            cursor: cursor(attachment, seq),
            bytes: bytes.to_vec(),
        })
    }

    fn write_daemon(rig: &mut Rig, message: DaemonMessage) {
        write_message(&mut rig.daemon, &message).expect("write");
    }

    const CORE_A: &str = "core-a";

    fn up(instance: &str) -> Delivered {
        Delivered::Connection(true, Some(instance.to_string()))
    }

    fn down() -> Delivered {
        Delivered::Connection(false, None)
    }

    fn list_of(instance: Option<&str>) -> Terminals {
        Terminals {
            backend: Some(BackendInfo {
                id: "herdr-core".to_string(),
                instance: instance.map(str::to_string),
            }),
            ..list()
        }
    }

    /// Waits until `STREAM` has been told `expected`, the last delivery it got.
    fn wait_connection(rig: &Rig, expected: Delivered) {
        let deadline = Instant::now() + WAIT;
        loop {
            let last = rig
                .terminals
                .deliveries()
                .into_iter()
                .rev()
                .find(|(stream, d)| *stream == STREAM && matches!(d, Delivered::Connection(..)));
            if last.as_ref().map(|(_, d)| d) == Some(&expected) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "STREAM was last told {last:?}, expected {expected:?}"
            );
            thread::yield_now();
        }
    }

    /// Opens `STREAM`, answers the session's `watch_terminals` with a list naming `instance`,
    /// and waits for the stream to hear the session is ready.
    fn open_ready(rig: &mut Rig, instance: &str) {
        let request = watch_request(read_client(&mut rig.daemon));
        rig.attachments.open_stream(STREAM, host_id(HOST_A));
        write_daemon(
            rig,
            DaemonMessage::Terminals(Terminals {
                request: Some(request),
                ..list_of(Some(instance))
            }),
        );
        wait_connection(rig, up(instance));
    }

    /// Opens `STREAM` on a started session, attaches it and answers with `attached`.
    fn attached_rig(attachment: u64) -> Rig {
        let (events, _published) = RecordingEvents::new();
        let mut rig = start(events);
        open_ready(&mut rig, CORE_A);
        attach_and_answer(&mut rig, attachment);
        rig
    }

    fn attach_and_answer(rig: &mut Rig, attachment: u64) {
        rig.attachments
            .attach(
                &rig.outbound,
                STREAM,
                TermId("t1".to_string()),
                options(),
                None,
            )
            .expect("attach");
        let request = match read_client(&mut rig.daemon) {
            ClientMessage::Attach(attach) => attach.request,
            other => panic!("expected attach, got {other:?}"),
        };
        write_daemon(
            rig,
            DaemonMessage::Attached(Attached {
                request,
                attachment: AttachmentId(attachment),
            }),
        );
    }

    /// Sends a sentinel request and returns it, so a test can read everything the client sent
    /// before it.
    fn sentinel(rig: &Rig) -> RequestId {
        rig.outbound
            .send(&host_id(HOST_A), RequestKind::Terminals, |request| {
                ClientMessage::WatchTerminals(WatchTerminals { request })
            })
            .expect("registered")
    }

    fn delivered_messages(rig: &Rig, count: usize) -> Vec<DaemonMessage> {
        rig.terminals
            .wait_for(count)
            .into_iter()
            .filter_map(|(stream, delivered)| match delivered {
                Delivered::Message(message) => {
                    assert_eq!(stream, STREAM);
                    Some(message)
                }
                Delivered::Connection(..) => None,
            })
            .collect()
    }

    #[test]
    fn a_stream_is_not_connected_until_the_sessions_terminals_name_the_instance() {
        let (events, _published) = RecordingEvents::new();
        let mut rig = start(events);
        let request = watch_request(read_client(&mut rig.daemon));
        rig.attachments.open_stream(STREAM, host_id(HOST_A));
        assert_eq!(
            rig.terminals.deliveries(),
            [(STREAM, down())],
            "a registered session that hasn't listed its terminals is not connected"
        );
        assert_eq!(
            rig.attachments.attach(
                &rig.outbound,
                STREAM,
                TermId("t1".to_string()),
                options(),
                Some(CORE_A.to_string())
            ),
            Err(super::super::attachments::ForwardError::NotConnected)
        );
        write_daemon(
            &mut rig,
            DaemonMessage::Terminals(Terminals {
                request: Some(request),
                ..list_of(Some("core-b"))
            }),
        );
        wait_connection(&rig, up("core-b"));
        write_daemon(
            &mut rig,
            DaemonMessage::Terminals(Terminals {
                request: None,
                ..list_of(Some("core-c"))
            }),
        );
        let after = sentinel(&rig);
        assert_eq!(watch_request(read_client(&mut rig.daemon)), after);
        assert_eq!(
            rig.terminals.deliveries(),
            [(STREAM, down()), (STREAM, up("core-b"))],
            "a later push doesn't restart the session"
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn a_refused_terminal_list_still_readies_the_session_without_an_instance() {
        let (events, _published) = RecordingEvents::new();
        let mut rig = start(events);
        let request = watch_request(read_client(&mut rig.daemon));
        rig.attachments.open_stream(STREAM, host_id(HOST_A));
        write_daemon(
            &mut rig,
            DaemonMessage::Error(ErrorMessage {
                code: ErrorCode::Internal,
                message: "the terminal list is larger than one frame".to_string(),
                request: Some(request),
                attachment: None,
            }),
        );
        wait_connection(&rig, Delivered::Connection(true, None));
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn a_reconnect_to_another_instance_reports_that_instance_and_never_the_old_one() {
        let mut rig = attached_rig(10);
        write_daemon(&mut rig, started(10));
        delivered_messages(&rig, 4);
        let (outbound, attachments, terminals) = (
            Arc::clone(&rig.outbound),
            Arc::clone(&rig.attachments),
            Arc::clone(&rig.terminals),
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
        let (events, _published) = RecordingEvents::new();
        let mut rig = start_on(events, outbound, attachments, terminals);
        let request = watch_request(read_client(&mut rig.daemon));
        let before = rig.terminals.deliveries().len();
        write_daemon(
            &mut rig,
            DaemonMessage::Terminals(Terminals {
                request: Some(request),
                ..list_of(Some("core-b"))
            }),
        );
        wait_connection(&rig, up("core-b"));
        let connections: Vec<Delivered> = rig.terminals.deliveries()[before - 1..]
            .iter()
            .filter(|(_, d)| matches!(d, Delivered::Connection(..)))
            .map(|(_, d)| d.clone())
            .collect();
        assert_eq!(connections, [down(), up("core-b")]);
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn attachment_messages_reach_terminal_sink() {
        let mut rig = attached_rig(10);
        write_daemon(&mut rig, started(10));
        write_daemon(
            &mut rig,
            DaemonMessage::Gap(Gap {
                attachment: AttachmentId(10),
                request: None,
            }),
        );
        let attached = DaemonMessage::Attached(Attached {
            request: RequestId(2),
            attachment: AttachmentId(10),
        });
        assert_eq!(
            delivered_messages(&rig, 5),
            [
                attached,
                started(10),
                DaemonMessage::Gap(Gap {
                    attachment: AttachmentId(10),
                    request: None,
                })
            ]
        );
        assert_eq!(rig.terminals.deliveries()[0].1, down());
        assert_eq!(rig.terminals.deliveries()[1].1, up(CORE_A));
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn data_frames_reach_sink_with_cursor() {
        let mut rig = attached_rig(10);
        write_daemon(&mut rig, started(10));
        write_daemon(&mut rig, output(10, 1, b"hi"));
        write_daemon(&mut rig, output(10, 2, b""));
        let messages = delivered_messages(&rig, 6);
        assert_eq!(messages[2..], [output(10, 1, b"hi"), output(10, 2, b"")]);
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn no_auto_ack_and_no_auto_resume_after_reconnect() {
        let mut rig = attached_rig(10);
        write_daemon(&mut rig, started(10));
        write_daemon(&mut rig, output(10, 1, b"hi"));
        delivered_messages(&rig, 5);
        let first = sentinel(&rig);
        assert_eq!(watch_request(read_client(&mut rig.daemon)), first);
        let (outbound, attachments, terminals) = (
            Arc::clone(&rig.outbound),
            Arc::clone(&rig.attachments),
            Arc::clone(&rig.terminals),
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
        let (events, _published) = RecordingEvents::new();
        let mut rig = start_on(events, outbound, attachments, terminals);
        watch_request(read_client(&mut rig.daemon));
        let next = sentinel(&rig);
        assert_eq!(watch_request(read_client(&mut rig.daemon)), next);
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn daemon_restart_resume_gets_gap_then_fresh_attach() {
        let mut rig = attached_rig(10);
        write_daemon(&mut rig, started(10));
        write_daemon(&mut rig, output(10, 1, b"hi"));
        delivered_messages(&rig, 5);
        let (outbound, attachments, terminals) = (
            Arc::clone(&rig.outbound),
            Arc::clone(&rig.attachments),
            Arc::clone(&rig.terminals),
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
        assert_eq!(
            terminals.deliveries().last().map(|(_, d)| d.clone()),
            Some(down())
        );
        let (events, _published) = RecordingEvents::new();
        let mut rig = start_on(events, outbound, attachments, terminals);
        let request = watch_request(read_client(&mut rig.daemon));
        write_daemon(
            &mut rig,
            DaemonMessage::Terminals(Terminals {
                request: Some(request),
                ..list_of(Some(CORE_A))
            }),
        );
        wait_connection(&rig, up(CORE_A));
        rig.attachments
            .resume(
                &rig.outbound,
                STREAM,
                cursor(10, 1),
                Some(CORE_A.to_string()),
            )
            .expect("resume");
        let resume = match read_client(&mut rig.daemon) {
            ClientMessage::Resume(resume) => resume,
            other => panic!("expected resume, got {other:?}"),
        };
        assert_eq!(resume.instance.as_deref(), Some(CORE_A));
        write_daemon(
            &mut rig,
            DaemonMessage::Gap(Gap {
                attachment: AttachmentId(10),
                request: Some(resume.request),
            }),
        );
        rig.attachments
            .attach(
                &rig.outbound,
                STREAM,
                TermId("t1".to_string()),
                options(),
                None,
            )
            .expect("attach");
        let attach = match read_client(&mut rig.daemon) {
            ClientMessage::Attach(attach) => attach.request,
            other => panic!("expected attach, got {other:?}"),
        };
        write_daemon(
            &mut rig,
            DaemonMessage::Attached(Attached {
                request: attach,
                attachment: AttachmentId(11),
            }),
        );
        let messages = delivered_messages(&rig, 9);
        assert_eq!(
            messages[3..],
            [
                DaemonMessage::Gap(Gap {
                    attachment: AttachmentId(10),
                    request: Some(resume.request),
                }),
                DaemonMessage::Attached(Attached {
                    request: attach,
                    attachment: AttachmentId(11),
                }),
            ]
        );
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn closing_a_stream_with_an_unacked_final_event_finishes_the_attachment() {
        let mut rig = attached_rig(10);
        write_daemon(&mut rig, started(10));
        let closed = DaemonMessage::Closed(Closed {
            cursor: cursor(10, 1),
            reason: CloseReason::Exited,
        });
        write_daemon(&mut rig, closed.clone());
        assert_eq!(delivered_messages(&rig, 5).last(), Some(&closed));
        rig.attachments.close_stream(&rig.outbound, STREAM);
        let after = sentinel(&rig);
        assert_eq!(
            read_client(&mut rig.daemon),
            ClientMessage::Ack(Ack {
                cursor: cursor(10, 1)
            })
        );
        assert_eq!(watch_request(read_client(&mut rig.daemon)), after);
        assert_eq!(end_session(rig), SessionEnd::Closed);
    }

    #[test]
    fn an_old_sessions_end_does_not_clear_a_newer_session() {
        let (events, _published) = RecordingEvents::new();
        let mut old = start(events);
        open_ready(&mut old, CORE_A);
        let (events, _published) = RecordingEvents::new();
        let mut new = start_on(
            events,
            Arc::clone(&old.outbound),
            Arc::clone(&old.attachments),
            Arc::clone(&old.terminals),
        );
        let request = watch_request(read_client(&mut new.daemon));
        write_daemon(
            &mut new,
            DaemonMessage::Terminals(Terminals {
                request: Some(request),
                ..list_of(Some("core-b"))
            }),
        );
        wait_connection(&new, up("core-b"));
        let terminals = Arc::clone(&new.terminals);
        let before = terminals.deliveries().len();
        assert_eq!(end_session(old), SessionEnd::Closed);
        assert_eq!(terminals.deliveries().len(), before);
        assert_eq!(terminals.deliveries().last(), Some(&(STREAM, up("core-b"))));
        assert_eq!(end_session(new), SessionEnd::Closed);
        assert_eq!(terminals.deliveries().last(), Some(&(STREAM, down())));
    }
}
