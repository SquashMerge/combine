//! The terminal panel event sink. Every message the attachment router delivers to a panel
//! stream goes down that stream's channel: output as a raw binary frame, everything else as a
//! JSON event. Every protocol counter in the JSON crosses as a decimal string.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use combine_protocol::{CloseReason, Cursor, DaemonMessage, Data, Display, ErrorCode, Fidelity};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::hosts::attachments::{StreamDelivery, StreamId, TerminalSink};
use crate::hosts::model::{DecimalU64, IpcCursor};

/// The bytes before the payload of a data frame: attachment, epoch and seq, each a big-endian
/// `u64`.
const DATA_HEADER_LEN: usize = 24;

/// A control event for a panel stream. Nullable fields serialize as an explicit `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcTerminalEvent {
    Connection {
        connected: bool,
    },
    Attached {
        request: DecimalU64,
        attachment: DecimalU64,
    },
    Resumed {
        request: DecimalU64,
        attachment: DecimalU64,
        replay_through: Option<IpcCursor>,
    },
    Gap {
        attachment: DecimalU64,
        request: Option<DecimalU64>,
    },
    Ownership {
        attachment: DecimalU64,
        ownership_generation: DecimalU64,
        owner: Option<DecimalU64>,
    },
    Error {
        code: ErrorCode,
        message: String,
        request: Option<DecimalU64>,
        attachment: Option<DecimalU64>,
    },
    Started {
        cursor: IpcCursor,
        backend_generation: DecimalU64,
        cols: u16,
        rows: u16,
        fidelity: Fidelity,
        ordered_resizes: bool,
    },
    Resized {
        cursor: IpcCursor,
        cols: u16,
        rows: u16,
    },
    Facts {
        cursor: IpcCursor,
        display: Display,
    },
    Resynced {
        cursor: IpcCursor,
    },
    Closed {
        cursor: IpcCursor,
        reason: CloseReason,
    },
    Lost {
        cursor: IpcCursor,
    },
}

impl IpcTerminalEvent {
    /// The event for a daemon message, or `None` for one that is not terminal traffic or is
    /// output, which has its own frame.
    fn from_message(message: &DaemonMessage) -> Option<Self> {
        Some(match message {
            DaemonMessage::Attached(m) => IpcTerminalEvent::Attached {
                request: m.request.into(),
                attachment: m.attachment.into(),
            },
            DaemonMessage::Resumed(m) => IpcTerminalEvent::Resumed {
                request: m.request.into(),
                attachment: m.attachment.into(),
                replay_through: m.replay_through.map(IpcCursor::from),
            },
            DaemonMessage::Gap(m) => IpcTerminalEvent::Gap {
                attachment: m.attachment.into(),
                request: m.request.map(DecimalU64::from),
            },
            DaemonMessage::Ownership(m) => IpcTerminalEvent::Ownership {
                attachment: m.attachment.into(),
                ownership_generation: m.ownership_generation.into(),
                owner: m.owner.map(DecimalU64::from),
            },
            DaemonMessage::Error(m) => IpcTerminalEvent::Error {
                code: m.code,
                message: m.message.clone(),
                request: m.request.map(DecimalU64::from),
                attachment: m.attachment.map(DecimalU64::from),
            },
            DaemonMessage::Started(m) => IpcTerminalEvent::Started {
                cursor: m.cursor.into(),
                backend_generation: m.backend_generation.into(),
                cols: m.cols,
                rows: m.rows,
                fidelity: m.fidelity,
                ordered_resizes: m.ordered_resizes,
            },
            DaemonMessage::Resized(m) => IpcTerminalEvent::Resized {
                cursor: m.cursor.into(),
                cols: m.cols,
                rows: m.rows,
            },
            DaemonMessage::Facts(m) => IpcTerminalEvent::Facts {
                cursor: m.cursor.into(),
                display: m.display.clone(),
            },
            DaemonMessage::Resynced(m) => IpcTerminalEvent::Resynced {
                cursor: m.cursor.into(),
            },
            DaemonMessage::Closed(m) => IpcTerminalEvent::Closed {
                cursor: m.cursor.into(),
                reason: m.reason,
            },
            DaemonMessage::Lost(m) => IpcTerminalEvent::Lost {
                cursor: m.cursor.into(),
            },
            DaemonMessage::Welcome(_)
            | DaemonMessage::Terminals(_)
            | DaemonMessage::Layout(_)
            | DaemonMessage::Data(_) => return None,
        })
    }
}

fn data_frame(cursor: Cursor, bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(DATA_HEADER_LEN + bytes.len());
    frame.extend_from_slice(&cursor.attachment.0.to_be_bytes());
    frame.extend_from_slice(&cursor.epoch.0.to_be_bytes());
    frame.extend_from_slice(&cursor.seq.0.to_be_bytes());
    frame.extend_from_slice(bytes);
    frame
}

fn body(delivery: &StreamDelivery<'_>) -> Option<InvokeResponseBody> {
    let event = match delivery {
        StreamDelivery::Connection { connected } => IpcTerminalEvent::Connection {
            connected: *connected,
        },
        StreamDelivery::Message(DaemonMessage::Data(Data { cursor, bytes })) => {
            return Some(InvokeResponseBody::Raw(data_frame(*cursor, bytes)));
        }
        StreamDelivery::Message(message) => IpcTerminalEvent::from_message(message)?,
    };
    serde_json::to_string(&event)
        .ok()
        .map(InvokeResponseBody::Json)
}

/// Sends the router's deliveries down the channel each panel stream opened.
#[derive(Default)]
pub struct TauriTerminalSink {
    channels: Mutex<BTreeMap<u64, Channel<InvokeResponseBody>>>,
}

impl TauriTerminalSink {
    fn channels(&self) -> MutexGuard<'_, BTreeMap<u64, Channel<InvokeResponseBody>>> {
        self.channels.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drops every channel: the webview that held them reloaded or closed.
    pub fn clear(&self) {
        self.channels().clear();
    }
}

impl TerminalSink for TauriTerminalSink {
    fn deliver(&self, stream: StreamId, event: StreamDelivery<'_>) {
        let Some(body) = body(&event) else {
            return;
        };
        if let Some(channel) = self.channels().get(&stream.0) {
            let _ = channel.send(body);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::mpsc;

    use combine_protocol::{WireMessage, MAX_FRAME_LEN};
    use serde_json::{json, Value};

    use super::*;

    fn fixture() -> Value {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/test/fixtures/terminal-events.json");
        serde_json::from_str(&fs::read_to_string(path).expect("fixture")).expect("fixture json")
    }

    fn decimal(text: &str) -> bool {
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
    }

    /// The protocol's own JSON for an IPC event: counters are numbers, not decimal strings.
    fn wire(value: &Value) -> Value {
        match value {
            Value::String(text) if decimal(text) => json!(text.parse::<u64>().expect("a u64")),
            Value::Array(items) => Value::Array(items.iter().map(wire).collect()),
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), wire(v))).collect())
            }
            other => other.clone(),
        }
    }

    fn decode(wire: &Value) -> DaemonMessage {
        let payload = serde_json::to_vec(wire).expect("json");
        let mut frame = Vec::new();
        frame.extend_from_slice(&(payload.len() as u32 + 1).to_be_bytes());
        frame.push(1);
        frame.extend_from_slice(&payload);
        assert!(payload.len() < MAX_FRAME_LEN as usize);
        DaemonMessage::decode(&frame).expect("a daemon message").0
    }

    fn json_of(body: InvokeResponseBody) -> Value {
        match body {
            InvokeResponseBody::Json(text) => serde_json::from_str(&text).expect("json"),
            InvokeResponseBody::Raw(_) => panic!("expected a JSON body"),
        }
    }

    fn cursor(attachment: u64, epoch: u64, seq: u64) -> Cursor {
        use combine_protocol::{AttachmentId, Epoch, Seq};
        Cursor {
            attachment: AttachmentId(attachment),
            epoch: Epoch(epoch),
            seq: Seq(seq),
        }
    }

    #[test]
    fn control_events_match_fixture() {
        let fixture = fixture();
        let entries = fixture["control"].as_array().expect("control entries");
        assert!(!entries.is_empty());
        for entry in entries {
            let name = &entry["name"];
            let ipc = &entry["ipc"];
            let message = (ipc["type"] != "connection").then(|| decode(&wire(ipc)));
            let delivery = match &message {
                Some(message) => StreamDelivery::Message(message),
                None => StreamDelivery::Connection {
                    connected: ipc["connected"].as_bool().expect("connected"),
                },
            };
            let body = body(&delivery).expect("a body");
            assert_eq!(&json_of(body), ipc, "{name}");
        }
    }

    #[test]
    fn data_frame_header_matches_fixture() {
        let fixture = fixture();
        let entries = fixture["data"].as_array().expect("data entries");
        assert!(!entries.is_empty());
        for entry in entries {
            let number = |key: &str| {
                entry["cursor"][key]
                    .as_str()
                    .and_then(|text| text.parse::<u64>().ok())
                    .expect(key)
            };
            let payload = hex(entry["payload_hex"].as_str().expect("payload"));
            let message = DaemonMessage::Data(Data {
                cursor: cursor(number("attachment"), number("epoch"), number("seq")),
                bytes: payload,
            });
            let Some(InvokeResponseBody::Raw(frame)) = body(&StreamDelivery::Message(&message))
            else {
                panic!("expected a raw body");
            };
            assert_eq!(
                frame,
                hex(entry["frame_hex"].as_str().expect("frame")),
                "{}",
                entry["name"]
            );
        }
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn u64_fields_cross_as_decimal_strings() {
        let max = u64::MAX;
        let message = DaemonMessage::Resized(combine_protocol::Resized {
            cursor: cursor(max, max, max),
            cols: 80,
            rows: 24,
        });
        let event = json_of(body(&StreamDelivery::Message(&message)).expect("a body"));
        let text = max.to_string();
        assert_eq!(
            event["cursor"],
            json!({ "attachment": text, "epoch": text, "seq": text })
        );
        let ownership = DaemonMessage::Ownership(combine_protocol::Ownership {
            attachment: combine_protocol::AttachmentId(max),
            ownership_generation: combine_protocol::OwnershipGeneration(max),
            owner: Some(combine_protocol::AttachmentId(max)),
        });
        let event = json_of(body(&StreamDelivery::Message(&ownership)).expect("a body"));
        for key in ["attachment", "ownership_generation", "owner"] {
            assert_eq!(event[key], json!(text), "{key}");
        }
    }

    #[test]
    fn deliveries_reach_the_streams_channel_only() {
        let sink = TauriTerminalSink::default();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let channel = Channel::new(move |body| {
            let _ = tx.lock().expect("sender").send(body);
            Ok(())
        });
        sink.channels().insert(5, channel);
        sink.deliver(StreamId(6), StreamDelivery::Connection { connected: true });
        assert!(rx.try_recv().is_err());
        sink.deliver(StreamId(5), StreamDelivery::Connection { connected: true });
        let received = rx.try_recv().expect("delivered");
        assert_eq!(
            json_of(received),
            json!({"type": "connection", "connected": true})
        );
        sink.clear();
        sink.deliver(StreamId(5), StreamDelivery::Connection { connected: false });
        assert!(rx.try_recv().is_err());
    }
}
