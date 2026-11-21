//! Frame layout: `u32 length | u8 kind | payload`, big-endian.

use crate::error::DecodeError;

/// The most bytes a frame's length may count: the kind byte plus the payload.
/// SPEC's "1 MB" is recorded as 1 MiB.
pub const MAX_FRAME_LEN: u32 = 1 << 20;
/// The most data bytes in one data frame (the cap less the kind byte and the
/// 24-byte header).
pub const MAX_DATA_CHUNK: usize = 1_048_551;
/// The most bytes in one typed-input frame (the cap less the kind byte and the
/// 8-byte header).
pub const MAX_INPUT_CHUNK: usize = 1_048_567;
/// The most bytes in one automatic-reply frame.
pub const MAX_AUTOMATIC_REPLY: usize = 1_048_551;
/// The most bytes of a `BadJson` detail or an `UnknownMessageType` name that a
/// `DecodeError` keeps.
pub const MAX_ERROR_DETAIL: usize = 256;

pub(crate) const KIND_CONTROL: u8 = 1;
pub(crate) const KIND_DATA: u8 = 2;
pub(crate) const KIND_INPUT: u8 = 3;
pub(crate) const KIND_AUTOMATIC_REPLY: u8 = 4;

const PREFIX: usize = 4;

/// Checks a frame length before anything is allocated for it.
pub(crate) fn check_length(length: u32) -> Result<usize, DecodeError> {
    if length == 0 {
        return Err(DecodeError::EmptyFrame);
    }
    if length > MAX_FRAME_LEN {
        return Err(DecodeError::FrameTooLarge {
            length,
            max: MAX_FRAME_LEN,
        });
    }
    Ok(length as usize)
}

/// Splits the first frame off `buf` into its kind, payload and the bytes it
/// consumed.
pub(crate) fn split_frame(buf: &[u8]) -> Result<(u8, &[u8], usize), DecodeError> {
    let Some(header) = buf.first_chunk::<PREFIX>() else {
        return Err(DecodeError::Truncated {
            needed: PREFIX,
            available: buf.len(),
        });
    };
    let length = check_length(u32::from_be_bytes(*header))?;
    let consumed = PREFIX + length;
    if buf.len() < consumed {
        return Err(DecodeError::Truncated {
            needed: consumed,
            available: buf.len(),
        });
    }
    Ok((buf[PREFIX], &buf[PREFIX + 1..consumed], consumed))
}
