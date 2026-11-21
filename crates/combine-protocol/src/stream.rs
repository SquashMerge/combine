//! Reading and writing frames over a caller-provided byte stream.

use std::io::{self, Read, Write};

use crate::codec::WireMessage;
use crate::error::{DecodeError, ReadError, WriteError};
use crate::frame::check_length;

/// Fills `buf` from `reader`, retrying on `Interrupted`, and returns how many
/// bytes arrived before the end of the stream.
fn fill<R: Read + ?Sized>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    while read < buf.len() {
        match reader.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read)
}

/// Reads one message. `Ok(None)` is a clean end of stream before any frame
/// byte.
///
/// The length is checked before the payload buffer is allocated. After a
/// message-level error (`UnknownKind`, `ShortPayload`, `BadJson`,
/// `UnknownMessageType`) the stream is at the next frame boundary; after a
/// frame-level error (`Truncated`, `EmptyFrame`, `FrameTooLarge`) it is
/// unusable. Either way, callers close the connection.
pub fn read_message<M: WireMessage, R: Read + ?Sized>(
    reader: &mut R,
) -> Result<Option<M>, ReadError> {
    let mut header = [0; 4];
    let got = fill(reader, &mut header)?;
    if got == 0 {
        return Ok(None);
    }
    if got < header.len() {
        return Err(DecodeError::Truncated {
            needed: header.len(),
            available: got,
        }
        .into());
    }
    let length = check_length(u32::from_be_bytes(header))?;
    let mut body = vec![0; length];
    let got = fill(reader, &mut body)?;
    if got < length {
        return Err(DecodeError::Truncated {
            needed: 4 + length,
            available: 4 + got,
        }
        .into());
    }
    let message = M::from_frame(body[0], &body[1..])?;
    Ok(Some(message))
}

/// Encodes `message` and writes it with one `write_all`. It does not flush.
pub fn write_message<M: WireMessage, W: Write + ?Sized>(
    writer: &mut W,
    message: &M,
) -> Result<(), WriteError> {
    writer.write_all(&message.encode()?)?;
    Ok(())
}
