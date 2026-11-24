//! The agents' MCP channel: who opens it, and the frames that carry its
//! JSON-RPC messages.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use crate::codec::bad_json;
use crate::error::DecodeError;

/// Who opens the MCP channel, as the agent's environment describes it: the
/// terminal ID and the backend socket it names, and the working directory of
/// the process that opened the channel. Each is `None` when unknown. The codec
/// checks none of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCaller {
    pub terminal_id: Option<String>,
    pub terminal_socket: Option<String>,
    pub cwd: Option<String>,
}

const NOT_AN_OBJECT: &str = "a JSON-RPC message must be a JSON object";

/// One JSON-RPC message: a request, a response or a notification.
///
/// It is one JSON object, never an array, `null` or a scalar, and it is kept
/// as the exact JSON text it arrived as: it is written back byte for byte, and
/// two messages are equal when their text is. It is never decoded into a
/// tree, which can hold far more memory than its text.
///
/// It decodes only straight from serde_json, as the codec decodes every
/// message. serde's buffered forms (internally tagged or untagged enums,
/// `flatten`) can't carry it.
#[derive(Debug, Clone)]
pub struct JsonRpcMessage(Box<RawValue>);

impl JsonRpcMessage {
    /// Takes one JSON-RPC message's JSON text. Text that isn't exactly one JSON
    /// object is a [`DecodeError::BadJson`].
    pub fn new(json: String) -> Result<JsonRpcMessage, DecodeError> {
        let raw = RawValue::from_string(json).map_err(|e| bad_json(&e))?;
        JsonRpcMessage::from_raw(raw).map_err(|detail| DecodeError::BadJson {
            detail: detail.to_owned(),
        })
    }

    /// The message's JSON text, exactly as it was received or given.
    pub fn as_str(&self) -> &str {
        self.0.get()
    }

    fn from_raw(raw: Box<RawValue>) -> Result<JsonRpcMessage, &'static str> {
        let text = raw.get().trim_start_matches([' ', '\t', '\n', '\r']);
        if text.starts_with('{') {
            Ok(JsonRpcMessage(raw))
        } else {
            Err(NOT_AN_OBJECT)
        }
    }
}

impl PartialEq for JsonRpcMessage {
    fn eq(&self, other: &JsonRpcMessage) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for JsonRpcMessage {}

impl Serialize for JsonRpcMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonRpcMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<JsonRpcMessage, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        JsonRpcMessage::from_raw(raw).map_err(<D::Error as serde::de::Error>::custom)
    }
}

/// One frame of the MCP channel, in either direction, after `mcp_opened`. It
/// carries exactly one JSON-RPC message. It has no `request` and is never
/// answered by an `error`: a JSON-RPC error travels inside an `mcp` frame. It
/// is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mcp {
    pub message: JsonRpcMessage,
}
