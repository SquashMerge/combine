//! The server loop: one task owns the driver and every connection's state, and waits on the
//! listener, the connections' inbox, the backend, the terminal watch, the file store's changes,
//! the panel API's calls, the diagram checks that run beside it, and four timers.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use combine_protocol::{
    ClientMessage, ClosePanel, DaemonMessage, DroppedNotice, ErrorCode, ErrorMessage, GetPanel,
    HostId, JsonRpcMessage, Layout, Mcp, McpOpen, McpOpened, PanelClosed, PanelState, Panels,
    PinPanel, ReportRender, RequestId, TermId,
};
use futures::future::BoxFuture;
use futures::stream::{BoxStream, FuturesUnordered};
use futures::StreamExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinError;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};

use super::connection::{self, ConnEvent, WriterCmd};
use super::flush::{FlushAt, FlushSchedule};
use super::mcp_channel::{self, ChannelStart, McpLink, Offer};
use super::panel_calls::{PanelCall, PanelHandle};
use super::panel_lane::{LaneOutcome, PanelIo};
use super::socket::BoundSocket;
use super::{until_shutdown, ServeOptions, ServerParts, ServerStats};
use crate::core::{
    dropped_notice, layout_snapshot, ops_from_wire, panel_error_code, panel_folder_refusal,
    panel_reply, panels_loading, panels_message, pin_answer, resolve_caller, ClientRender,
    ConnectionId, Driver, LayoutEffect, LayoutOp, LayoutRead, LayoutWritten, Layouts, MutateError,
    PanelApiError, PanelCreate, PanelEffect, PanelEvent, PanelList, PanelRead, PanelStore,
    PanelView, RenderStatuses, SpawnEffect, SpawnTicket, Spawns, TerminalList, WatchError,
    LAYOUT_FILE, LAYOUT_MAX_BYTES, PANELS_DIR, PANEL_FILE, PANEL_LIST_MAX, PANEL_MAX_BYTES,
};
use crate::mcp_server::{McpAudit, McpOptions};
use crate::model::{
    LayoutRevision, Lifetime, Millis, PanelId, SpaceId, TermEvent, TermInfo, TileId,
};
use crate::ports::{
    BackendResult, CallerIdentity, DiagramCheck, DiagramCheckError, DiagramVerdict, FileChange,
    FileMark, FileStore, StoreError, TerminalBackend, WallClock,
};
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
    /// The connection's MCP channel, once its `mcp_open` was accepted. Dropping it closes the
    /// channel.
    mcp: Option<McpLink>,
    /// Replies to `get_panel`, in the order they arose.
    panel_out: VecDeque<PanelOut>,
    /// The panels of the watched space changed since the connection's last `panels` message.
    panels_stale: bool,
    /// The connection's last `panels` message was the loading form, so it is owed another once
    /// the space has loaded.
    panels_loading_sent: bool,
    /// The request of a `watch_layout` whose answer has not been flushed. No `panels` push
    /// precedes that answer.
    watch_unanswered: Option<RequestId>,
    /// Replies to `mcp_open` and `mcp`, in the order they arose.
    mcp_out: VecDeque<DaemonMessage>,
    /// The connection's MCP channel thread has ended.
    mcp_ended: bool,
    /// The connection's last message is queued and its input is ignored. It still counts
    /// toward `max_connections` until its writer reports the socket closed.
    closing: bool,
}

/// A reply owed to a `get_panel`.
enum PanelOut {
    /// Answered from the store when it is flushed.
    Get {
        request: RequestId,
        space: SpaceId,
        panel: String,
    },
    Error(ErrorMessage),
    /// A reply that is already complete.
    Answer(DaemonMessage),
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
    Checked(CheckDone),
    Panel,
    PanelCall(Option<PanelCall>),
    PanelIo(Result<LaneOutcome, JoinError>),
    Housekeeping,
    WatchRetry,
}

/// A diagram check that finished, for the panel it was started for.
struct CheckDone {
    space: SpaceId,
    panel: String,
    revision: u64,
    result: Result<DiagramVerdict, DiagramCheckError>,
}

/// Panel work that arrived while the lane was busy, applied in arrival order once it is idle.
enum Deferred {
    Change(SpaceId, String),
    Check(CheckDone),
    AddSpaces,
    Client(ClientPanelCall),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClientPanelKind {
    Close,
    Pin,
}

/// A `close_panel` or `pin_panel` a client sent.
struct ClientPanelCall {
    conn: ConnectionId,
    request: RequestId,
    space: SpaceId,
    panel: String,
    kind: ClientPanelKind,
}

/// A client call whose store call has run, answered at the next idle point once its file work is done.
struct ClientAnswer {
    call: ClientPanelCall,
    /// The store's refusal, if it refused the call.
    refused: Option<PanelApiError>,
}

/// A reply to a panel call, held until the call's file work has run.
enum PanelAnswer {
    View(
        oneshot::Sender<Result<PanelView, PanelApiError>>,
        Result<PanelView, PanelApiError>,
    ),
    List(
        oneshot::Sender<Result<PanelList, PanelApiError>>,
        Result<PanelList, PanelApiError>,
    ),
    Get(
        oneshot::Sender<Result<(PanelView, String), PanelApiError>>,
        Result<(PanelView, String), PanelApiError>,
    ),
    Render(
        oneshot::Sender<Result<Vec<ClientRender>, PanelApiError>>,
        Result<Vec<ClientRender>, PanelApiError>,
    ),
    Dropped(
        oneshot::Sender<Result<Option<DroppedNotice>, PanelApiError>>,
        Result<Option<DroppedNotice>, PanelApiError>,
    ),
}

impl PanelAnswer {
    /// The caller may have stopped waiting, which is not an error.
    fn send(self) {
        match self {
            PanelAnswer::View(reply, result) => drop(reply.send(result)),
            PanelAnswer::List(reply, result) => drop(reply.send(result)),
            PanelAnswer::Get(reply, result) => drop(reply.send(result)),
            PanelAnswer::Render(reply, result) => drop(reply.send(result)),
            PanelAnswer::Dropped(reply, result) => drop(reply.send(result)),
        }
    }
}

type Watch = BoxStream<'static, TermEvent>;
type Changes = BoxStream<'static, FileChange>;
type Spawning = FuturesUnordered<BoxFuture<'static, (SpawnTicket, BackendResult<TermId>)>>;
type Checking = FuturesUnordered<BoxFuture<'static, CheckDone>>;

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
    panels: PanelStore,
    panel_io: PanelIo,
    /// The panel file effect running on a blocking thread, if any.
    panel_running: Option<BoxFuture<'static, Result<LaneOutcome, JoinError>>>,
    /// Effects taken from the panel store and not started yet.
    panel_queue: VecDeque<PanelEffect>,
    /// Panel work that arrived while the lane was busy.
    panel_deferred: VecDeque<Deferred>,
    /// A `create` waiting for its new space to load.
    parked_create: Option<PanelCall>,
    /// Panels a close or remove may have deleted: each is taken out of its space's layout at the
    /// next idle point, if the store then holds it removed.
    removals: Vec<(SpaceId, String)>,
    /// Client close and pin calls the store has taken, answered at the next idle point.
    client_answers: Vec<ClientAnswer>,
    render: RenderStatuses,
    diagram_check: Arc<dyn DiagramCheck>,
    wall: Arc<dyn WallClock>,
    /// Diagram checks that run beside the loop, never awaited inline.
    checking: Checking,
    panel_timer: Pin<Box<Sleep>>,
    panel_armed: bool,
    panel_calls: Option<BoxStream<'static, PanelCall>>,
    panel_answers: Vec<PanelAnswer>,
    housekeeping: Interval,
    /// The spaces the file store could not watch, with the error of the last attempt.
    unwatched: BTreeMap<SpaceId, String>,
    watch_retry: Interval,
    callers: Option<Arc<dyn CallerIdentity>>,
    audit: Arc<dyn McpAudit>,
    /// The server's own panel channel, for the MCP channels' tool threads.
    mcp_panels: PanelHandle,
    /// MCP slots taken, which is what `max_mcp_connections` counts. A slot is taken by an
    /// accepted `mcp_open` and freed once the connection is removed and its channel thread has
    /// ended, in either order.
    mcp_slots: usize,
    /// Removed connections whose MCP channel thread has not ended yet; each still holds a slot.
    mcp_lingering: HashSet<ConnectionId>,
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
        diagram_check,
        wall_clock,
        scratch,
        panel_timing,
        panel_calls,
        caller_identity,
        mcp_audit,
    } = parts;
    let Watched { watch, list } = watched;
    let file_store_for_lane = Arc::clone(&file_store);
    terminals.apply_list(&list);
    terminals.take_changed();
    let (mcp_calls, mut mcp_rx) = mpsc::channel(64);
    let internal = futures::stream::poll_fn(move |cx| mcp_rx.poll_recv(cx)).boxed();
    let panel_calls = match panel_calls {
        Some(mut calls) => {
            let external = futures::stream::poll_fn(move |cx| calls.poll_recv(cx));
            futures::stream::select(external, internal).boxed()
        }
        None => internal,
    };

    let (inbox_tx, inbox) = mpsc::channel(INBOX);
    let mut housekeeping = tokio::time::interval(options.housekeeping);
    housekeeping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut watch_retry =
        tokio::time::interval_at(Instant::now() + options.watch_retry, options.watch_retry);
    watch_retry.set_missed_tick_behavior(MissedTickBehavior::Delay);
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
        panels: PanelStore::new(panel_timing),
        panel_io: PanelIo::new(Arc::clone(&file_store_for_lane), scratch),
        panel_running: None,
        panel_queue: VecDeque::new(),
        panel_deferred: VecDeque::new(),
        parked_create: None,
        removals: Vec::new(),
        client_answers: Vec::new(),
        render: RenderStatuses::new(),
        diagram_check,
        wall: wall_clock,
        checking: FuturesUnordered::new(),
        panel_timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
        panel_armed: false,
        panel_calls: Some(panel_calls),
        panel_answers: Vec::new(),
        housekeeping,
        unwatched: BTreeMap::new(),
        watch_retry,
        callers: caller_identity,
        audit: mcp_audit,
        mcp_panels: PanelHandle::new(mcp_calls),
        mcp_slots: 0,
        mcp_lingering: HashSet::new(),
    };
    let mut shutdown = std::pin::pin!(shutdown);
    let now = server.clock.now();
    server.panels.start(now);
    server.add_terminal_spaces(now);
    // The start-up load runs before the first wake, so the timers are armed and a call that
    // arrives first finds the spaces loading or loaded.
    let result = match until_shutdown(shutdown.as_mut(), server.settle()).await {
        None => Ok(()),
        Some(Err(error)) => Err(error),
        Some(Ok(())) => server.run(shutdown.as_mut()).await,
    };
    // The panel file work in flight is awaited, so nothing is left running on a blocking
    // thread. Nothing more starts.
    if let Some(running) = server.panel_running.take() {
        let _ = running.await;
    }
    // A write that found an outside edit is rebased by the conflict and written by the second
    // pass. Panels have nothing to flush: a save retry still held here is lost, and a call
    // still queued is dropped with the receiver, which its handle reports as a stopped server.
    for _ in 0..2 {
        server.layouts.flush_all(server.clock.now());
        server.run_layout_effects().await;
    }
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

async fn lane_next(
    running: &mut Option<BoxFuture<'static, Result<LaneOutcome, JoinError>>>,
) -> Result<LaneOutcome, JoinError> {
    match running {
        Some(running) => running.await,
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
                Some(done) = self.checking.next(), if !self.checking.is_empty() => Wake::Checked(done),
                () = &mut self.panel_timer, if self.panel_armed && self.panel_lane_idle() => Wake::Panel,
                item = next_item(&mut self.panel_calls), if self.panel_calls.is_some() && self.panel_lane_idle() && self.parked_create.is_none() => Wake::PanelCall(item),
                result = lane_next(&mut self.panel_running), if self.panel_running.is_some() => Wake::PanelIo(result),
                _ = self.housekeeping.tick() => Wake::Housekeeping,
                _ = self.watch_retry.tick() => Wake::WatchRetry,
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
                        if self.panel_lane_idle() {
                            self.add_terminal_spaces(self.clock.now());
                        } else if !self
                            .panel_deferred
                            .iter()
                            .any(|item| matches!(item, Deferred::AddSpaces))
                        {
                            self.panel_deferred.push_back(Deferred::AddSpaces);
                        }
                    }
                }
                Wake::Watch(None) => {
                    return Err(ServeError::Backend("terminal watch ended".to_string()))
                }
                Wake::File(Some(change)) => {
                    if change.path == LAYOUT_FILE {
                        self.layouts.apply_change(&change.space);
                    } else if change.path == PANELS_DIR
                        || change.path.starts_with(&format!("{PANELS_DIR}/"))
                    {
                        self.panel_change(change.space, change.path);
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
                Wake::Checked(done) => {
                    if self.panel_lane_idle() {
                        self.apply_check(done);
                    } else {
                        self.panel_deferred.push_back(Deferred::Check(done));
                    }
                }
                Wake::Panel => {
                    self.panel_armed = false;
                    self.panels.tick(self.clock.now(), self.wall.now_unix_ms());
                }
                Wake::PanelCall(Some(call)) => self.panel_call(call),
                Wake::PanelCall(None) => self.panel_calls = None,
                Wake::PanelIo(result) => {
                    self.panel_running = None;
                    match result {
                        Ok(outcome) => self.apply_lane_outcome(outcome),
                        Err(error) if error.is_panic() => {
                            std::panic::resume_unwind(error.into_panic())
                        }
                        Err(_) => {
                            return Err(ServeError::State(
                                "the panel file work was cancelled".to_string(),
                            ))
                        }
                    }
                }
                Wake::Housekeeping => self.driver.core_mut().tick(self.clock.now()),
                Wake::WatchRetry => self.retry_watches().await,
            }
            self.settle().await?;
        }
    }

    /// Takes a new connection, or closes it unanswered when the server is full.
    fn admit(&mut self, stream: UnixStream) {
        let admitted = std::time::Instant::now();
        // MCP connections have their own cap, so they never take a client's slot.
        let clients = self
            .conns
            .values()
            .filter(|state| state.mcp.is_none())
            .count();
        if clients >= self.options.max_connections {
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
                panel_out: VecDeque::new(),
                panels_stale: false,
                panels_loading_sent: false,
                watch_unanswered: None,
                mcp: None,
                mcp_out: VecDeque::new(),
                mcp_ended: false,
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
                    ClientMessage::UnwatchLayout(_) => {
                        self.layouts.unwatch(conn, now);
                        if let Some(state) = self.conns.get_mut(&conn) {
                            state.watch_unanswered = None;
                        }
                    }
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
                    ClientMessage::GetPanel(get) => self.get_panel(conn, get),
                    ClientMessage::ClosePanel(ClosePanel {
                        request,
                        space,
                        panel,
                    }) => self.client_panel_call(ClientPanelCall {
                        conn,
                        request,
                        space: SpaceId(space),
                        panel,
                        kind: ClientPanelKind::Close,
                    }),
                    ClientMessage::PinPanel(PinPanel {
                        request,
                        space,
                        panel,
                    }) => self.client_panel_call(ClientPanelCall {
                        conn,
                        request,
                        space: SpaceId(space),
                        panel,
                        kind: ClientPanelKind::Pin,
                    }),
                    ClientMessage::ReportRender(report) => self.report_render(conn, report),
                    ClientMessage::McpOpen(open) => self.mcp_open(conn, open),
                    ClientMessage::Mcp(mcp) => self.mcp_message(conn, mcp.message),
                    message => self.driver.core_mut().client_message(conn, message, now),
                }
            }
            ConnEvent::Malformed(conn, text) => {
                self.driver.core_mut().disconnect(conn, now);
                self.layouts.unwatch(conn, now);
                self.spawns.disconnect(conn);
                self.render.disconnect(conn);
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
                state.panel_out.clear();
                state.panels_stale = false;
                state.panels_loading_sent = false;
                state.watch_unanswered = None;
                state.mcp_out.clear();
                if let Some(link) = &state.mcp {
                    link.close();
                }
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
            ConnEvent::McpReply(conn, message) => {
                self.queue_mcp_out(conn, DaemonMessage::Mcp(Mcp { message }));
            }
            ConnEvent::McpEnded(conn) => {
                if self.mcp_lingering.remove(&conn) {
                    self.release_mcp_slot();
                    return;
                }
                let Some(state) = self.conns.get_mut(&conn) else {
                    return;
                };
                state.mcp_ended = true;
                // A thread that ended while its connection is still open died on its own: the
                // shim would wait for replies that never come. A closing connection keeps its
                // slot until its socket is closed.
                if !state.closing {
                    self.driver.core_mut().disconnect(conn, now);
                    self.remove(conn);
                }
            }
        }
    }

    /// Opens the connection's MCP channel, or answers why not. It never blocks: the tool thread
    /// does the calls.
    fn mcp_open(&mut self, conn: ConnectionId, open: McpOpen) {
        let request = open.request;
        let refuse = |server: &mut Server, code, text: &str| {
            server.queue_mcp_out(
                conn,
                DaemonMessage::Error(ErrorMessage {
                    code,
                    message: text.to_string(),
                    request: Some(request),
                    attachment: None,
                }),
            );
        };
        if self
            .conns
            .get(&conn)
            .is_some_and(|state| state.mcp.is_some())
        {
            return refuse(
                self,
                ErrorCode::InvalidRequest,
                "the MCP channel is already open on this connection",
            );
        }
        let limit = self.options.max_mcp_connections;
        if self.mcp_slots >= limit {
            return refuse(
                self,
                ErrorCode::InvalidRequest,
                &format!("too many MCP connections; the limit is {limit}"),
            );
        }
        let Some(caller) = resolve_caller(&open.caller, self.callers.as_deref(), &self.terminals)
        else {
            return refuse(
                self,
                ErrorCode::UnknownCaller,
                "no terminal or space matches the caller",
            );
        };
        let space = caller.space.0.clone();
        let started = mcp_channel::start(ChannelStart {
            conn,
            caller,
            panels: self.mcp_panels.clone(),
            inbox: self.inbox_tx.clone(),
            audit: Arc::clone(&self.audit),
            options: McpOptions {
                wait: self.options.mcp_wait,
                poll: self.options.mcp_poll,
            },
            queue: self.options.watch_answer_limit,
        });
        let link = match started {
            Ok(link) => link,
            Err(error) => {
                eprintln!("combined: cannot start an MCP channel: {error}");
                return refuse(self, ErrorCode::Internal, "couldn't start the MCP channel");
            }
        };
        let Some(state) = self.conns.get_mut(&conn) else {
            return;
        };
        state.mcp = Some(link);
        self.mcp_slots += 1;
        self.stats.mcp_started();
        self.queue_mcp_out(conn, DaemonMessage::McpOpened(McpOpened { request, space }));
    }

    /// Hands an `mcp` frame to the connection's tool thread. With no open channel it is
    /// ignored: `mcp` is never answered by an `error`.
    fn mcp_message(&mut self, conn: ConnectionId, message: JsonRpcMessage) {
        let Some(link) = self.conns.get(&conn).and_then(|state| state.mcp.as_ref()) else {
            return;
        };
        match link.offer(message) {
            Offer::Queued => return,
            Offer::Full => eprintln!(
                "combined: connection {} left more than {} MCP requests waiting; disconnecting",
                conn.0, self.options.watch_answer_limit
            ),
            Offer::Gone => {}
        }
        self.driver.core_mut().disconnect(conn, self.clock.now());
        self.remove(conn);
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
        if let Some(state) = self.conns.get_mut(&conn) {
            state.watch_unanswered = Some(request);
        }
        match self.layouts.watch(conn, space, request.0, known, now) {
            Ok(()) => {}
            Err(WatchError::UnknownSpace) => {
                if let Some(state) = self.conns.get_mut(&conn) {
                    state.watch_unanswered = None;
                }
                self.layout_error(
                    conn,
                    ErrorCode::UnknownSpace,
                    "the host lists no such space".to_string(),
                    request,
                )
            }
        }
    }

    fn mutate_layout(
        &mut self,
        conn: ConnectionId,
        mutate: combine_protocol::MutateLayout,
        now: Millis,
    ) {
        let request = mutate.request;
        let mut ops = match ops_from_wire(mutate.ops) {
            Ok(ops) => ops,
            Err(text) => return self.layout_error(conn, ErrorCode::InvalidRequest, text, request),
        };
        let base = LayoutRevision(mutate.base_layout_revision);
        let space = SpaceId(mutate.space);
        for op in &mut ops {
            if let LayoutOp::AddPanel {
                panel,
                tile: tile @ None,
                ..
            } = op
            {
                *tile = self
                    .panels
                    .get(&space, panel.as_str())
                    .ok()
                    .and_then(|(view, _)| view.file.placement)
                    .and_then(|hint| TileId::new(hint).ok());
            }
        }
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

    /// Queues a `get_panel` reply for `conn`, and disconnects it when it has left more than
    /// `watch_answer_limit` of them unread.
    fn queue_panel_out(&mut self, conn: ConnectionId, item: PanelOut) {
        let Some(state) = self.conns.get_mut(&conn).filter(|state| !state.closing) else {
            return;
        };
        state.panel_out.push_back(item);
        if state.panel_out.len() > self.options.watch_answer_limit {
            eprintln!(
                "combined: connection {} left more than {} panel replies unread; disconnecting",
                conn.0, self.options.watch_answer_limit
            );
            self.driver.core_mut().disconnect(conn, self.clock.now());
            self.remove(conn);
        }
    }

    /// Queues the answer to a `get_panel`. The answer is read from the store when it is
    /// flushed, so it is the revision current then.
    fn get_panel(&mut self, conn: ConnectionId, get: GetPanel) {
        let item = match panel_folder_refusal(&get.panel) {
            Some(why) => PanelOut::Error(ErrorMessage {
                code: ErrorCode::InvalidRequest,
                message: format!("the panel ID isn't valid: {why}"),
                request: Some(get.request),
                attachment: None,
            }),
            None => PanelOut::Get {
                request: get.request,
                space: SpaceId(get.space),
                panel: get.panel,
            },
        };
        self.queue_panel_out(conn, item);
    }

    /// Runs a client's `close_panel` or `pin_panel` now when the panel lane is idle and no create is
    /// parked, and otherwise once it is, so the store is never changed while lane work runs.
    fn client_panel_call(&mut self, call: ClientPanelCall) {
        if let Some(why) = panel_folder_refusal(&call.panel) {
            let error = ErrorMessage {
                code: ErrorCode::InvalidRequest,
                message: format!("the panel ID isn't valid: {why}"),
                request: Some(call.request),
                attachment: None,
            };
            self.queue_panel_out(call.conn, PanelOut::Error(error));
            return;
        }
        if self.panel_lane_idle() && self.parked_create.is_none() {
            self.run_client_panel_call(call);
        } else {
            self.panel_deferred.push_back(Deferred::Client(call));
        }
    }

    fn run_client_panel_call(&mut self, call: ClientPanelCall) {
        let now = self.clock.now();
        let result = match call.kind {
            ClientPanelKind::Close => self.panels.close(&call.space, &call.panel, now),
            ClientPanelKind::Pin => self.panels.pin(&call.space, &call.panel, now),
        };
        if call.kind == ClientPanelKind::Close && result.is_ok() {
            self.removals.push((call.space.clone(), call.panel.clone()));
        }
        self.client_answers.push(ClientAnswer {
            call,
            refused: result.err(),
        });
        self.pump_panels();
    }

    /// Records what the connection drew. It is never answered, not even with an error.
    fn report_render(&mut self, conn: ConnectionId, report: ReportRender) {
        let space = SpaceId(report.space);
        let current = match self.panels.get(&space, &report.panel) {
            Ok((view, _)) if view.state != PanelState::Removed => view.revision,
            _ => return,
        };
        let render = ClientRender {
            revision: report.revision,
            status: report.status,
            error: report.error,
        };
        self.render
            .record(conn, &space, &report.panel, render, current);
    }

    /// Queues an MCP reply for `conn`, and disconnects it when it has left more than
    /// `watch_answer_limit` of them unread.
    fn queue_mcp_out(&mut self, conn: ConnectionId, message: DaemonMessage) {
        let Some(state) = self.conns.get_mut(&conn).filter(|state| !state.closing) else {
            return;
        };
        state.mcp_out.push_back(message);
        if state.mcp_out.len() > self.options.watch_answer_limit {
            eprintln!(
                "combined: connection {} left more than {} MCP replies unread; disconnecting",
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
        self.render.disconnect(conn);
        if let Some(state) = self.conns.remove(&conn) {
            self.stats.removed();
            let _ = state.socket.shutdown(Shutdown::Both);
            if state.mcp.is_some() {
                if state.mcp_ended {
                    self.release_mcp_slot();
                } else {
                    self.mcp_lingering.insert(conn);
                }
            }
        }
    }

    fn release_mcp_slot(&mut self) {
        self.mcp_slots = self.mcp_slots.saturating_sub(1);
        self.stats.mcp_ended();
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
                || !state.panel_out.is_empty()
                || (state.panels_stale && state.watch_unanswered.is_none())
                || !state.mcp_out.is_empty()
        })
    }

    /// Asks the file store to watch `space`.
    async fn watch_space(&mut self, space: &SpaceId) {
        let result = self.store.watch(space).await;
        self.watched(space, result);
    }

    /// What the file store answered when asked to watch `space`. A refusal is remembered and
    /// retried every `watch_retry`, and logged when it starts or its text changes, not on every
    /// retry.
    fn watched(&mut self, space: &SpaceId, result: Result<(), StoreError>) {
        match result {
            Ok(()) => {
                if self.unwatched.remove(space).is_some() {
                    eprintln!("combined: watching {} again", space.0);
                    // Edits made while it wasn't watched were never reported.
                    self.layouts.apply_change(space);
                    self.panel_change(space.clone(), PANELS_DIR.to_string());
                }
            }
            Err(error) => {
                let detail = error.to_string();
                if self.unwatched.get(space) != Some(&detail) {
                    eprintln!(
                        "combined: cannot watch {}, so edits made outside Combine to its layout and panels are not seen until it can be: {detail}",
                        space.0
                    );
                    self.unwatched.insert(space.clone(), detail);
                }
            }
        }
    }

    async fn retry_watches(&mut self) {
        let spaces: Vec<SpaceId> = self.unwatched.keys().cloned().collect();
        for space in spaces {
            self.watch_space(&space).await;
        }
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
                        self.watch_space(&space).await;
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
                        // The file must still be what the daemon last saw. The window between
                        // this read and the write's rename can't be closed: an outside write
                        // landing in it is overwritten.
                        let expected = FileMark::from(self.layouts.known_hash(&space));
                        let current = self.store.read(&space, LAYOUT_FILE, LAYOUT_MAX_BYTES).await;
                        if !expected.matches(&current) {
                            let read = match current {
                                Ok(file) => LayoutRead::from(file),
                                Err(error) => LayoutRead::from(error),
                            };
                            self.layouts
                                .apply_write_conflict(&space, read, self.clock.now());
                            continue;
                        }
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

    /// Adds every space the terminal list names to the panels. Spaces are never dropped.
    fn add_terminal_spaces(&mut self, now: Millis) {
        for space in self.terminals.space_ids() {
            self.panels.add_space(&space, now);
        }
    }

    /// Runs one call of the panel API. A new space's listing and first reads are applied before
    /// a create runs, so the first create in a space doesn't find it loading: a create that has
    /// to wait for them is parked. The answer waits in `panel_answers` until the lane is idle.
    /// A call is only taken while the lane is idle.
    fn panel_call(&mut self, call: PanelCall) {
        let now = self.clock.now();
        let answer = match call {
            PanelCall::Create {
                space,
                create,
                reply,
            } => {
                self.panels.add_space(&space, now);
                self.pump_panels();
                if !self.panel_lane_idle() {
                    self.parked_create = Some(PanelCall::Create {
                        space,
                        create,
                        reply,
                    });
                    return;
                }
                self.create_panel(space, create, reply)
            }
            PanelCall::Update {
                space,
                id,
                update,
                reply,
            } => {
                let wall_ms = self.wall.now_unix_ms();
                let result =
                    self.panels
                        .update(&space, &id, update, now, wall_ms, self.driver.core_mut());
                PanelAnswer::View(reply, result)
            }
            PanelCall::Remove { space, id, reply } => {
                let result = self.panels.remove(&space, &id, now);
                if result.is_ok() {
                    self.removals.push((space, id));
                }
                PanelAnswer::View(reply, result)
            }
            PanelCall::Close { space, id, reply } => {
                let result = self.panels.close(&space, &id, now);
                if result.is_ok() {
                    self.removals.push((space, id));
                }
                PanelAnswer::View(reply, result)
            }
            PanelCall::Pin { space, id, reply } => {
                PanelAnswer::View(reply, self.panels.pin(&space, &id, now))
            }
            PanelCall::List { space, reply } => PanelAnswer::List(reply, self.panels.list(&space)),
            PanelCall::Status { space, id, reply } => {
                PanelAnswer::View(reply, self.panels.get(&space, &id).map(|(view, _)| view))
            }
            PanelCall::Get { space, id, reply } => {
                let result = self
                    .panels
                    .get(&space, &id)
                    .map(|(view, source)| (view, source.to_owned()));
                PanelAnswer::Get(reply, result)
            }
            PanelCall::RenderStatus { space, id, reply } => {
                PanelAnswer::Render(reply, Ok(self.render.render_status(&space, &id)))
            }
            PanelCall::LayoutDropped { space, reply } => {
                let notice = self
                    .layouts
                    .status(&space)
                    .and_then(|status| status.dropped)
                    .map(|dropped| dropped_notice(&dropped));
                PanelAnswer::Dropped(reply, Ok(notice))
            }
        };
        self.panel_answers.push(answer);
    }

    fn create_panel(
        &mut self,
        space: SpaceId,
        create: PanelCreate,
        reply: oneshot::Sender<Result<PanelView, PanelApiError>>,
    ) -> PanelAnswer {
        let now = self.clock.now();
        let wall_ms = self.wall.now_unix_ms();
        let result =
            self.panels
                .create_with_outcome(&space, create, now, wall_ms, self.driver.core_mut());
        if let Ok(outcome) = &result {
            if outcome.created {
                self.place(&space, &outcome.view, now);
                self.removals.push((space, outcome.view.id.clone()));
            }
        }
        PanelAnswer::View(reply, result.map(|outcome| outcome.view))
    }

    /// Adds a panel the store just made to its space's layout, in the tile its placement names.
    fn place(&mut self, space: &SpaceId, view: &PanelView, now: Millis) {
        let Ok(panel) = PanelId::new(view.id.as_str()) else {
            return;
        };
        let hint = view
            .file
            .placement
            .clone()
            .and_then(|tile| TileId::new(tile).ok());
        self.layouts.place(space, panel, hint, now);
    }

    /// Takes a panel out of its space's layout.
    fn unplace(&mut self, space: &SpaceId, panel: &str) {
        if let Ok(panel) = PanelId::new(panel) {
            self.layouts.unplace(space, &panel, self.clock.now());
        }
    }

    /// Takes the panels a close or remove deleted out of the layout. It runs at an idle point,
    /// when each call's file work has finished, so a remove that conflicted keeps its tab.
    fn answer_panel_requests(&mut self) {
        for (space, panel) in std::mem::take(&mut self.removals) {
            let removed = matches!(
                self.panels.get(&space, &panel),
                Ok((view, _)) if view.state == PanelState::Removed
            );
            if removed {
                self.unplace(&space, &panel);
            }
        }
        for ClientAnswer { call, refused } in std::mem::take(&mut self.client_answers) {
            let ClientPanelCall {
                conn,
                request,
                space,
                panel,
                kind,
            } = call;
            let out = match (refused, kind) {
                (Some(error), _) => PanelOut::Error(ErrorMessage {
                    code: panel_error_code(&error),
                    message: error.to_string(),
                    request: Some(request),
                    attachment: None,
                }),
                (None, ClientPanelKind::Close) => {
                    PanelOut::Answer(DaemonMessage::PanelClosed(PanelClosed {
                        request,
                        space: space.0,
                        panel,
                    }))
                }
                (None, ClientPanelKind::Pin) => PanelOut::Answer(pin_answer(
                    request,
                    &space,
                    &panel,
                    self.panels.get(&space, &panel).map(|(view, _)| view),
                )),
            };
            self.queue_panel_out(conn, out);
        }
    }

    /// Runs the create that was waiting for its space to load. The lane is idle.
    fn run_parked_create(&mut self, call: PanelCall) {
        let PanelCall::Create {
            space,
            create,
            reply,
        } = call
        else {
            return;
        };
        let answer = self.create_panel(space, create, reply);
        self.panel_answers.push(answer);
        self.pump_panels();
    }

    fn send_panel_answers(&mut self) {
        for answer in std::mem::take(&mut self.panel_answers) {
            answer.send();
        }
    }

    /// Arms the panel timer for the earliest deadline the panels have.
    fn arm_panels(&mut self) {
        match self.panels.next_deadline() {
            Some(at) => {
                let deadline = Instant::from_std(self.clock.instant_at(at));
                self.panel_timer.as_mut().reset(deadline);
                self.panel_armed = true;
            }
            None => self.panel_armed = false,
        }
    }

    /// No panel file effect is running or waiting its turn.
    fn panel_lane_idle(&self) -> bool {
        self.panel_running.is_none() && self.panel_queue.is_empty()
    }

    /// Starts the next panel effect when none is running. The effects the store asked for are
    /// started one at a time, in the order they were asked, and each outcome is applied before
    /// the next starts, so the store applies reads in the order it asked them and knows the
    /// hashes of its own writes before their echoes arrive. A check is started and not awaited:
    /// its verdict arrives as `Wake::Checked`, so a slow diagram never holds terminal output.
    fn pump_panels(&mut self) {
        loop {
            if self.panel_running.is_some() {
                return;
            }
            if self.panel_queue.is_empty() {
                self.panel_queue.extend(self.panels.take_effects());
            }
            let Some(effect) = self.panel_queue.pop_front() else {
                return;
            };
            match effect {
                PanelEffect::Check {
                    space,
                    panel,
                    revision,
                    source,
                } => {
                    let check = Arc::clone(&self.diagram_check);
                    self.checking.push(Box::pin(async move {
                        CheckDone {
                            space,
                            panel,
                            revision,
                            result: check.check(&source).await,
                        }
                    }));
                }
                PanelEffect::Log { text } => eprintln!("combined: {text}"),
                effect => {
                    self.panel_running = Some(self.panel_io.start(effect));
                    return;
                }
            }
        }
    }

    /// Applies what the lane's effect found. This and the buffer drains are the only changes to
    /// the panel store while the lane is busy.
    fn apply_lane_outcome(&mut self, outcome: LaneOutcome) {
        let now = self.clock.now();
        match outcome {
            LaneOutcome::Watched { space, result } => self.watched(&space, result),
            LaneOutcome::Listed { space, listing } => {
                self.panels.apply_listing(&space, listing, now)
            }
            LaneOutcome::ScratchListed { listing } => {
                self.panels.apply_scratch_listing(listing, now)
            }
            LaneOutcome::Read { at, file, read } => {
                self.panels
                    .apply_read(&at, file, read, now, self.driver.core_mut())
            }
            LaneOutcome::Saved { ticket, report } => self.panels.apply_saved(ticket, report, now),
            LaneOutcome::Deleted { ticket, report } => {
                self.panels.apply_deleted(ticket, report, now)
            }
        }
    }

    /// A file under `.workspace/` of `space` changed. It is applied at once when the lane is
    /// idle, and when it is next idle otherwise.
    fn panel_change(&mut self, space: SpaceId, path: String) {
        if self.panel_lane_idle() {
            self.panels.apply_change(&space, &path, self.clock.now());
        } else {
            self.panel_deferred.push_back(Deferred::Change(space, path));
        }
    }

    fn apply_check(&mut self, done: CheckDone) {
        self.panels.apply_check(
            &done.space,
            &done.panel,
            done.revision,
            done.result,
            self.clock.now(),
        );
    }

    fn apply_deferred(&mut self, item: Deferred) {
        match item {
            Deferred::Change(space, path) => {
                self.panels.apply_change(&space, &path, self.clock.now())
            }
            Deferred::Check(done) => self.apply_check(done),
            Deferred::AddSpaces => self.add_terminal_spaces(self.clock.now()),
            Deferred::Client(call) => self.run_client_panel_call(call),
        }
    }

    /// Marks the watchers of every space whose panels changed, so each is sent the new list.
    fn consume_panel_events(&mut self) {
        for event in self.panels.take_events() {
            let space = match event {
                PanelEvent::Changed {
                    space,
                    panel,
                    state: PanelState::Removed,
                    ..
                } => {
                    let expired = matches!(
                        self.panels.get(&space, &panel),
                        Ok((view, _)) if view.state == PanelState::Removed
                            && view.lifetime == Lifetime::Scratch
                    );
                    if expired {
                        self.unplace(&space, &panel);
                    }
                    space
                }
                PanelEvent::Changed { space, .. } | PanelEvent::Listing { space } => space,
            };
            for conn in self.layouts.subscribers(&space) {
                if let Some(state) = self.conns.get_mut(&conn).filter(|state| !state.closing) {
                    state.panels_stale = true;
                }
            }
        }
    }

    /// Marks the watchers that were sent the loading form of a space that has loaded since.
    fn mark_loaded_watchers(&mut self) {
        for (conn, state) in &mut self.conns {
            if !state.panels_loading_sent {
                continue;
            }
            match self.layouts.watched(*conn) {
                Some(space) => {
                    if !matches!(
                        self.panels.list(space),
                        Err(PanelApiError::SpaceLoading | PanelApiError::UnknownSpace)
                    ) {
                        state.panels_stale = true;
                    }
                }
                None => state.panels_loading_sent = false,
            }
        }
    }

    /// The panel part of a settle pass. Answers go out at the first point where the lane is
    /// idle, before a parked create or a deferred item can start new file work.
    fn settle_panels(&mut self) {
        loop {
            self.pump_panels();
            if !self.panel_lane_idle() {
                break;
            }
            self.send_panel_answers();
            self.answer_panel_requests();
            if let Some(call) = self.parked_create.take() {
                self.run_parked_create(call);
                continue;
            }
            match self.panel_deferred.pop_front() {
                Some(item) => {
                    self.apply_deferred(item);
                    continue;
                }
                None => break,
            }
        }
        self.consume_panel_events();
        self.mark_loaded_watchers();
        if self.panel_lane_idle() {
            self.arm_panels();
        } else {
            self.panel_armed = false;
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
            self.settle_panels();
            // Panel calls and answers place and unplace panels. Run that layout work in this
            // pass, so a placement is read and written without waiting for another wake.
            if self.layouts.has_work() {
                continue;
            }
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
        // Each space's `panels` body is built once per pass and given each connection's request.
        let mut bodies: BTreeMap<SpaceId, (Panels, bool)> = BTreeMap::new();
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
            let mut panels_answered = false;
            for item in std::mem::take(&mut state.layout_out) {
                match item {
                    LayoutOut::Answer(request, space) => {
                        if let Some(message) = layout_message(&self.layouts, &space, Some(request))
                        {
                            answered = true;
                            batch.push(message);
                            // The answer to the watch is followed at once by the space's panels.
                            if state.watch_unanswered == Some(request) {
                                state.watch_unanswered = None;
                                state.panels_stale = false;
                                panels_answered = true;
                                batch.push(DaemonMessage::Panels(panels_for(
                                    &self.panels,
                                    &mut bodies,
                                    state,
                                    &space,
                                    Some(request),
                                )));
                            }
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
            let watched = self.layouts.watched(conn);
            if panels_push_due(
                state.panels_stale,
                state.watch_unanswered,
                panels_answered,
                watched.is_some(),
            ) {
                if let Some(space) = watched {
                    batch.push(DaemonMessage::Panels(panels_for(
                        &self.panels,
                        &mut bodies,
                        state,
                        space,
                        None,
                    )));
                }
            }
            if state.watch_unanswered.is_none() {
                state.panels_stale = false;
            }
            batch.extend(state.spawn_out.drain(..));
            for item in state.panel_out.drain(..) {
                batch.push(match item {
                    PanelOut::Get {
                        request,
                        space,
                        panel,
                    } => match self.panels.get(&space, &panel) {
                        Ok((view, source)) => {
                            DaemonMessage::Panel(panel_reply(request, &view, source))
                        }
                        Err(error) => DaemonMessage::Error(ErrorMessage {
                            code: panel_error_code(&error),
                            message: error.to_string(),
                            request: Some(request),
                            attachment: None,
                        }),
                    },
                    PanelOut::Error(error) => DaemonMessage::Error(error),
                    PanelOut::Answer(message) => message,
                });
            }
            batch.extend(state.mcp_out.drain(..));
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
/// Whether a `panels` push is due: the watched space's panels changed, the connection's watch
/// has been answered, and that answer did not already carry the list. A push never precedes
/// the `layout` that answers the connection's watch.
fn panels_push_due(
    stale: bool,
    watch_unanswered: Option<RequestId>,
    answered_now: bool,
    watching: bool,
) -> bool {
    stale && watch_unanswered.is_none() && !answered_now && watching
}

/// The `panels` message for `space` with `request`, and whether it is the loading form, which
/// the connection is then owed a replacement for.
fn panels_for(
    panels: &PanelStore,
    bodies: &mut BTreeMap<SpaceId, (Panels, bool)>,
    state: &mut ConnState,
    space: &SpaceId,
    request: Option<RequestId>,
) -> Panels {
    let (body, loading) = bodies
        .entry(space.clone())
        .or_insert_with(|| panels_body(panels, space));
    state.panels_loading_sent = *loading;
    Panels {
        request,
        ..body.clone()
    }
}

fn panels_body(panels: &PanelStore, space: &SpaceId) -> (Panels, bool) {
    match panels.list(space) {
        Ok(list) => (panels_message(None, space, list), false),
        Err(PanelApiError::SpaceLoading | PanelApiError::UnknownSpace) => {
            (panels_loading(None, space), true)
        }
        Err(error) => (
            Panels {
                request: None,
                space: space.0.clone(),
                panels: Vec::new(),
                skipped: Vec::new(),
                error: Some(error.to_string()),
            },
            false,
        ),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_push_precedes_the_answer_to_a_watch() {
        let watch = Some(RequestId(4));
        // Stale and watching, but the watch is unanswered: the answer carries the list.
        assert!(!panels_push_due(true, watch, false, true));
        assert!(!panels_push_due(true, watch, true, true));
        // Once answered, a stale list is pushed...
        assert!(panels_push_due(true, None, false, true));
        // ...unless the answer itself carried it, nothing changed, or nothing is watched.
        assert!(!panels_push_due(true, None, true, true));
        assert!(!panels_push_due(false, None, false, true));
        assert!(!panels_push_due(true, None, false, false));
    }
}
