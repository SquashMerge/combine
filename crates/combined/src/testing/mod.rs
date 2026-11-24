#![cfg(feature = "test-support")]
//! The in-memory fakes, for tests: the TerminalBackend, the file store and the diagram check.
//!
//! [`FakeBackend::new`] gives the backend and a [`FakeControl`] handle. The control handle
//! drives the program in each terminal, injects faults and observes what the backend was asked
//! to do. The fake runs no threads and reads no clock: nothing happens until a test calls into
//! it, or polls an event stream.

mod control;
mod diagram_check;
mod fake_attachment;
mod fake_backend;
mod file_store;
pub(crate) mod world;

use combine_protocol::{Display, TermId};

use crate::ports::{AttachOptions, Capabilities};

pub use control::FakeControl;
pub use diagram_check::{FakeAnswer, FakeDiagramCheck, FakeHold};
pub use fake_backend::FakeBackend;
pub use file_store::MemoryFileStore;

/// Opens every paint.
pub const FAKE_PAINT_START: &[u8] = b"\x18\x1b[!p";
/// What a late event from an old generation carries, after `Started` for the new one.
pub const LATE_DATA: &[u8] = b"<late>";
/// The size a late `Resized` from an old generation names. No contract case asks for 77x21, so
/// a client that applies it shows.
pub const LATE_RESIZED: (u16, u16) = (77, 21);
/// Closes every paint.
pub const FAKE_PAINT_END: &[u8] = b"\x1b[?9001l";

/// How the fake tells a viewer it dropped output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncMode {
    /// `Resynced`, then a paint.
    Signal,
    /// A paint alone, in band.
    Silent,
}

/// A deliberate fault in the event stream, applied once per attachment after its first
/// `Started`. A mutation exists so a suite can prove it notices the fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// The first `Resized` is never sent.
    DropResized,
    /// The first `Resized` is sent before the latest `Data` still waiting.
    MoveResized,
    /// The first program-output `Data` item of at least 1,024 bytes loses its middle byte.
    DropByte,
}

/// One of an attachment's two backend halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Display,
    Input,
}

/// Names one attachment the fake opened. Refs count up from 1 across the whole backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FakeAttachmentRef(pub u64);

/// What the fake does. `Default` is the faithful backend.
#[derive(Debug, Clone)]
pub struct FakeConfig {
    /// All true except `agent_state`. `resync_signal` follows `resync`, not this field.
    pub capabilities: Capabilities,
    /// The largest `Data` event the fake emits.
    pub chunk_size: usize,
    /// Undelivered `Data` bytes per attachment before the fake drops them and repaints.
    pub backend_queue_limit: usize,
    pub resync: ResyncMode,
    /// The PTY size when no attachment is active.
    pub headless_size: (u16, u16),
    pub paint_after_resize: bool,
    /// False: the fake never emits `Resized`.
    pub ordered_resizes: bool,
    pub late_old_generation_events: bool,
    /// Failed opens of a reopening pair before the fake gives up with `Lost`.
    pub max_reopen_attempts: u32,
    /// Requests held while a pair reopens.
    pub reopen_queue_limit: usize,
    /// Bytes of typed input held while a pair reopens.
    pub reopen_input_limit: usize,
    pub mutation: Option<Mutation>,
}

impl Default for FakeConfig {
    fn default() -> Self {
        FakeConfig {
            capabilities: Capabilities {
                ordered_attach: true,
                byte_credit: true,
                initial_credit: 262_144,
                display_queries: true,
                automatic_replies: true,
                resync_signal: true,
                agent_state: false,
            },
            chunk_size: 16_384,
            backend_queue_limit: 4_194_304,
            resync: ResyncMode::Signal,
            headless_size: (200, 120),
            paint_after_resize: true,
            ordered_resizes: true,
            late_old_generation_events: false,
            max_reopen_attempts: 3,
            reopen_queue_limit: 1_024,
            reopen_input_limit: 65_536,
            mutation: None,
        }
    }
}

/// A call the fake received, in the order it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    Spawn {
        term: TermId,
    },
    Kill {
        term: TermId,
    },
    Attach {
        att: FakeAttachmentRef,
        term: TermId,
        options: AttachOptions,
    },
    Write {
        att: FakeAttachmentRef,
        bytes: Vec<u8>,
    },
    WriteAutomatic {
        att: FakeAttachmentRef,
        bytes: Vec<u8>,
    },
    Resize {
        att: FakeAttachmentRef,
        cols: u16,
        rows: u16,
    },
    Focus {
        att: FakeAttachmentRef,
    },
    SetDisplay {
        att: FakeAttachmentRef,
        display: Display,
    },
    Credit {
        att: FakeAttachmentRef,
        bytes: u32,
    },
    Detach {
        att: FakeAttachmentRef,
    },
}
