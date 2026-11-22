//! Encoding and decoding of whole frames.

use std::fmt::{self, Write as _};

use serde::Deserialize;

use crate::client::{
    Ack, Attach, AutomaticReply, ClientMessage, Detach, Focus, Hello, Input, MutateLayout,
    RequestOwnership, Resize, Resume, SetDisplay, UnwatchLayout, WatchLayout, WatchTerminals,
};
use crate::daemon::{
    Attached, Closed, DaemonMessage, Data, ErrorMessage, Facts, Gap, Layout, Lost, Ownership,
    Resized, Resumed, Resynced, Started, Terminals, Welcome,
};
use crate::error::{DecodeError, EncodeError};
use crate::frame::{
    split_frame, KIND_AUTOMATIC_REPLY, KIND_CONTROL, KIND_DATA, KIND_INPUT, MAX_ERROR_DETAIL,
    MAX_FRAME_LEN,
};
use crate::ids::{AttachmentId, Cursor, Epoch, OwnershipGeneration, Seq};

pub(crate) mod sealed {
    pub trait Sealed: Sized {
        fn from_frame(kind: u8, payload: &[u8]) -> Result<Self, crate::DecodeError>;
        fn to_frame(&self) -> Result<(u8, Vec<u8>), crate::EncodeError>;
    }
}

/// A message that travels in frames: [`ClientMessage`] or [`DaemonMessage`].
/// The trait is sealed.
pub trait WireMessage: Sized + sealed::Sealed {
    /// Encodes one whole frame, length prefix included. It never splits; see
    /// [`Data::split`](crate::Data::split).
    fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let (kind, payload) = self.to_frame()?;
        let length = 1 + payload.len();
        if length > MAX_FRAME_LEN as usize {
            return Err(EncodeError::FrameTooLarge {
                length,
                max: MAX_FRAME_LEN,
            });
        }
        let mut frame = Vec::with_capacity(4 + length);
        frame.extend_from_slice(&(length as u32).to_be_bytes());
        frame.push(kind);
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    /// Decodes the first frame in `buf` and returns it with the bytes it
    /// consumed. [`DecodeError::Truncated`]'s `needed` is never over
    /// `4 + MAX_FRAME_LEN`, so a caller can buffer and retry.
    fn decode(buf: &[u8]) -> Result<(Self, usize), DecodeError> {
        let (kind, payload, consumed) = split_frame(buf)?;
        Ok((Self::from_frame(kind, payload)?, consumed))
    }
}

impl WireMessage for ClientMessage {}
impl WireMessage for DaemonMessage {}

/// Holds the first [`MAX_ERROR_DETAIL`] bytes written to it, cut back to a
/// char boundary, so serde_json's error text (which can be several MiB) is
/// never copied whole.
struct CappedText {
    text: String,
    full: bool,
}

impl CappedText {
    fn new() -> Self {
        Self {
            text: String::with_capacity(MAX_ERROR_DETAIL),
            full: false,
        }
    }
}

impl fmt::Write for CappedText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.full {
            return Ok(());
        }
        let room = MAX_ERROR_DETAIL - self.text.len();
        if s.len() <= room {
            self.text.push_str(s);
        } else {
            self.text.push_str(&s[..floor_char_boundary(s, room)]);
            self.full = true;
        }
        Ok(())
    }
}

pub(crate) fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    while !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn bad_json(error: &dyn fmt::Display) -> DecodeError {
    let mut detail = CappedText::new();
    let _ = write!(detail, "{error}");
    DecodeError::BadJson {
        detail: detail.text,
    }
}

fn not_an_object() -> DecodeError {
    DecodeError::BadJson {
        detail: "control payload is not a JSON object".to_owned(),
    }
}

fn unknown_message_type(type_name: &str) -> DecodeError {
    let end = floor_char_boundary(type_name, type_name.len().min(MAX_ERROR_DETAIL));
    DecodeError::UnknownMessageType {
        type_name: type_name[..end].to_owned(),
    }
}

#[derive(Deserialize)]
struct TypeTag {
    #[serde(rename = "type")]
    type_name: String,
}

/// Checks that a control payload is UTF-8 and starts a JSON object, and
/// returns its tag. Never deserializes into a message enum: serde would buffer
/// the whole object, unknown fields included.
fn control_tag(payload: &[u8]) -> Result<(&str, String), DecodeError> {
    let text = std::str::from_utf8(payload).map_err(|e| bad_json(&e))?;
    let first = text
        .bytes()
        .find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
    if first != Some(b'{') {
        return Err(not_an_object());
    }
    let tag: TypeTag = serde_json::from_str(text).map_err(|e| bad_json(&e))?;
    Ok((text, tag.type_name))
}

fn parse<'a, T: Deserialize<'a>>(text: &'a str) -> Result<T, DecodeError> {
    serde_json::from_str(text).map_err(|e| bad_json(&e))
}

fn to_json<T: serde::Serialize>(message: &T) -> Result<(u8, Vec<u8>), EncodeError> {
    serde_json::to_vec(message)
        .map(|json| (KIND_CONTROL, json))
        .map_err(|e| EncodeError::Json {
            detail: e.to_string(),
        })
}

fn short(kind: u8, needed: usize, payload: &[u8]) -> Result<(), DecodeError> {
    if payload.len() < needed {
        return Err(DecodeError::ShortPayload {
            kind,
            needed,
            actual: payload.len(),
        });
    }
    Ok(())
}

fn be_u64(bytes: &[u8]) -> u64 {
    let mut word = [0; 8];
    word.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(word)
}

fn binary(kind: u8, header: &[u64], bytes: &[u8]) -> (u8, Vec<u8>) {
    let mut payload = Vec::with_capacity(header.len() * 8 + bytes.len());
    for word in header {
        payload.extend_from_slice(&word.to_be_bytes());
    }
    payload.extend_from_slice(bytes);
    (kind, payload)
}

impl sealed::Sealed for ClientMessage {
    fn from_frame(kind: u8, payload: &[u8]) -> Result<Self, DecodeError> {
        match kind {
            KIND_CONTROL => {
                let (text, tag) = control_tag(payload)?;
                Ok(match tag.as_str() {
                    "hello" => Self::Hello(parse::<Hello>(text)?),
                    "attach" => Self::Attach(parse::<Attach>(text)?),
                    "resume" => Self::Resume(parse::<Resume>(text)?),
                    "detach" => Self::Detach(parse::<Detach>(text)?),
                    "ack" => Self::Ack(parse::<Ack>(text)?),
                    "resize" => Self::Resize(parse::<Resize>(text)?),
                    "focus" => Self::Focus(parse::<Focus>(text)?),
                    "set_display" => Self::SetDisplay(parse::<SetDisplay>(text)?),
                    "request_ownership" => Self::RequestOwnership(parse::<RequestOwnership>(text)?),
                    "watch_terminals" => Self::WatchTerminals(parse::<WatchTerminals>(text)?),
                    "watch_layout" => Self::WatchLayout(parse::<WatchLayout>(text)?),
                    "unwatch_layout" => Self::UnwatchLayout(parse::<UnwatchLayout>(text)?),
                    "mutate_layout" => Self::MutateLayout(parse::<MutateLayout>(text)?),
                    other => return Err(unknown_message_type(other)),
                })
            }
            KIND_INPUT => {
                short(kind, 8, payload)?;
                Ok(Self::Input(Input {
                    attachment: AttachmentId(be_u64(payload)),
                    bytes: payload[8..].to_vec(),
                }))
            }
            KIND_AUTOMATIC_REPLY => {
                short(kind, 24, payload)?;
                Ok(Self::AutomaticReply(AutomaticReply {
                    attachment: AttachmentId(be_u64(payload)),
                    epoch: Epoch(be_u64(&payload[8..])),
                    ownership_generation: OwnershipGeneration(be_u64(&payload[16..])),
                    bytes: payload[24..].to_vec(),
                }))
            }
            kind => Err(DecodeError::UnknownKind { kind }),
        }
    }

    fn to_frame(&self) -> Result<(u8, Vec<u8>), EncodeError> {
        match self {
            Self::Input(m) => Ok(binary(KIND_INPUT, &[m.attachment.0], &m.bytes)),
            Self::AutomaticReply(m) => Ok(binary(
                KIND_AUTOMATIC_REPLY,
                &[m.attachment.0, m.epoch.0, m.ownership_generation.0],
                &m.bytes,
            )),
            _ => to_json(self),
        }
    }
}

impl sealed::Sealed for DaemonMessage {
    fn from_frame(kind: u8, payload: &[u8]) -> Result<Self, DecodeError> {
        match kind {
            KIND_CONTROL => {
                let (text, tag) = control_tag(payload)?;
                Ok(match tag.as_str() {
                    "welcome" => Self::Welcome(parse::<Welcome>(text)?),
                    "attached" => Self::Attached(parse::<Attached>(text)?),
                    "resumed" => Self::Resumed(parse::<Resumed>(text)?),
                    "gap" => Self::Gap(parse::<Gap>(text)?),
                    "ownership" => Self::Ownership(parse::<Ownership>(text)?),
                    "error" => Self::Error(parse::<ErrorMessage>(text)?),
                    "terminals" => Self::Terminals(parse::<Terminals>(text)?),
                    "layout" => Self::Layout(parse::<Layout>(text)?),
                    "started" => Self::Started(parse::<Started>(text)?),
                    "resized" => Self::Resized(parse::<Resized>(text)?),
                    "facts" => Self::Facts(parse::<Facts>(text)?),
                    "resynced" => Self::Resynced(parse::<Resynced>(text)?),
                    "closed" => Self::Closed(parse::<Closed>(text)?),
                    "lost" => Self::Lost(parse::<Lost>(text)?),
                    other => return Err(unknown_message_type(other)),
                })
            }
            KIND_DATA => {
                short(kind, 24, payload)?;
                Ok(Self::Data(Data {
                    cursor: Cursor {
                        attachment: AttachmentId(be_u64(payload)),
                        epoch: Epoch(be_u64(&payload[8..])),
                        seq: Seq(be_u64(&payload[16..])),
                    },
                    bytes: payload[24..].to_vec(),
                }))
            }
            kind => Err(DecodeError::UnknownKind { kind }),
        }
    }

    fn to_frame(&self) -> Result<(u8, Vec<u8>), EncodeError> {
        match self {
            Self::Data(m) => Ok(binary(
                KIND_DATA,
                &[m.cursor.attachment.0, m.cursor.epoch.0, m.cursor.seq.0],
                &m.bytes,
            )),
            _ => to_json(self),
        }
    }
}
