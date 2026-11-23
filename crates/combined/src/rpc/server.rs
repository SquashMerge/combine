//! The server loop: one task owns the driver and every connection's state, and waits on the
//! listener, the connections' inbox, the backend, the terminal watch, the file store's changes
//! and three timers.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, ErrorMessage, HostId, Layout, RequestId, TermId,
};
use futures::future::BoxFuture;
use futures::stream::{BoxStream, FuturesUnordered};
use futures::StreamExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};

use super::connection::{self, ConnEvent, WriterCmd};
use super::flush::{FlushAt, FlushSchedule};
use super::socket::BoundSocket;
use super::{until_shutdown, ServeOptions, ServerParts, ServerStats};
use crate::core::{
    layout_snapshot, ops_from_wire, ConnectionId, Driver, LayoutEffect, LayoutRead, LayoutWritten,
    Layouts, MutateError, PanelRead, SpawnEffect, SpawnTicket, Spawns, TerminalList, WatchError,
    LAYOUT_FILE, LAYOUT_MAX_BYTES, PANELS_DIR, PANEL_FILE, PANEL_LIST_MAX, PANEL_MAX_BYTES,
};
use crate::model::{LayoutRevision, Millis, SpaceId, TermEvent, TermInfo};
use crate::ports::{BackendResult, FileChange, FileStore, TerminalBackend};
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
    /// Layout replies and errors owed to the connection, in the order they arose.
    layout_out: VecDeque<LayoutOut>,
    /// The space the connection watches changed since its last layout message.
    layout_stale: bool,
    /// Replies to `list_templates` and `spawn_terminal`, in the order they arose.
    spawn_out: VecDeque<DaemonMessage>,
    /// The connection's last message is queued and its input is ignored. It still counts
    /// toward `max_connections` until its writer reports the socket closed.
    closing: bool,
}

enum LayoutOut {
    /// The watched space's layout, answering `request`.
    Answer(RequestId, SpaceId),
    Error(ErrorMessage),
}

enum Wake {
    Shutdown,
    Accept(io::Result<UnixStream>),
    Inbox(ConnEvent),
    Driver,
    Watch(Option<TermEvent>),
    File(Option<FileChange>),
    Flush,
    Layout,
    Spawned(SpawnTicket, BackendResult<TermId>),
    Housekeeping,
}

type Watch = BoxStream<'static, TermEvent>;
type Changes = BoxStream<'static, FileChange>;
type Spawning = FuturesUnordered<BoxFuture<'static, (SpawnTicket, BackendResult<TermId>)>>;

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
    layouts: Layouts,
    store: Arc<dyn FileStore>,
    changes: Option<Changes>,
    flush_timer: Pin<Box<Sleep>>,
    flush_armed: bool,
    layout_timer: Pin<Box<Sleep>>,
    layout_armed: bool,
    spawns: Spawns,
    spawning: Spawning,
    housekeeping: Interval,
}

/// What the backend reported when the server started watching it.
pub struct Watched {
    watch: Watch,
    list: Vec<TermInfo>,
}

/// Watches the backend's terminals and then lists them, so no change between the two is lost.
pub async fn watch_backend(backend: &dyn TerminalBackend) -> Result<Watched, ServeError> {
    let watch = backend
        .watch()
        .await
        .map_err(|error| ServeError::Backend(format!("cannot watch terminals: {error}")))?;
    let list = backend
        .list()
        .await
        .map_err(|error| ServeError::Backend(format!("cannot list terminals: {error}")))?;
    Ok(Watched { watch, list })
}

/// Serves `socket` until `shutdown` completes or something fails.
///
/// The initial watch and list race `shutdown`, so a backend that never answers can't keep the
/// server from stopping.
pub async fn serve(
    socket: BoundSocket,
    parts: ServerParts,
    options: ServeOptions,
    shutdown: impl Future<Output = ()>,
    stats: Arc<ServerStats>,
) -> Result<(), ServeError> {
    let mut shutdown = std::pin::pin!(shutdown);
    let backend = Arc::clone(parts.driver.backend());
    match until_shutdown(shutdown.as_mut(), watch_backend(&*backend)).await {
        None => Ok(()),
        Some(watched) => serve_watched(socket, parts, watched?, options, shutdown, stats).await,
    }
}

/// Serves `socket` from a backend that is already watched and listed, until `shutdown`
/// completes or something fails.
pub async fn serve_watched(
    socket: BoundSocket,
    parts: ServerParts,
    watched: Watched,
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
        file_store,
        layout_timing,
        project_dirs,
    } = parts;
    let Watched { watch, list } = watched;
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
        layouts: Layouts::new(layout_timing),
        changes: Some(file_store.changes()),
        store: file_store,
        flush_timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
        flush_armed: false,
        layout_timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
        layout_armed: false,
        spawns: Spawns::new(project_dirs),
        spawning: FuturesUnordered::new(),
        housekeeping,
    };
    let result = server.run(shutdown).await;
    server.layouts.flush_all(server.clock.now());
    server.run_layout_effects().await;
    server.close_all();
    drop(lock);
    result
}

async fn next_item<T>(watch: &mut Option<BoxStream<'static, T>>) -> Option<T> {
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
                item = next_item(&mut self.watch), if self.watch.is_some() => Wake::Watch(item),
                item = next_item(&mut self.changes), if self.changes.is_some() => Wake::File(item),
                () = &mut self.flush_timer, if self.flush_armed => Wake::Flush,
                () = &mut self.layout_timer, if self.layout_armed => Wake::Layout,
                Some((ticket, result)) = self.spawning.next(), if !self.spawning.is_empty() => Wake::Spawned(ticket, result),
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
                Wake::File(Some(change)) => {
                    if change.path == LAYOUT_FILE {
                        self.layouts.apply_change(&change.space);
                    }
                }
                Wake::File(None) => self.changes = None,
                Wake::Layout => {
                    self.layout_armed = false;
                    self.layouts.tick(self.clock.now());
                }
                Wake::Flush => {
                    self.flush_armed = false;
                    let now = self.clock.now();
                    match self.schedule.request(now) {
                        FlushAt::Now => self.flush(now),
                        FlushAt::At(at) => self.arm(at),
                    }
                }
                Wake::Spawned(ticket, result) => self.spawns.apply_spawned(ticket, result),
                Wake::Housekeeping => self.driver.core_mut().tick(self.clock.now()),
            }
            self.settle().await?;
        }
    }

    /// Takes a new connection, or closes it unanswered when the server is full.
    fn admit(&mut self, stream: UnixStream) {
        let admitted = std::time::Instant::now();
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
            admitted,
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
                layout_out: VecDeque::new(),
                layout_stale: false,
                spawn_out: VecDeque::new(),
                closing: false,
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
                if !self.conns.get(&conn).is_some_and(|state| !state.closing) {
                    return;
                }
                match message {
                    ClientMessage::WatchTerminals(watch) => {
                        self.watch_terminals(conn, watch.request)
                    }
                    ClientMessage::WatchLayout(watch) => {
                        self.watch_layout(conn, watch.request, watch.space, now)
                    }
                    ClientMessage::UnwatchLayout(_) => self.layouts.unwatch(conn, now),
                    ClientMessage::MutateLayout(mutate) => self.mutate_layout(conn, mutate, now),
                    ClientMessage::ListTemplates(list) => {
                        let space = SpaceId(list.space);
                        let listed = self.terminals.space_listed(&space);
                        self.spawns
                            .list_templates(conn, list.request, space, listed);
                    }
                    ClientMessage::SpawnTerminal(spawn) => {
                        let space = SpaceId(spawn.space);
                        let listed = self.terminals.space_listed(&space);
                        self.spawns.spawn_terminal(
                            conn,
                            spawn.request,
                            space,
                            spawn.source,
                            listed,
                        );
                    }
                    message => self.driver.core_mut().client_message(conn, message, now),
                }
            }
            ConnEvent::Malformed(conn, text) => {
                self.driver.core_mut().disconnect(conn, now);
                self.layouts.unwatch(conn, now);
                self.spawns.disconnect(conn);
                let Some(state) = self.conns.get_mut(&conn) else {
                    return;
                };
                if state.closing {
                    return;
                }
                state.closing = true;
                state.watching = false;
                state.answers.clear();
                state.stale = false;
                state.layout_out.clear();
                state.layout_stale = false;
                state.spawn_out.clear();
                let error = DaemonMessage::Error(ErrorMessage {
                    code: ErrorCode::Malformed,
                    message: format!("malformed frame: {text}"),
                    request: None,
                    attachment: None,
                });
                // The slot stays taken until the writer has sent this and closed the socket
                // and then reports `Closed`; the write timeout bounds a blocked peer.
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

    fn watch_layout(&mut self, conn: ConnectionId, request: RequestId, space: String, now: Millis) {
        let space = SpaceId(space);
        let known = self
            .terminals
            .snapshot(None)
            .spaces
            .iter()
            .any(|listed| listed.id == space.0);
        match self.layouts.watch(conn, space, request.0, known, now) {
            Ok(()) => {}
            Err(WatchError::UnknownSpace) => self.layout_error(
                conn,
                ErrorCode::UnknownSpace,
                "the host lists no such space".to_string(),
                request,
            ),
        }
    }

    fn mutate_layout(
        &mut self,
        conn: ConnectionId,
        mutate: combine_protocol::MutateLayout,
        now: Millis,
    ) {
        let request = mutate.request;
        let ops = match ops_from_wire(mutate.ops) {
            Ok(ops) => ops,
            Err(text) => return self.layout_error(conn, ErrorCode::InvalidRequest, text, request),
        };
        let base = LayoutRevision(mutate.base_layout_revision);
        let space = SpaceId(mutate.space);
        if let Err(error) = self.layouts.mutate(conn, &space, request.0, base, ops, now) {
            let (code, text) = match error {
                MutateError::UnknownSpace => (
                    ErrorCode::UnknownSpace,
                    "the host lists no such space".to_string(),
                ),
                MutateError::NotWatched => (
                    ErrorCode::InvalidRequest,
                    "the connection isn't watching that space".to_string(),
                ),
                MutateError::Stale => (
                    ErrorCode::StaleLayout,
                    "the layout changed since the revision this change was made against"
                        .to_string(),
                ),
                MutateError::Invalid(text) => (ErrorCode::InvalidRequest, text),
            };
            self.layout_error(conn, code, text, request);
        }
    }

    fn layout_error(
        &mut self,
        conn: ConnectionId,
        code: ErrorCode,
        message: String,
        request: RequestId,
    ) {
        self.queue_layout_out(
            conn,
            LayoutOut::Error(ErrorMessage {
                code,
                message,
                request: Some(request),
                attachment: None,
            }),
        );
    }

    /// Queues a layout reply or error for `conn`, and disconnects it when it has left more than
    /// `watch_answer_limit` of them unread, as for `watch_terminals` answers.
    fn queue_layout_out(&mut self, conn: ConnectionId, item: LayoutOut) {
        let Some(state) = self.conns.get_mut(&conn).filter(|state| !state.closing) else {
            return;
        };
        state.layout_out.push_back(item);
        if state.layout_out.len() > self.options.watch_answer_limit {
            eprintln!(
                "combined: connection {} left more than {} layout replies unread; disconnecting",
                conn.0, self.options.watch_answer_limit
            );
            self.driver.core_mut().disconnect(conn, self.clock.now());
            self.remove(conn);
        }
    }

    /// Queues a template or spawn reply for `conn`, and disconnects it when it has left more
    /// than `watch_answer_limit` of them unread.
    fn queue_spawn_out(&mut self, conn: ConnectionId, message: DaemonMessage) {
        let Some(state) = self.conns.get_mut(&conn).filter(|state| !state.closing) else {
            return;
        };
        state.spawn_out.push_back(message);
        if state.spawn_out.len() > self.options.watch_answer_limit {
            eprintln!(
                "combined: connection {} left more than {} spawn replies unread; disconnecting",
                conn.0, self.options.watch_answer_limit
            );
            self.driver.core_mut().disconnect(conn, self.clock.now());
            self.remove(conn);
        }
    }

    /// Forgets the connection and closes its socket, which ends both its threads. It does
    /// nothing for a connection already removed.
    fn remove(&mut self, conn: ConnectionId) {
        self.layouts.unwatch(conn, self.clock.now());
        self.spawns.disconnect(conn);
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
        self.conns.iter().any(|(conn, state)| {
            core.has_output(*conn)
                || !state.answers.is_empty()
                || state.stale
                || !state.layout_out.is_empty()
                || state.layout_stale
                || !state.spawn_out.is_empty()
        })
    }

    /// Runs the file store work the layouts asked for. Each outcome is reported before the next
    /// effect runs and before the change stream is polled again, so the hash of the daemon's
    /// own write is known by the time its echo arrives.
    async fn run_layout_effects(&mut self) {
        loop {
            let effects = self.layouts.take_effects();
            if effects.is_empty() {
                return;
            }
            for effect in effects {
                match effect {
                    LayoutEffect::Watch { space } => {
                        if let Err(error) = self.store.watch(&space).await {
                            eprintln!("combined: cannot watch {}: {error}", space.0);
                        }
                    }
                    LayoutEffect::Read { space } => {
                        let read =
                            match self.store.read(&space, LAYOUT_FILE, LAYOUT_MAX_BYTES).await {
                                Ok(file) => LayoutRead::from(file),
                                Err(error) => LayoutRead::from(error),
                            };
                        self.layouts.apply_read(&space, read, self.clock.now());
                    }
                    LayoutEffect::Write { space, bytes, .. } => {
                        let written = match self.store.write(&space, LAYOUT_FILE, bytes).await {
                            Ok(outcome) => LayoutWritten::from(outcome),
                            Err(error) => LayoutWritten::from(error),
                        };
                        self.layouts.apply_write(&space, written, self.clock.now());
                    }
                }
            }
        }
    }

    /// Queues what the layouts owe their watchers.
    fn queue_layouts(&mut self) {
        for reply in self.layouts.take_replies() {
            self.queue_layout_out(
                reply.conn,
                LayoutOut::Answer(RequestId(reply.request), reply.space),
            );
        }
        for space in self.layouts.take_changed() {
            for conn in self.layouts.subscribers(&space) {
                if let Some(state) = self.conns.get_mut(&conn).filter(|state| !state.closing) {
                    state.layout_stale = true;
                }
            }
        }
    }

    /// Runs the file store and backend work the spawns asked for. A spawn is started and not
    /// awaited here: its outcome arrives as `Wake::Spawned`. This is the only place the daemon
    /// starts a terminal, and only a `spawn_terminal` request leads here.
    async fn run_spawn_effects(&mut self) {
        loop {
            let effects = self.spawns.take_effects();
            if effects.is_empty() {
                return;
            }
            for effect in effects {
                match effect {
                    SpawnEffect::ListPanels { ticket, space } => {
                        let listing = self
                            .store
                            .list_dir(&space, PANELS_DIR, PANEL_LIST_MAX)
                            .await;
                        self.spawns.apply_listing(ticket, listing);
                    }
                    SpawnEffect::ReadPanels {
                        ticket,
                        space,
                        panels,
                    } => {
                        let limit = usize::try_from(PANEL_MAX_BYTES).unwrap_or(usize::MAX);
                        let mut reads = Vec::with_capacity(panels.len());
                        for panel in panels {
                            let path = format!("{PANELS_DIR}/{panel}/{PANEL_FILE}");
                            let read = self.store.read(&space, &path, limit).await;
                            reads.push((panel, PanelRead::from(read)));
                        }
                        self.spawns.apply_panels(ticket, reads);
                    }
                    SpawnEffect::Spawn { ticket, spec } => {
                        let backend = Arc::clone(self.driver.backend());
                        self.spawning
                            .push(Box::pin(async move { (ticket, backend.spawn(spec).await) }));
                    }
                }
            }
        }
    }

    /// Queues the replies the spawns owe.
    fn queue_spawn_replies(&mut self) {
        for (conn, message) in self.spawns.take_replies() {
            self.queue_spawn_out(conn, message);
        }
    }

    /// Arms the layout timer for the earliest write due.
    fn arm_layouts(&mut self) {
        match self.layouts.next_deadline() {
            Some(at) => {
                let deadline = Instant::from_std(self.clock.instant_at(at));
                self.layout_timer.as_mut().reset(deadline);
                self.layout_armed = true;
            }
            None => self.layout_armed = false,
        }
    }

    /// Runs core's effects, drops what it disconnected, keeps the ID reservation ahead, and
    /// flushes or arms the flush timer when output is waiting.
    async fn settle(&mut self) -> Result<(), ServeError> {
        loop {
            self.driver.run_effects(self.clock.now()).await;
            self.run_layout_effects().await;
            self.queue_layouts();
            self.run_spawn_effects().await;
            self.queue_spawn_replies();
            self.arm_layouts();
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
            if state.closing || state.writer.capacity() != state.writer.max_capacity() {
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
            let mut answered = false;
            for item in state.layout_out.drain(..) {
                match item {
                    LayoutOut::Answer(request, space) => {
                        if let Some(message) = layout_message(&self.layouts, &space, Some(request))
                        {
                            answered = true;
                            batch.push(message);
                        }
                    }
                    LayoutOut::Error(error) => batch.push(DaemonMessage::Error(error)),
                }
            }
            if std::mem::take(&mut state.layout_stale) && !answered {
                let push = self
                    .layouts
                    .watched(conn)
                    .and_then(|space| layout_message(&self.layouts, space, None));
                batch.extend(push);
            }
            batch.extend(state.spawn_out.drain(..));
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

/// The space's layout as a message, answering `request` or pushed when it is `None`. Nothing
/// before the space's first read.
fn layout_message(
    layouts: &Layouts,
    space: &SpaceId,
    request: Option<RequestId>,
) -> Option<DaemonMessage> {
    let status = layouts.status(space)?;
    Some(match layout_snapshot(space, &status) {
        Ok(layout) => DaemonMessage::Layout(Layout { request, layout }),
        Err(error) => DaemonMessage::Error(ErrorMessage {
            code: ErrorCode::Internal,
            message: format!("cannot send the layout: {error}"),
            request,
            attachment: None,
        }),
    })
}
