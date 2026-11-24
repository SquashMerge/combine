//! The `hello` / `welcome` handshake.
//!
//! The client's first frame is `hello { protocol_version }`; the daemon
//! answers `welcome { protocol_version, host_id }`, or, when the versions
//! differ, an `error` with code `version_mismatch` and then closes. Neither
//! side sends anything before this. The frame header, `hello`, `welcome` and
//! that error are frozen across all protocol versions.

use std::io::{Read, Write};
use std::{error, fmt};

use crate::client::{ClientMessage, Hello};
use crate::daemon::{DaemonMessage, ErrorCode, ErrorMessage, Welcome};
use crate::error::{ReadError, WriteError};
use crate::ids::HostId;
use crate::stream::{read_message, write_message};

/// The protocol version. One integer, starting at 1. Any change an older peer
/// can't safely ignore bumps it: a new message type, frame kind or enum value,
/// or a changed or removed field. A new optional field doesn't. There is no
/// negotiation: the daemon accepts exactly its own version.
pub const PROTOCOL_VERSION: u32 = 5;

/// The client and daemon versions differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionMismatch {
    pub client: u32,
    pub daemon: u32,
}

impl fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "protocol version mismatch: client {}, daemon {}",
            self.client, self.daemon
        )
    }
}

impl error::Error for VersionMismatch {}

impl From<VersionMismatch> for ErrorMessage {
    fn from(mismatch: VersionMismatch) -> Self {
        ErrorMessage {
            code: ErrorCode::VersionMismatch,
            message: mismatch.to_string(),
            request: None,
            attachment: None,
        }
    }
}

/// Why a handshake failed.
#[derive(Debug)]
pub enum HandshakeError {
    Read(ReadError),
    Write(WriteError),
    /// The peer closed the stream before its handshake message.
    Closed,
    /// The peer's first message was not the handshake message.
    UnexpectedMessage {
        type_name: &'static str,
    },
    VersionMismatch(VersionMismatch),
    /// The daemon answered the hello with an `error`.
    Refused(ErrorMessage),
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => e.fmt(f),
            Self::Write(e) => e.fmt(f),
            Self::Closed => f.write_str("connection closed during the handshake"),
            Self::UnexpectedMessage { type_name } => {
                write!(f, "expected the handshake, got {type_name}")
            }
            Self::VersionMismatch(m) => m.fmt(f),
            Self::Refused(e) => f.write_str(e.trimmed_message()),
        }
    }
}

impl error::Error for HandshakeError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            Self::Write(e) => Some(e),
            Self::VersionMismatch(e) => Some(e),
            Self::Closed | Self::UnexpectedMessage { .. } | Self::Refused(_) => None,
        }
    }
}

impl From<ReadError> for HandshakeError {
    fn from(e: ReadError) -> Self {
        Self::Read(e)
    }
}

impl From<WriteError> for HandshakeError {
    fn from(e: WriteError) -> Self {
        Self::Write(e)
    }
}

/// The daemon's answer to `hello`: `welcome` when the version is
/// [`PROTOCOL_VERSION`].
pub fn answer_hello(hello: &Hello, host_id: &HostId) -> Result<Welcome, VersionMismatch> {
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(VersionMismatch {
            client: hello.protocol_version,
            daemon: PROTOCOL_VERSION,
        });
    }
    Ok(Welcome {
        protocol_version: PROTOCOL_VERSION,
        host_id: host_id.clone(),
    })
}

/// Checks the daemon's reply to `hello`.
pub fn check_welcome(reply: DaemonMessage) -> Result<Welcome, HandshakeError> {
    match reply {
        DaemonMessage::Welcome(welcome) if welcome.protocol_version == PROTOCOL_VERSION => {
            Ok(welcome)
        }
        DaemonMessage::Welcome(welcome) => Err(HandshakeError::VersionMismatch(VersionMismatch {
            client: PROTOCOL_VERSION,
            daemon: welcome.protocol_version,
        })),
        DaemonMessage::Error(error) => Err(HandshakeError::Refused(error)),
        other => Err(HandshakeError::UnexpectedMessage {
            type_name: other.type_name(),
        }),
    }
}

/// Runs the daemon's side: reads the client's `hello`, then writes `welcome`
/// and flushes. On a version mismatch it writes the `version_mismatch` error,
/// flushes and returns [`HandshakeError::VersionMismatch`]; the caller closes.
/// A first message other than `hello` writes nothing.
pub fn daemon_handshake<R: Read + ?Sized, W: Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    host_id: &HostId,
) -> Result<Hello, HandshakeError> {
    let hello = match read_message::<ClientMessage, _>(reader)? {
        None => return Err(HandshakeError::Closed),
        Some(ClientMessage::Hello(hello)) => hello,
        Some(other) => {
            return Err(HandshakeError::UnexpectedMessage {
                type_name: other.type_name(),
            })
        }
    };
    match answer_hello(&hello, host_id) {
        Ok(welcome) => {
            write_message(writer, &DaemonMessage::Welcome(welcome))?;
            writer.flush().map_err(WriteError::from)?;
            Ok(hello)
        }
        Err(mismatch) => {
            write_message(writer, &DaemonMessage::Error(mismatch.into()))?;
            writer.flush().map_err(WriteError::from)?;
            Err(HandshakeError::VersionMismatch(mismatch))
        }
    }
}

/// Runs the client's side: writes `hello` at [`PROTOCOL_VERSION`], flushes,
/// reads the daemon's reply and checks it.
pub fn client_handshake<R: Read + ?Sized, W: Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
) -> Result<Welcome, HandshakeError> {
    let hello = ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
    });
    write_message(writer, &hello)?;
    writer.flush().map_err(WriteError::from)?;
    match read_message::<DaemonMessage, _>(reader)? {
        None => Err(HandshakeError::Closed),
        Some(reply) => check_welcome(reply),
    }
}
