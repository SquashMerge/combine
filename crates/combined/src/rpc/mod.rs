//! The daemon's client-facing server: the socket, its connections and the flush loop.

use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use combine_protocol::HostId;
use tokio::sync::oneshot;

use crate::core::{Driver, TerminalList};
use crate::model::Millis;
use crate::ports::FileStore;
use crate::wiring::clock::MonotonicClock;
use crate::wiring::ids::IdReservation;

pub mod connect;
mod connection;
pub mod flush;
mod server;
pub mod socket;

pub use server::{serve, ServeError};
pub use socket::BoundSocket;

/// Timing and size limits of the server.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// The least time between two flush passes.
    pub flush_interval: Duration,
    /// The most stream bytes one flush pass takes for one connection.
    pub max_pull_bytes: usize,
    /// How often core's timers are checked.
    pub housekeeping: Duration,
    /// How long a new connection may take to send `hello`.
    pub handshake_timeout: Duration,
    /// How long a write may make no progress before its connection is closed.
    pub write_timeout: Duration,
    pub max_connections: usize,
    /// The most `watch_terminals` answers one connection may leave unflushed.
    pub watch_answer_limit: usize,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            flush_interval: Duration::from_millis(16),
            max_pull_bytes: 1 << 20,
            housekeeping: Duration::from_millis(250),
            handshake_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
            max_connections: 64,
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
    /// Held for the layouts slice: core has no consumer for it yet.
    pub file_store: Arc<dyn FileStore>,
}

/// Counters the server keeps for whoever started it.
#[derive(Debug)]
pub struct ServerStats {
    connections: AtomicUsize,
    flushes: AtomicU64,
    min_flush_gap: AtomicU64,
}

impl ServerStats {
    pub fn new() -> ServerStats {
        ServerStats {
            connections: AtomicUsize::new(0),
            flushes: AtomicU64::new(0),
            min_flush_gap: AtomicU64::new(u64::MAX),
        }
    }

    /// Connections admitted and not yet removed.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
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
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), ServeError>>>,
}

impl ServerHandle {
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
    let thread = thread::Builder::new()
        .name("combined-rpc".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(ServeError::Io)?;
            let _entered = runtime.enter();
            let parts = make_parts();
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
        stop: Some(stop),
        thread: Some(thread),
    })
}
