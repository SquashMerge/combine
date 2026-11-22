//! Client-to-daemon messages.

use serde::{Deserialize, Serialize};

use crate::display::Display;
use crate::ids::{AttachmentId, Cursor, Epoch, OwnershipGeneration, RequestId, TermId};

/// The client's first frame. See the handshake in [`crate::client_handshake`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
}

/// The attach options on the wire.
///
/// These are the backend trait's `AttachOptions` without `resumed`, which the
/// separate [`Resume`] request replaces. They have their own name because the
/// daemon, which converts one into the other, would otherwise hold two types
/// called `AttachOptions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachRequestOptions {
    pub cols: u16,
    pub rows: u16,
    pub display: Display,
    pub observe: bool,
    pub history: u32,
}

/// A fresh attach. The daemon answers `attached` (before the attachment's first
/// `started`) or an `error` naming this request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attach {
    pub request: RequestId,
    pub terminal: TermId,
    pub options: AttachRequestOptions,
}

/// Resumes an attachment from the last *applied* event. The daemon answers
/// `resumed` before the first replayed event, or `gap` carrying this request
/// when it cannot serve the resume exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    pub request: RequestId,
    pub cursor: Cursor,
}

/// Detaches. The daemon answers with the attachment's final `closed` event
/// with reason `detached`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detach {
    pub attachment: AttachmentId,
}

/// Acknowledges the last applied event. This is the client's only
/// flow-control signal; no credit message crosses the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub cursor: Cursor,
}

/// Resizes the terminal. A stale `ownership_generation` gets an `error` with
/// code `stale_generation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resize {
    pub attachment: AttachmentId,
    pub cols: u16,
    pub rows: u16,
    pub ownership_generation: OwnershipGeneration,
}

/// Reports that the attachment's panel has focus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Focus {
    pub attachment: AttachmentId,
}

/// Replaces the attachment's display facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetDisplay {
    pub attachment: AttachmentId,
    pub display: Display,
}

/// Asks for input ownership without typing. Typed input from a non-owner is
/// itself the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestOwnership {
    pub attachment: AttachmentId,
}

/// Subscribes the connection to the host's terminals and spaces. The daemon
/// answers `terminals` carrying this `request`, then pushes a fresh snapshot
/// after each change. Asking again re-answers; there is no unwatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchTerminals {
    pub request: RequestId,
}

/// Typed input: frame kind 3, never JSON. A caller with more than
/// [`MAX_INPUT_CHUNK`](crate::MAX_INPUT_CHUNK) bytes splits it, in order, with
/// `chunks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub attachment: AttachmentId,
    pub bytes: Vec<u8>,
}

/// An emulator's reply to a terminal query: frame kind 4, never JSON. The
/// daemon drops one whose epoch or ownership generation is stale, with no
/// error. Over [`MAX_AUTOMATIC_REPLY`](crate::MAX_AUTOMATIC_REPLY) bytes is an
/// encode error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticReply {
    pub attachment: AttachmentId,
    pub epoch: Epoch,
    pub ownership_generation: OwnershipGeneration,
    pub bytes: Vec<u8>,
}

/// Every message a client sends. `Input` and `AutomaticReply` are binary
/// frames; the rest are JSON control frames tagged by `type`.
///
/// There is no `Deserialize`: [`WireMessage::decode`](crate::WireMessage::decode)
/// decodes each struct directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello(Hello),
    Attach(Attach),
    Resume(Resume),
    Detach(Detach),
    Ack(Ack),
    Resize(Resize),
    Focus(Focus),
    SetDisplay(SetDisplay),
    RequestOwnership(RequestOwnership),
    WatchTerminals(WatchTerminals),
    #[serde(skip)]
    Input(Input),
    #[serde(skip)]
    AutomaticReply(AutomaticReply),
}

impl ClientMessage {
    /// The JSON `type` tag, or `"input"` / `"automatic_reply"` for the binary
    /// messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Attach(_) => "attach",
            Self::Resume(_) => "resume",
            Self::Detach(_) => "detach",
            Self::Ack(_) => "ack",
            Self::Resize(_) => "resize",
            Self::Focus(_) => "focus",
            Self::SetDisplay(_) => "set_display",
            Self::RequestOwnership(_) => "request_ownership",
            Self::WatchTerminals(_) => "watch_terminals",
            Self::Input(_) => "input",
            Self::AutomaticReply(_) => "automatic_reply",
        }
    }
}
