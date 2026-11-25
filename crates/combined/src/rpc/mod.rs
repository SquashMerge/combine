//! The daemon's client-facing server: the socket, its connections and the flush loop.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use combine_protocol::HostId;
use tokio::sync::{mpsc, oneshot};

use crate::core::{Driver, LayoutTiming, PanelTiming, TerminalList};
use crate::mcp_server::McpAudit;
use crate::model::Millis;
use crate::ports::{CallerIdentity, DiagramCheck, FileStore, ProjectDirs, ScratchStore, WallClock};
use crate::wiring::clock::MonotonicClock;
use crate::wiring::ids::IdReservation;

pub mod connect;
mod connection;
pub mod flush;
mod mcp_channel;
mod panel_calls;
mod server;
pub mod socket;

pub use panel_calls::{PanelCall, PanelHandle};
pub use server::{serve, serve_watched, watch_backend, ServeError, Watched};
pub use socket::BoundSocket;

/// Runs `work` until it finishes or `shutdown` completes, whichever comes first: `None` means
/// shutdown won. `shutdown` is borrowed, so one pinned future can guard each start-up step and
/// then the server.
pub async fn until_shutdown<S, W>(shutdown: Pin<&mut S>, work: W) -> Option<W::Output>
where
    S: Future<Output = ()>,
    W: Future,
{
    tokio::select! {
        biased;
        () = shutdown => None,
        output = work => Some(output),
    }
}

/// Timing and size limits of the server.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// The least time between two flush passes.
    pub flush_interval: Duration,
    /// The most stream bytes one flush pass takes for one connection.
    pub max_pull_bytes: usize,
    /// How often core's timers are checked.
    pub housekeeping: Duration,
    /// How often a space the file store could not watch is tried again.
    pub watch_retry: Duration,
    /// How long a new connection may take to send `hello`.
    pub handshake_timeout: Duration,
    /// How long a write may make no progress before its connection is closed.
    pub write_timeout: Duration,
    pub max_connections: usize,
    /// MCP channels open at once, besides `max_connections`.
    pub max_mcp_connections: usize,
    /// How long a create or update waits for a verdict.
    pub mcp_wait: Duration,
    /// How often a waiting tool asks for the panel's state, and checks its closed flag.
    pub mcp_poll: Duration,
    /// The most `watch_terminals` answers one connection may leave unflushed. It also bounds the
    /// requests an MCP channel may queue and the replies it may leave unflushed.
    pub watch_answer_limit: usize,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            flush_interval: Duration::from_millis(16),
            max_pull_bytes: 1 << 20,
            housekeeping: Duration::from_millis(250),
            watch_retry: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
            max_connections: 64,
            max_mcp_connections: 16,
            mcp_wait: Duration::from_secs(5),
            mcp_poll: Duration::from_millis(50),
            watch_answer_limit: 16,
        }
    }
}

/// What the server runs on.
pub struct ServerParts {
    pub driver: Driver,
    pub terminals: TerminalList,
    pub host_id: HostId,
    pub ids: IdReservation,
    pub clock: MonotonicClock,
    /// Where spaces' `layout.json` files are read and written.
    pub file_store: Arc<dyn FileStore>,
    pub layout_timing: LayoutTiming,
    /// Checks the folders a spawn would start in.
    pub project_dirs: Box<dyn ProjectDirs>,
    /// Validates the Mermaid source of panels.
    pub diagram_check: Arc<dyn DiagramCheck>,
    /// The time of day, for the scratch panels' stamps.
    pub wall_clock: Arc<dyn WallClock>,
    /// Where scratch panels live.
    pub scratch: Arc<dyn ScratchStore>,
    pub panel_timing: PanelTiming,
    /// Calls to the panel API from embedders. `spawn` fills it; the MCP channels use the
    /// server's own.
    pub panel_calls: Option<mpsc::Receiver<PanelCall>>,
    /// Vouches for the terminal an MCP caller names. `None`: no terminal is accepted, callers
    /// resolve by cwd.
    pub caller_identity: Option<Arc<dyn CallerIdentity>>,
    /// Where each mutating MCP call's audit line goes.
    pub mcp_audit: Arc<dyn McpAudit>,
}

/// Counters the server keeps for whoever started it.
#[derive(Debug)]
pub struct ServerStats {
    connections: AtomicUsize,
    mcp: AtomicUsize,
    flushes: AtomicU64,
    min_flush_gap: AtomicU64,
}

impl ServerStats {
    pub fn new() -> ServerStats {
        ServerStats {
            connections: AtomicUsize::new(0),
            mcp: AtomicUsize::new(0),
            flushes: AtomicU64::new(0),
            min_flush_gap: AtomicU64::new(u64::MAX),
        }
    }

    /// Connections admitted and not yet removed.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// MCP channels whose tool thread hasn't ended.
    pub fn mcp_channels(&self) -> usize {
        self.mcp.load(Ordering::SeqCst)
    }

    /// Flush passes so far.
    pub fn flushes(&self) -> u64 {
        self.flushes.load(Ordering::SeqCst)
    }

    /// The smallest gap between two consecutive flush passes, in the daemon's own milliseconds.
    /// `None` before the second pass.
    pub fn min_flush_gap(&self) -> Option<u64> {
        match self.min_flush_gap.load(Ordering::SeqCst) {
            u64::MAX => None,
            gap => Some(gap),
        }
    }

    fn admitted(&self) {
        self.connections.fetch_add(1, Ordering::SeqCst);
    }

    fn removed(&self) {
        self.connections.fetch_sub(1, Ordering::SeqCst);
    }

    fn mcp_started(&self) {
        self.mcp.fetch_add(1, Ordering::SeqCst);
    }

    fn mcp_ended(&self) {
        self.mcp.fetch_sub(1, Ordering::SeqCst);
    }

    fn flush_pass(&self, gap: Option<Millis>) {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        if let Some(gap) = gap {
            self.min_flush_gap.fetch_min(gap.0, Ordering::SeqCst);
        }
    }
}

impl Default for ServerStats {
    fn default() -> Self {
        ServerStats::new()
    }
}

/// A server running on its own thread and runtime, for tests and embedding.
pub struct ServerHandle {
    stats: Arc<ServerStats>,
    panels: PanelHandle,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), ServeError>>>,
}

impl ServerHandle {
    /// A handle for calling the panel API of this server.
    pub fn panels(&self) -> PanelHandle {
        self.panels.clone()
    }

    pub fn stats(&self) -> Arc<ServerStats> {
        Arc::clone(&self.stats)
    }

    /// Stops the server, waits for its thread, and returns the error it stopped with, if any.
    /// Calling it again does nothing.
    pub fn shutdown(&mut self) -> Option<ServeError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let thread = self.thread.take()?;
        match thread.join() {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some(ServeError::State("the server thread panicked".to_string())),
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Runs the server on a new thread with its own runtime. `make_parts` runs on that thread.
pub fn spawn(
    socket: BoundSocket,
    make_parts: impl FnOnce() -> ServerParts + Send + 'static,
    options: ServeOptions,
) -> io::Result<ServerHandle> {
    let stats = Arc::new(ServerStats::new());
    let (stop, stopped) = oneshot::channel::<()>();
    let serving = Arc::clone(&stats);
    let (calls, panel_calls) = mpsc::channel(64);
    let thread = thread::Builder::new()
        .name("combined-rpc".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(ServeError::Io)?;
            let _entered = runtime.enter();
            let mut parts = make_parts();
            parts.panel_calls = Some(panel_calls);
            runtime.block_on(serve(
                socket,
                parts,
                options,
                async {
                    let _ = stopped.await;
                },
                serving,
            ))
        })?;
    Ok(ServerHandle {
        stats,
        panels: PanelHandle::new(calls),
        stop: Some(stop),
        thread: Some(thread),
    })
}
