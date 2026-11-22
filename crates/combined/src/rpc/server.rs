//! The server loop: one task owns the driver and every connection's state, and waits on the
//! listener, the connections' inbox, the backend, the terminal watch and two timers.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use combine_protocol::{ClientMessage, DaemonMessage, ErrorCode, ErrorMessage, HostId, RequestId};
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};

use super::connection::{self, ConnEvent, WriterCmd};
use super::flush::{FlushAt, FlushSchedule};
use super::socket::BoundSocket;
use super::{ServeOptions, ServerParts, ServerStats};
use crate::core::{ConnectionId, Driver, TerminalList};
use crate::model::{Millis, TermEvent};
use crate::wiring::clock::MonotonicClock;
use crate::wiring::ids::IdReservation;

/// Messages the connection threads may queue before they block.
const INBOX: usize = 256;
/// Batches a connection's writer may hold. Flushes only fill an empty channel, so a close
/// message always fits.
const WRITER_QUEUE: usize = 2;
/// How long the loop waits after a failed accept, so a persistent failure can't spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// Why the server stopped.
#[derive(Debug)]
pub enum ServeError {
    /// The backend failed or its terminal watch ended: exit 3.
    Backend(String),
    /// The daemon's own state could not be kept, such as the ID reservation: exit 1.
    State(String),
    /// The listener or the runtime failed: exit 1.
    Io(io::Error),
}

impl ServeError {
    pub fn exit_code(&self) -> u8 {
        match self {
            ServeError::Backend(_) => 3,
            ServeError::State(_) | ServeError::Io(_) => 1,
        }
    }
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeError::Backend(text) | ServeError::State(text) => f.write_str(text),
            ServeError::Io(error) => write!(f, "server I/O failed: {error}"),
        }
    }
}

impl std::error::Error for ServeError {}

struct ConnState {
    writer: mpsc::Sender<WriterCmd>,
    socket: StdUnixStream,
    watching: bool,
    answers: VecDeque<RequestId>,
    stale: bool,
}

enum Wake {
    Shutdown,
    Accept(io::Result<UnixStream>),
    Inbox(ConnEvent),
    Driver,
    Watch(Option<TermEvent>),
    Flush,
    Housekeeping,
}

type Watch = BoxStream<'static, TermEvent>;

struct Server {
    driver: Driver,
    terminals: TerminalList,
    host_id: HostId,
    ids: IdReservation,
    clock: MonotonicClock,
    options: ServeOptions,
    stats: Arc<ServerStats>,
    conns: BTreeMap<ConnectionId, ConnState>,
    schedule: FlushSchedule,
    inbox_tx: mpsc::Sender<ConnEvent>,
    inbox: mpsc::Receiver<ConnEvent>,
    listener: UnixListener,
    watch: Option<Watch>,
    flush_timer: Pin<Box<Sleep>>,
    flush_armed: bool,
    housekeeping: Interval,
}

/// Serves `socket` until `shutdown` completes or something fails.
///
/// The server first watches the backend's terminals and then lists them, so no change between
/// the two is lost.
pub async fn serve(
    socket: BoundSocket,
    parts: ServerParts,
    options: ServeOptions,
    shutdown: impl Future<Output = ()>,
    stats: Arc<ServerStats>,
) -> Result<(), ServeError> {
    let BoundSocket { listener, lock } = socket;
    listener.set_nonblocking(true).map_err(ServeError::Io)?;
    let listener = UnixListener::from_std(listener).map_err(ServeError::Io)?;

    let ServerParts {
        driver,
        mut terminals,
        host_id,
        ids,
        clock,
        file_store: _,
    } = parts;
    let backend = Arc::clone(driver.backend());
    let watch = backend
        .watch()
        .await
        .map_err(|error| ServeError::Backend(format!("cannot watch terminals: {error}")))?;
    let list = backend
        .list()
        .await
        .map_err(|error| ServeError::Backend(format!("cannot list terminals: {error}")))?;
    terminals.apply_list(&list);
    terminals.take_changed();

    let (inbox_tx, inbox) = mpsc::channel(INBOX);
    let mut housekeeping = tokio::time::interval(options.housekeeping);
    housekeeping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut server = Server {
        driver,
        terminals,
        host_id,
        ids,
        clock,
        schedule: FlushSchedule::new(options.flush_interval),
        options,
        stats,
        conns: BTreeMap::new(),
        inbox_tx,
        inbox,
        listener,
        watch: Some(watch),
        flush_timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
        flush_armed: false,
        housekeeping,
    };
    let result = server.run(shutdown).await;
    server.close_all();
    drop(lock);
    result
}

async fn next_terminal_event(watch: &mut Option<Watch>) -> Option<TermEvent> {
    match watch {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

impl Server {
    async fn run(&mut self, shutdown: impl Future<Output = ()>) -> Result<(), ServeError> {
        tokio::pin!(shutdown);
        let clock = self.clock;
        loop {
            let wake = tokio::select! {
                () = &mut shutdown => Wake::Shutdown,
                accepted = self.listener.accept() => Wake::Accept(accepted.map(|(stream, _)| stream)),
                Some(event) = self.inbox.recv() => Wake::Inbox(event),
                () = std::future::poll_fn(|cx| self.driver.poll_event(cx, clock.now())) => Wake::Driver,
                item = next_terminal_event(&mut self.watch), if self.watch.is_some() => Wake::Watch(item),
                () = &mut self.flush_timer, if self.flush_armed => Wake::Flush,
                _ = self.housekeeping.tick() => Wake::Housekeeping,
            };
            match wake {
                Wake::Shutdown => return Ok(()),
                Wake::Accept(Ok(stream)) => self.admit(stream),
                Wake::Accept(Err(error)) => {
                    eprintln!("combined: accept failed: {error}");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
                Wake::Inbox(event) => self.handle_event(event),
                Wake::Driver => {}
                Wake::Watch(Some(event)) => {
                    self.terminals.apply_event(&event);
                    if self.terminals.take_changed() {
                        for state in self.conns.values_mut().filter(|state| state.watching) {
                            state.stale = true;
                        }
                    }
                }
                Wake::Watch(None) => {
                    return Err(ServeError::Backend("terminal watch ended".to_string()))
                }
                Wake::Flush => {
                    self.flush_armed = false;
                    let now = self.clock.now();
                    match self.schedule.request(now) {
                        FlushAt::Now => self.flush(now),
                        FlushAt::At(at) => self.arm(at),
                    }
                }
                Wake::Housekeeping => self.driver.core_mut().tick(self.clock.now()),
            }
            self.settle().await?;
        }
    }

    /// Takes a new connection, or closes it unanswered when the server is full.
    fn admit(&mut self, stream: UnixStream) {
        if self.conns.len() >= self.options.max_connections {
            return;
        }
        let Ok(stream) = stream.into_std() else {
            return;
        };
        let socket = match self.configure(&stream) {
            Ok(socket) => socket,
            Err(error) => {
                eprintln!("combined: cannot set up a connection: {error}");
                return;
            }
        };
        let conn = self.driver.core_mut().connect();
        let (writer, commands) = mpsc::channel(WRITER_QUEUE);
        let started = connection::spawn(connection::Spawn {
            conn,
            stream,
            commands,
            inbox: self.inbox_tx.clone(),
            host_id: self.host_id.clone(),
            handshake_timeout: self.options.handshake_timeout,
        });
        if let Err(error) = started {
            eprintln!("combined: cannot start a connection thread: {error}");
            self.driver.core_mut().disconnect(conn, self.clock.now());
            return;
        }
        self.conns.insert(
            conn,
            ConnState {
                writer,
                socket,
                watching: false,
                answers: VecDeque::new(),
                stale: false,
            },
        );
        self.stats.admitted();
    }

    /// Puts the socket in blocking mode with the write timeout and returns a handle for shutdowns.
    fn configure(&self, stream: &StdUnixStream) -> io::Result<StdUnixStream> {
        stream.set_nonblocking(false)?;
        stream.set_write_timeout(Some(self.options.write_timeout))?;
        stream.try_clone()
    }

    fn handle_event(&mut self, event: ConnEvent) {
        let now = self.clock.now();
        match event {
            ConnEvent::Message(conn, message) => {
                if !self.conns.contains_key(&conn) {
                    return;
                }
                match message {
                    ClientMessage::WatchTerminals(watch) => {
                        self.watch_terminals(conn, watch.request)
                    }
                    message => self.driver.core_mut().client_message(conn, message, now),
                }
            }
            ConnEvent::Malformed(conn, text) => {
                self.driver.core_mut().disconnect(conn, now);
                let Some(state) = self.conns.remove(&conn) else {
                    return;
                };
                self.stats.removed();
                let error = DaemonMessage::Error(ErrorMessage {
                    code: ErrorCode::Malformed,
                    message: format!("malformed frame: {text}"),
                    request: None,
                    attachment: None,
                });
                if state
                    .writer
                    .try_send(WriterCmd::Close(Some(error)))
                    .is_err()
                {
                    let _ = state.socket.shutdown(Shutdown::Both);
                }
            }
            ConnEvent::Closed(conn) => {
                self.driver.core_mut().disconnect(conn, now);
                self.remove(conn);
            }
        }
    }

    fn watch_terminals(&mut self, conn: ConnectionId, request: RequestId) {
        let Some(state) = self.conns.get_mut(&conn) else {
            return;
        };
        state.watching = true;
        state.answers.push_back(request);
        if state.answers.len() > self.options.watch_answer_limit {
            eprintln!(
                "combined: connection {} left more than {} watch_terminals answers unread; disconnecting",
                conn.0, self.options.watch_answer_limit
            );
            self.driver.core_mut().disconnect(conn, self.clock.now());
            self.remove(conn);
        }
    }

    /// Forgets the connection and closes its socket, which ends both its threads. It does
    /// nothing for a connection already removed.
    fn remove(&mut self, conn: ConnectionId) {
        if let Some(state) = self.conns.remove(&conn) {
            self.stats.removed();
            let _ = state.socket.shutdown(Shutdown::Both);
        }
    }

    fn close_all(&mut self) {
        let all: Vec<ConnectionId> = self.conns.keys().copied().collect();
        for conn in all {
            self.remove(conn);
        }
    }

    fn arm(&mut self, at: Millis) {
        let deadline = Instant::from_std(self.clock.instant_at(at));
        self.flush_timer.as_mut().reset(deadline);
        self.flush_armed = true;
    }

    fn has_pending(&self) -> bool {
        let core = self.driver.core();
        self.conns
            .iter()
            .any(|(conn, state)| core.has_output(*conn) || !state.answers.is_empty() || state.stale)
    }

    /// Runs core's effects, drops what it disconnected, keeps the ID reservation ahead, and
    /// flushes or arms the flush timer when output is waiting.
    async fn settle(&mut self) -> Result<(), ServeError> {
        loop {
            self.driver.run_effects(self.clock.now()).await;
            for conn in self.driver.take_disconnects() {
                self.remove(conn);
            }
            self.ids
                .keep_ahead(self.driver.core().high_water())
                .map_err(|error| {
                    ServeError::State(format!("cannot extend the ID reservation: {error}"))
                })?;
            if self.flush_armed || !self.has_pending() {
                return Ok(());
            }
            let now = self.clock.now();
            match self.schedule.request(now) {
                FlushAt::Now => self.flush(now),
                FlushAt::At(at) => {
                    self.arm(at);
                    return Ok(());
                }
            }
        }
    }

    /// One flush pass: for each connection whose writer is idle, its `terminals` answers, then
    /// whatever core has queued for it.
    fn flush(&mut self, now: Millis) {
        let gap = self
            .schedule
            .last()
            .map(|last| Millis(now.0.saturating_sub(last.0)));
        self.schedule.flushed(now);
        self.stats.flush_pass(gap);
        let max_bytes = self.options.max_pull_bytes;
        let conns: Vec<ConnectionId> = self.conns.keys().copied().collect();
        for conn in conns {
            let Some(state) = self.conns.get_mut(&conn) else {
                continue;
            };
            if state.writer.capacity() != state.writer.max_capacity() {
                continue;
            }
            let mut batch: Vec<DaemonMessage> = state
                .answers
                .iter()
                .map(|request| DaemonMessage::Terminals(self.terminals.snapshot(Some(*request))))
                .collect();
            if state.stale && batch.is_empty() {
                batch.push(DaemonMessage::Terminals(self.terminals.snapshot(None)));
            }
            state.answers.clear();
            state.stale = false;
            batch.extend(self.driver.core_mut().pull(conn, max_bytes, now));
            if batch.is_empty() {
                continue;
            }
            if state.writer.try_send(WriterCmd::Batch(batch)).is_err() {
                let _ = state.socket.shutdown(Shutdown::Both);
            }
        }
    }
}
