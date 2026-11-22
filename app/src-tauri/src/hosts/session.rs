//! The live session phase: subscribes to the host's terminals, publishes them as spaces, and
//! runs the writer that the outbound channel feeds.

use std::io::Write;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

use combine_protocol::{
    read_message, write_message, ClientMessage, DaemonMessage, ErrorCode, HostId, LayoutSnapshot,
    ReadError, RequestId, WatchTerminals,
};

use super::outbound::{Outbound, Outgoing, RequestKind};
use super::supervisor::{SessionEnd, SessionIo, SessionPhase};
use super::terminals::space_views;
use super::text::display_text;

/// Where a host's layout traffic goes. The three methods mirror the layout methods of
/// [`HostEvents`](super::supervisor::HostEvents).
pub trait LayoutSink: Send + Sync {
    fn layout(&self, host_id: &HostId, request: Option<RequestId>, snapshot: LayoutSnapshot);
    fn layout_error(&self, host_id: &HostId, request: RequestId, code: ErrorCode, message: String);
    fn layout_disconnected(&self, host_id: &HostId);
}

pub struct LivePhase {
    outbound: Arc<Outbound>,
}

impl LivePhase {
    pub fn new(outbound: Arc<Outbound>) -> Self {
        LivePhase { outbound }
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
                    match read_message::<DaemonMessage, _>(&mut *reader) {
                        Ok(None) => break SessionEnd::Closed,
                        Ok(Some(DaemonMessage::Terminals(list))) => {
                            events.spaces(host_id, space_views(host_id, &list));
                        }
                        Ok(Some(DaemonMessage::Layout(reply))) => {
                            events.layout(host_id, reply.request, reply.layout);
                        }
                        Ok(Some(DaemonMessage::Error(error))) => {
                            if let Some(request) = error.request.filter(|request| {
                                self.outbound.kind_of(host_id, *request)
                                    == Some(RequestKind::Layout)
                            }) {
                                events.layout_error(
                                    host_id,
                                    request,
                                    error.code,
                                    display_text(error.trimmed_message()),
                                );
                            }
                        }
                        Ok(Some(_)) => {}
                        Err(ReadError::Io(e)) => break SessionEnd::Io(e.to_string()),
                        Err(ReadError::Decode(e)) => break SessionEnd::Protocol(e.to_string()),
                    }
                }
            };
            drop(registration);
            events.layout_disconnected(host_id);
            let _ = tx.send(Outgoing::Stop);
            let _ = writer_thread.join();
            end
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;
    use std::time::Instant;

    use combine_protocol::{
        ErrorCode, ErrorMessage, HostId, Layout, LayoutNode, LayoutSnapshot, MutateLayout,
        RequestId, SpaceInfo, SpaceKind, TermId, Terminals, WatchLayout,
    };

    use super::super::model::{ConfigStatus, ConnectionState, HostView, SpaceView};
    use super::super::outbound::NotConnected;
    use super::super::registry::HostRegistry;
    use super::super::supervisor::HostEvents;
    use super::super::testing::{
        host_id, recv, LayoutEvent, RecordingEvents, RecordingLayoutSink, HOST_A, WAIT,
    };
    use super::*;

    struct Rig {
        outbound: Arc<Outbound>,
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
        let (client, daemon) = UnixStream::pair().expect("socket pair");
        daemon.set_read_timeout(Some(WAIT)).expect("read timeout");
        let (done, ended) = mpsc::channel();
        let outbound = Arc::new(Outbound::default());
        let mut phase = LivePhase::new(Arc::clone(&outbound));
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
        fn spaces(&self, host_id: &HostId, spaces: Vec<SpaceView>) {
            self.0
                .lock()
                .expect("registry")
                .replace_spaces(host_id, spaces);
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
        fn spaces(&self, _host_id: &HostId, _spaces: Vec<SpaceView>) {}
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
    fn session_end_reports_layout_disconnected() {
        let (rig, received) = layout_rig();
        assert!(received.try_recv().is_err());
        assert_eq!(end_session(rig), SessionEnd::Closed);
        assert_eq!(recv(&received), LayoutEvent::Disconnected(host_id(HOST_A)));
    }
}
