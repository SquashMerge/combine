//! JSON-RPC framing: the bounds on what comes in, and replies that always fit one frame.
//!
//! Nothing here builds a JSON tree. Untrusted text is scanned for depth, then read
//! into borrowed `RawValue` slices, so memory per message is bounded by the message's length.

use combine_protocol::{DaemonMessage, JsonRpcMessage, Mcp, WireMessage, MAX_FRAME_LEN};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use super::MCP_ID_MAX_BYTES;

pub(super) const INVALID_REQUEST: i32 = -32600;
pub(super) const METHOD_NOT_FOUND: i32 = -32601;
pub(super) const INVALID_PARAMS: i32 = -32602;
pub(super) const INTERNAL_ERROR: i32 = -32603;

/// The most serde text an error reply echoes.
pub(super) const SERDE_TEXT_MAX: usize = 256;
/// The most method or tool name text an error reply echoes.
pub(super) const NAME_TEXT_MAX: usize = 128;

/// What `handle` reads of a message first. Unknown fields and the bodies of the `RawValue`
/// fields are skipped by serde_json without building anything.
#[derive(Deserialize)]
pub(super) struct Envelope<'a> {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    #[serde(borrow, default, deserialize_with = "present")]
    pub id: Option<&'a RawValue>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(borrow, default)]
    pub params: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub result: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub error: Option<&'a RawValue>,
}

/// Just the id, for answering a message the envelope refused.
#[derive(Deserialize)]
pub(super) struct IdOnly<'a> {
    #[serde(borrow, default, deserialize_with = "present")]
    pub id: Option<&'a RawValue>,
}

/// Keeps a field that is present but `null` apart from one that is absent.
pub(super) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Whether the text nests objects and arrays deeper than `max`, the message's own object
/// counting as 1. It is one pass over the bytes and allocates nothing.
pub(super) fn nests_deeper_than(text: &str, max: usize) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in text.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// Why an id can't be echoed.
pub(super) enum IdRefusal {
    NotStringOrNumber,
    TooLong,
}

impl IdRefusal {
    pub(super) fn message(&self) -> String {
        match self {
            IdRefusal::NotStringOrNumber => {
                "Invalid Request: id must be a string or a number".to_owned()
            }
            IdRefusal::TooLong => {
                format!("Invalid Request: id is longer than {MCP_ID_MAX_BYTES} bytes")
            }
        }
    }
}

/// The id's raw JSON text when a reply may echo it: a string or a number of at most
/// `MCP_ID_MAX_BYTES` bytes.
pub(super) fn echoable_id(raw: &RawValue) -> Result<&str, IdRefusal> {
    let text = raw.get();
    if !matches!(text.as_bytes().first(), Some(b'"' | b'-' | b'0'..=b'9')) {
        return Err(IdRefusal::NotStringOrNumber);
    }
    if text.len() > MCP_ID_MAX_BYTES {
        return Err(IdRefusal::TooLong);
    }
    Ok(text)
}

/// The id of a message the envelope parse refused or the depth scan stopped, when it has one
/// a reply may echo.
pub(super) fn salvage_id(text: &str) -> Option<&str> {
    let only: IdOnly<'_> = serde_json::from_str(text).ok()?;
    echoable_id(only.id?).ok()
}

/// `text` cut to at most `max` bytes on a char boundary.
pub(super) fn clip(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(super) fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

fn id_text(id: Option<&str>) -> &str {
    id.unwrap_or("null")
}

fn seal(text: String) -> Option<JsonRpcMessage> {
    let wire = DaemonMessage::Mcp(Mcp {
        message: JsonRpcMessage::new(text).ok()?,
    });
    wire.encode().ok()?;
    match wire {
        DaemonMessage::Mcp(mcp) => Some(mcp.message),
        _ => None,
    }
}

fn error_text(id: Option<&str>, code: i32, message: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{code},"message":{}}}}}"#,
        id_text(id),
        quoted(message)
    )
}

/// An error reply. The id is at most `MCP_ID_MAX_BYTES` and the message is short, so it fits.
pub(super) fn error_reply(id: Option<&str>, code: i32, message: &str) -> JsonRpcMessage {
    seal(error_text(id, code, message)).unwrap_or_else(|| too_large_reply(None))
}

fn too_large_reply(id: Option<&str>) -> JsonRpcMessage {
    let message =
        format!("Internal error: the reply is larger than one frame ({MAX_FRAME_LEN} bytes)");
    seal(error_text(id, INTERNAL_ERROR, &message)).unwrap_or_else(|| {
        let text = error_text(None, INTERNAL_ERROR, &message);
        match JsonRpcMessage::new(text) {
            Ok(message) => message,
            Err(_) => unreachable!("a fixed JSON-RPC error is one JSON object"),
        }
    })
}

fn result_text(id: Option<&str>, result: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
        id_text(id)
    )
}

/// Whether a result with this id encodes into one frame.
pub(super) fn fits(id: Option<&str>, result: &str) -> bool {
    seal(result_text(id, result)).is_some()
}

/// A result reply. One that doesn't fit a frame becomes `-32603` with the same id, and
/// failing that, with a null id.
pub(super) fn result_reply(id: Option<&str>, result: &str) -> JsonRpcMessage {
    seal(result_text(id, result)).unwrap_or_else(|| too_large_reply(id))
}
