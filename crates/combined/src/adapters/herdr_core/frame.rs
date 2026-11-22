//! herdr-core's framed protocol: `u32 LE channel | u8 kind | u32 LE length | payload`.
//!
//! Pure encoding and an incremental decoder. Channel 0 carries JSON-RPC as kind 0; every
//! other channel belongs to one attachment or to the event subscription.

/// Bytes before a frame's payload.
pub(super) const HEADER_LEN: usize = 9;

/// The largest payload the decoder accepts. herdr-core's own frames stay far below it; a
/// length beyond it means the stream is out of step.
pub(super) const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;

/// The most input bytes one DATA frame carries to the core.
pub(super) const DATA_FRAME_MAX: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Data = 0,
    Credit = 1,
    Close = 2,
    Resize = 3,
    Focus = 4,
    Facts = 5,
    AutomaticInput = 6,
}

impl Kind {
    fn from_byte(byte: u8) -> Option<Kind> {
        Some(match byte {
            0 => Kind::Data,
            1 => Kind::Credit,
            2 => Kind::Close,
            3 => Kind::Resize,
            4 => Kind::Focus,
            5 => Kind::Facts,
            6 => Kind::AutomaticInput,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Frame {
    pub channel: u32,
    pub kind: Kind,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FrameError {
    /// A frame announced a payload longer than [`PAYLOAD_LIMIT`].
    Oversized(usize),
}

pub(super) fn encode(channel: u32, kind: Kind, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
    bytes.extend_from_slice(&channel.to_le_bytes());
    bytes.push(kind as u8);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// A JSON-RPC message on channel 0.
pub(super) fn rpc(message: &[u8]) -> Vec<u8> {
    encode(0, Kind::Data, message)
}

/// Grants the core `bytes` more DATA payload bytes on `channel`.
pub(super) fn credit(channel: u32, bytes: u32) -> Vec<u8> {
    encode(channel, Kind::Credit, &bytes.to_le_bytes())
}

pub(super) fn resize(channel: u32, cols: u16, rows: u16) -> Vec<u8> {
    let mut payload = [0u8; 4];
    payload[..2].copy_from_slice(&cols.to_le_bytes());
    payload[2..].copy_from_slice(&rows.to_le_bytes());
    encode(channel, Kind::Resize, &payload)
}

pub(super) fn focus(channel: u32, focused: bool) -> Vec<u8> {
    encode(channel, Kind::Focus, &[u8::from(focused)])
}

pub(super) fn facts(channel: u32, display_json: &[u8]) -> Vec<u8> {
    encode(channel, Kind::Facts, display_json)
}

pub(super) fn close(channel: u32) -> Vec<u8> {
    encode(channel, Kind::Close, &[])
}

/// Typed input, as one DATA frame per 16 KiB.
pub(super) fn data_frames(channel: u32, bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .chunks(DATA_FRAME_MAX)
        .map(|chunk| encode(channel, Kind::Data, chunk))
        .collect()
}

/// Automatic replies are small, but split the same way so a frame never exceeds the limit.
pub(super) fn automatic_frames(channel: u32, bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .chunks(DATA_FRAME_MAX)
        .map(|chunk| encode(channel, Kind::AutomaticInput, chunk))
        .collect()
}

/// The size a RESIZE frame carries.
pub(super) fn parse_resize(payload: &[u8]) -> Option<(u16, u16)> {
    match payload {
        [c0, c1, r0, r1] => Some((
            u16::from_le_bytes([*c0, *c1]),
            u16::from_le_bytes([*r0, *r1]),
        )),
        _ => None,
    }
}

/// Reassembles frames from whatever sizes the socket delivers. A frame of a kind this client
/// doesn't know is skipped, so a newer core can add kinds.
#[derive(Debug, Default)]
pub(super) struct Decoder {
    buffer: Vec<u8>,
    start: usize,
}

impl Decoder {
    pub(super) fn new() -> Self {
        Decoder::default()
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        } else if self.start > 1 << 20 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole frame, `None` while more bytes are needed.
    pub(super) fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        loop {
            let pending = &self.buffer[self.start..];
            if pending.len() < HEADER_LEN {
                return Ok(None);
            }
            let channel = u32::from_le_bytes([pending[0], pending[1], pending[2], pending[3]]);
            let length = u32::from_le_bytes([pending[5], pending[6], pending[7], pending[8]]);
            let length = length as usize;
            if length > PAYLOAD_LIMIT {
                return Err(FrameError::Oversized(length));
            }
            if pending.len() < HEADER_LEN + length {
                return Ok(None);
            }
            let kind = Kind::from_byte(pending[4]);
            let payload = pending[HEADER_LEN..HEADER_LEN + length].to_vec();
            self.start += HEADER_LEN + length;
            if let Some(kind) = kind {
                return Ok(Some(Frame {
                    channel,
                    kind,
                    payload,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn encodes_resize_as_m0_saw_it() {
        assert_eq!(hex(&resize(1, 100, 30)), "01000000030400000064001e00");
    }

    #[test]
    fn encodes_credit_in_bytes() {
        assert_eq!(hex(&credit(1, 1069)), "0100000001040000002d040000");
    }

    #[test]
    fn encodes_focus_in() {
        assert_eq!(hex(&focus(1, true)), "01000000040100000001");
        assert_eq!(hex(&focus(1, false)), "01000000040100000000");
    }

    #[test]
    fn encodes_automatic_input_as_kind_6() {
        let frames = automatic_frames(1, b"\x1b[?3;1R");
        assert_eq!(frames.len(), 1);
        assert_eq!(hex(&frames[0]), "0100000006070000001b5b3f333b3152");
    }

    #[test]
    fn decodes_frames_split_across_reads() {
        let mut wire = encode(1, Kind::Data, b"hello");
        wire.extend(resize(1, 80, 24));
        wire.extend(encode(7, Kind::Close, b"{}"));
        let mut decoder = Decoder::new();
        let mut frames = Vec::new();
        for byte in wire {
            decoder.push(&[byte]);
            while let Some(frame) = decoder.next_frame().expect("a valid frame") {
                frames.push(frame);
            }
        }
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].payload, b"hello");
        assert_eq!(parse_resize(&frames[1].payload), Some((80, 24)));
        assert_eq!((frames[2].channel, frames[2].kind), (7, Kind::Close));
    }

    #[test]
    fn skips_a_frame_of_an_unknown_kind() {
        let mut wire = encode(1, Kind::Data, b"a");
        wire[4] = 99;
        wire.extend(encode(1, Kind::Data, b"b"));
        let mut decoder = Decoder::new();
        decoder.push(&wire);
        let frame = decoder.next_frame().expect("valid").expect("a frame");
        assert_eq!(frame.payload, b"b");
    }

    #[test]
    fn rejects_an_oversized_frame() {
        let mut header = Vec::new();
        header.extend_from_slice(&1u32.to_le_bytes());
        header.push(0);
        header.extend_from_slice(&((PAYLOAD_LIMIT as u32) + 1).to_le_bytes());
        let mut decoder = Decoder::new();
        decoder.push(&header);
        assert_eq!(
            decoder.next_frame(),
            Err(FrameError::Oversized(PAYLOAD_LIMIT + 1))
        );
    }

    #[test]
    fn splits_input_into_16_kib_data_frames() {
        let input = vec![b'x'; 40_000];
        let frames = data_frames(1, &input);
        let lengths: Vec<usize> = frames
            .iter()
            .map(|frame| frame.len() - HEADER_LEN)
            .collect();
        assert_eq!(lengths, vec![16_384, 16_384, 7_232]);
    }
}
