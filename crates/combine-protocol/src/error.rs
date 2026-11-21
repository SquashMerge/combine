//! Codec, stream and handshake error types.

use std::{error, fmt, io};

/// Why a frame could not be decoded. Returned, never sent: a daemon that gets
/// one reports it to the client as an `error` with code `malformed`, and every
/// `Display` text is under 2 KiB so that error always fits one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The buffer or stream ends inside a frame.
    Truncated { needed: usize, available: usize },
    /// The frame length is 0, so there is no kind byte.
    EmptyFrame,
    /// The frame length is over the cap.
    FrameTooLarge { length: u32, max: u32 },
    /// A kind this side does not accept.
    UnknownKind { kind: u8 },
    /// A binary payload shorter than its fixed header.
    ShortPayload {
        kind: u8,
        needed: usize,
        actual: usize,
    },
    /// Invalid UTF-8, not an object, invalid JSON, a missing or non-string
    /// `type`, a missing or mistyped field, an unknown enum value, or trailing
    /// bytes. `detail` keeps at most [`MAX_ERROR_DETAIL`](crate::MAX_ERROR_DETAIL) bytes.
    BadJson { detail: String },
    /// A well-formed object with a `type` unknown for this direction.
    /// `type_name` keeps at most [`MAX_ERROR_DETAIL`](crate::MAX_ERROR_DETAIL) bytes.
    UnknownMessageType { type_name: String },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => {
                write!(f, "truncated frame: need {needed} bytes, have {available}")
            }
            Self::EmptyFrame => f.write_str("frame length is 0: no kind byte"),
            Self::FrameTooLarge { length, max } => {
                write!(f, "frame length {length} is over the {max}-byte cap")
            }
            Self::UnknownKind { kind } => write!(f, "unknown frame kind {kind}"),
            Self::ShortPayload {
                kind,
                needed,
                actual,
            } => write!(
                f,
                "frame kind {kind} needs a {needed}-byte header, payload has {actual}"
            ),
            Self::BadJson { detail } => write!(f, "bad JSON message: {detail}"),
            Self::UnknownMessageType { type_name } => {
                write!(f, "unknown message type {type_name:?}")
            }
        }
    }
}

impl error::Error for DecodeError {}

/// Why a message could not be encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The encoded frame would be over the cap.
    FrameTooLarge { length: usize, max: u32 },
    /// [`Data::split`](crate::Data::split) would pass the largest seq.
    SeqOverflow,
    /// serde_json failed to encode. Unreachable in practice.
    Json { detail: String },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { length, max } => {
                write!(
                    f,
                    "message needs a {length}-byte frame, over the {max}-byte cap"
                )
            }
            Self::SeqOverflow => f.write_str("data split would run past the largest seq"),
            Self::Json { detail } => write!(f, "could not encode JSON: {detail}"),
        }
    }
}

impl error::Error for EncodeError {}

/// Why reading a message failed.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    Decode(DecodeError),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "read failed: {e}"),
            Self::Decode(e) => e.fmt(f),
        }
    }
}

impl error::Error for ReadError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Decode(e) => Some(e),
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<DecodeError> for ReadError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

/// Why writing a message failed.
#[derive(Debug)]
pub enum WriteError {
    Io(io::Error),
    Encode(EncodeError),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "write failed: {e}"),
            Self::Encode(e) => e.fmt(f),
        }
    }
}

impl error::Error for WriteError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Encode(e) => Some(e),
        }
    }
}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<EncodeError> for WriteError {
    fn from(e: EncodeError) -> Self {
        Self::Encode(e)
    }
}
