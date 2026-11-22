//! The terminal backend port: one trait for the backend, one for each viewer of a terminal.
//!
//! A [`TermId`] is `<prefix>:<address>`. The prefix is fixed per backend and the address is
//! non-empty. Outside the adapter the ID is opaque: nothing here or in core parses it.

use std::fmt;

use async_trait::async_trait;
use combine_protocol::{
    AttachRequestOptions, BackendGeneration, CloseReason, Display, Fidelity, TermId,
};
use futures::stream::BoxStream;

use crate::model::{SpawnSpec, TermEvent, TermInfo};

pub type BackendResult<T> = Result<T, BackendError>;

/// An attachment's events, each paired with the backend generation that produced it.
pub type AttachEvents = BoxStream<'static, (BackendGeneration, AttachEvent)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    UnknownTerminal(TermId),
    /// Another machine's terminal; rejected in any call.
    RemoteTerminal(TermId),
    InvalidRequest(String),
    Closed,
    Failed(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::UnknownTerminal(id) => write!(f, "unknown terminal {}", id.0),
            BackendError::RemoteTerminal(id) => {
                write!(f, "{} is another machine's terminal", id.0)
            }
            BackendError::InvalidRequest(detail) => write!(f, "invalid request: {detail}"),
            BackendError::Closed => write!(f, "the attachment has closed"),
            BackendError::Failed(detail) => write!(f, "backend failure: {detail}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// What a backend can do. A `false` flag says the matching call is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// `attach` gives an ordered first paint, then live bytes.
    pub ordered_attach: bool,
    /// `credit` meters `Data` bytes; false: `credit` is a no-op.
    pub byte_credit: bool,
    /// Data bytes a new display stream may send before any `credit`.
    pub initial_credit: u32,
    /// `set_display` feeds query answers and yields `Facts`; false: a no-op.
    pub display_queries: bool,
    /// `write_automatic` reaches the program; false: a no-op.
    pub automatic_replies: bool,
    /// The backend emits `Resynced` when it drops output.
    pub resync_signal: bool,
    /// The backend reports agent status (unused until later).
    pub agent_state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachOptions {
    pub cols: u16,
    pub rows: u16,
    pub display: Display,
    /// Watch only: never types or resizes.
    pub observe: bool,
    /// Back after a dropped line. The daemon always passes `false` in M1.
    pub resumed: bool,
    /// Scrollback lines to paint first.
    pub history: u32,
}

impl From<AttachRequestOptions> for AttachOptions {
    fn from(options: AttachRequestOptions) -> Self {
        AttachOptions {
            cols: options.cols,
            rows: options.rows,
            display: options.display,
            observe: options.observe,
            resumed: false,
            history: options.history,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachEvent {
    Started {
        cols: u16,
        rows: u16,
        fidelity: Fidelity,
        ordered_resizes: bool,
    },
    Data(Vec<u8>),
    Resized {
        cols: u16,
        rows: u16,
    },
    Facts(Display),
    Resynced,
    Closed(CloseReason),
    Lost,
}

/// A terminal backend. Implementations: an adapter for each real backend, and the in-memory fake.
///
/// - `list` returns this host's terminals only, and `watch` the changes after the call. The
///   daemon watches first, then lists.
/// - A [`TermId`] is `<prefix>:<address>`. The prefix is fixed per backend (`fake` for the
///   fake) and the address is non-empty.
#[async_trait]
pub trait TerminalBackend: Send + Sync {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    async fn list(&self) -> BackendResult<Vec<TermInfo>>;
    async fn watch(&self) -> BackendResult<BoxStream<'static, TermEvent>>;
    async fn spawn(&self, spec: SpawnSpec) -> BackendResult<TermId>;
    /// Returns once the pair is ready. The stream's first item is `(g1, Started{..})`, then
    /// the first paint. A failure halfway tears down what opened and returns `Err`.
    async fn attach(&self, id: &TermId, opts: AttachOptions) -> BackendResult<Box<dyn Attachment>>;
    async fn kill(&self, id: &TermId) -> BackendResult<()>;
}

/// One viewer of one terminal. Two panels on one terminal are two attachments.
///
/// - Generations of one attachment strictly increase.
/// - Methods other than `attach` complete once the request is queued or applied locally. They
///   never wait on a remote round trip. The driver awaits them in order.
/// - After `detach()` the stream ends. It may yield `Closed(Detached)` first; the daemon
///   ignores it.
/// - `Facts(d)` follows a `set_display(d)` once applied, only with `display_queries`.
/// - On a watch-only attachment, `write`, `write_automatic`, `resize` and `focus` are `Ok`
///   no-ops.
/// - While a pair reopens, `write`, `resize`, `focus` and `set_display` are queued and applied
///   in order once it is ready. `write_automatic` and `credit` are dropped. The queue is
///   bounded by the adapter; a request beyond the bound returns `Err(BackendError::Failed(..))`
///   and is not applied.
/// - A queued owner-affecting request (`focus`, `resize`, `set_display` and the activation a
///   `write` brings) is not applied on reopen if another attachment of the terminal became the
///   active viewer after it was queued: it must not make this attachment active or change the
///   terminal's size or display. Typed bytes in such a `write` were accepted while the
///   attachment owned the terminal, so they are still written, once, without activating it.
#[async_trait]
pub trait Attachment: Send {
    /// Of the current generation.
    fn fidelity(&self) -> Fidelity;
    fn ordered_resizes(&self) -> bool;
    /// Taken once; a second call returns an empty stream.
    fn events(&mut self) -> AttachEvents;
    async fn write(&mut self, bytes: &[u8]) -> BackendResult<()>;
    async fn write_automatic(&mut self, bytes: &[u8]) -> BackendResult<()>;
    async fn resize(&mut self, cols: u16, rows: u16) -> BackendResult<()>;
    async fn focus(&mut self) -> BackendResult<()>;
    async fn set_display(&mut self, display: Display) -> BackendResult<()>;
    async fn credit(&mut self, bytes: u32) -> BackendResult<()>;
    async fn detach(self: Box<Self>) -> BackendResult<()>;
}
