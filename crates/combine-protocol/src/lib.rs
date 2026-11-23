//! The wire protocol between Combine clients and daemons: frames, messages and
//! a blocking codec over a byte stream the caller provides.
//!
//! A frame is `u32 length | u8 kind | payload`, with every integer big-endian.
//! The length counts the kind byte and the payload and is at most
//! [`MAX_FRAME_LEN`]. Control messages are JSON objects (kind 1); terminal
//! output, typed input and automatic replies are binary kinds.
//!
//! Memory per decoded control frame is bounded by a constant factor of the
//! frame cap: within the 32 MiB bound, the frame buffer for unknown values and
//! unescaped keys. An unknown key containing an escape can grow serde_json's
//! key scratch buffer to about twice that key.
//!
//! Nested known structs (`cursor`, `options`, `display`) accept JSON array
//! form, as serde does by default: `{"type":"ack","cursor":[7,4,9]}` decodes.
//! The top-level message must still be an object.
#![forbid(unsafe_code)]

mod client;
mod codec;
mod daemon;
mod display;
mod error;
mod frame;
mod handshake;
mod ids;
mod layout;
mod stream;
mod terminals;

pub use client::{
    Ack, Attach, AttachRequestOptions, AutomaticReply, ClientMessage, Detach, Focus, Hello, Input,
    MutateLayout, RequestOwnership, Resize, Resume, SetDisplay, UnwatchLayout, WatchLayout,
    WatchTerminals,
};
pub use codec::WireMessage;
pub use daemon::{
    Attached, CloseReason, Closed, DaemonMessage, Data, ErrorCode, ErrorMessage, Facts, Fidelity,
    Gap, Layout, Lost, Ownership, Resized, Resumed, Resynced, Started, Terminals, Welcome,
};
pub use display::Display;
pub use error::{DecodeError, EncodeError, ReadError, WriteError};
pub use frame::{
    MAX_AUTOMATIC_REPLY, MAX_DATA_CHUNK, MAX_ERROR_DETAIL, MAX_FRAME_LEN, MAX_INPUT_CHUNK,
};
pub use handshake::{
    answer_hello, check_welcome, client_handshake, daemon_handshake, HandshakeError,
    VersionMismatch, PROTOCOL_VERSION,
};
pub use ids::{
    AttachmentId, BackendGeneration, Cursor, Epoch, HostId, OwnershipGeneration, RequestId, Seq,
    TermId,
};
pub use layout::{DroppedNotice, LayoutNode, LayoutOp, LayoutSnapshot, Ratio, SplitDirection};
pub use stream::{read_message, write_message};
pub use terminals::{SpaceInfo, SpaceKind, TerminalInfo, TerminalState};
