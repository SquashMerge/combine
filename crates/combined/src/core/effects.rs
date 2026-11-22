//! What core asks its caller to do.

use combine_protocol::{AttachmentId, BackendGeneration, Display, TermId};

use crate::ports::AttachOptions;

/// Names one client connection inside core. It never goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCommand {
    Attach {
        attachment: AttachmentId,
        terminal: TermId,
        options: AttachOptions,
    },
    Write {
        attachment: AttachmentId,
        bytes: Vec<u8>,
    },
    /// `generation` is the backend generation of the `Started` that opened the reply's epoch.
    /// The adapter drops the reply if the pair has been rebuilt since.
    WriteAutomatic {
        attachment: AttachmentId,
        generation: BackendGeneration,
        bytes: Vec<u8>,
    },
    Resize {
        attachment: AttachmentId,
        cols: u16,
        rows: u16,
    },
    Focus {
        attachment: AttachmentId,
    },
    SetDisplay {
        attachment: AttachmentId,
        display: Display,
    },
    Credit {
        attachment: AttachmentId,
        bytes: u32,
    },
    Detach {
        attachment: AttachmentId,
    },
}

impl BackendCommand {
    /// The attachment the command is for.
    pub fn attachment(&self) -> AttachmentId {
        match self {
            BackendCommand::Attach { attachment, .. }
            | BackendCommand::Write { attachment, .. }
            | BackendCommand::WriteAutomatic { attachment, .. }
            | BackendCommand::Resize { attachment, .. }
            | BackendCommand::Focus { attachment }
            | BackendCommand::SetDisplay { attachment, .. }
            | BackendCommand::Credit { attachment, .. }
            | BackendCommand::Detach { attachment } => *attachment,
        }
    }
}

/// A client-driven attachment command, named when the backend refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedCommand {
    Input,
    Resize,
    Focus,
    SetDisplay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Backend(BackendCommand),
    /// Close the connection. Core has already dropped it and unbound its attachments, so
    /// the caller only closes the transport.
    Disconnect(ConnectionId),
}
