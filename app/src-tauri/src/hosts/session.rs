//! The live session phase: subscribes to the host's terminals, publishes them as spaces, and
//! runs the writer that the outbound channel feeds.

use std::io::Write;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

use combine_protocol::{
    read_message, write_message, ClientMessage, DaemonMessage, ReadError, WatchTerminals,
};

use super::outbound::{Outbound, Outgoing, RequestKind};
use super::supervisor::{SessionEnd, SessionIo, SessionPhase};
use super::terminals::space_views;

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
                        Ok(Some(_)) => {}
                        Err(ReadError::Io(e)) => break SessionEnd::Io(e.to_string()),
                        Err(ReadError::Decode(e)) => break SessionEnd::Protocol(e.to_string()),
                    }
                }
            };
            drop(registration);
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
    use std::thread::JoinHandle;
    use std::time::Instant;

    use combine_protocol::{HostId, RequestId, SpaceInfo, SpaceKind, TermId, Terminals};

    use super::super::model::{ConfigStatus, ConnectionState, HostView, SpaceView};
    use super::super::outbound::NotConnected;
    use super::super::registry::HostRegistry;
    use super::super::supervisor::HostEvents;
    use super::super::testing::{host_id, RecordingEvents, HOST_A, WAIT};
    use super::*;

    struct Rig {
        outbound: Arc<Outbound>,
        daemon: UnixStream,
        session: JoinHandle<SessionEnd>,
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
        let outbound = Arc::new(Outbound::default());
        let mut phase = LivePhase::new(Arc::clone(&outbound));
        let session = thread::spawn(move || {
            let id = host_id(HOST_A);
            let mut reader = client.try_clone().expect("clone");
            let mut writer = client;
            phase.run(SessionIo {
                host_id: &id,
                reader: &mut reader,
                writer: &mut writer,
                events: &*events,
            })
        });
        Rig {
            outbound,
            daemon,
            session,
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
        rig.session.join().expect("the session thread")
    }

    #[test]
    fn live_phase_sends_watch_terminals_first() {
        let (events, _rx) = RecordingEvents::new();
        let mut rig = start(events);
        let request = watch_request(read_client(&mut rig.daemon));
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
    fn spaces_unregisters_and_joins_writer() {
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
}
