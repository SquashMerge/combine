//! The terminal panel event sink and the terminal commands. Every message the attachment router
//! delivers to a panel stream goes down that stream's channel: output as a raw binary frame,
//! everything else as a JSON event. Every protocol counter crosses as a decimal string, in the
//! events, in the command arguments and in the raw-body headers. Every command goes through
//! `Attachments`; none writes to the outbound queue directly.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use combine_protocol::{
    Ack, AttachRequestOptions, AttachmentId, AutomaticReply, ClientMessage, CloseReason, Cursor,
    DaemonMessage, Data, Detach, Display, Epoch, ErrorCode, Fidelity, Focus, HostId, Input,
    OwnershipGeneration, RequestOwnership, Resize, SetDisplay, TermId,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tauri::http::HeaderMap;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::State;

use crate::hosts::attachments::{ForwardError, StreamDelivery, StreamId, TerminalSink};
use crate::hosts::model::{DecimalU64, IpcCursor};
use crate::hosts::service::HostsService;

/// The bytes before the payload of a data frame: attachment, epoch and seq, each a big-endian
/// `u64`.
const DATA_HEADER_LEN: usize = 24;

/// A control event for a panel stream. Nullable fields serialize as an explicit `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcTerminalEvent {
    /// `instance` is the backend instance the connected session's daemon serves, or `null`
    /// while not connected and for a daemon that names none.
    Connection {
        connected: bool,
        instance: Option<String>,
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
            | DaemonMessage::Templates(_)
            | DaemonMessage::Spawned(_)
            | DaemonMessage::Panels(_)
            | DaemonMessage::Panel(_)
            | DaemonMessage::PanelClosed(_)
            | DaemonMessage::PanelPinned(_)
            | DaemonMessage::McpOpened(_)
            | DaemonMessage::Mcp(_)
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
        StreamDelivery::Connection {
            connected,
            instance,
        } => IpcTerminalEvent::Connection {
            connected: *connected,
            instance: instance.map(str::to_string),
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
pub struct TauriTerminalSink {
    next: AtomicU64,
    channels: Mutex<BTreeMap<u64, Channel<InvokeResponseBody>>>,
}

impl Default for TauriTerminalSink {
    fn default() -> Self {
        TauriTerminalSink {
            next: AtomicU64::new(1),
            channels: Mutex::default(),
        }
    }
}

impl TauriTerminalSink {
    fn channels(&self) -> MutexGuard<'_, BTreeMap<u64, Channel<InvokeResponseBody>>> {
        self.channels.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Names a new panel stream and keeps its channel.
    fn register(&self, channel: Channel<InvokeResponseBody>) -> StreamId {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.channels().insert(id, channel);
        StreamId(id)
    }

    fn unregister(&self, stream: StreamId) {
        self.channels().remove(&stream.0);
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

const NOT_CONNECTED: &str = "not connected";
const UNKNOWN_STREAM: &str = "unknown stream";
const UNKNOWN_ATTACHMENT: &str = "unknown attachment";
const TOO_LARGE: &str = "too large";
const MALFORMED: &str = "malformed";

const STREAM_HEADER: &str = "Combine-Stream";
const ATTACHMENT_HEADER: &str = "Combine-Attachment";
const EPOCH_HEADER: &str = "Combine-Epoch";
const OWNERSHIP_GENERATION_HEADER: &str = "Combine-Ownership-Generation";

fn forward_error(error: ForwardError) -> String {
    match error {
        ForwardError::NotConnected => NOT_CONNECTED,
        ForwardError::UnknownStream => UNKNOWN_STREAM,
        ForwardError::UnknownAttachment => UNKNOWN_ATTACHMENT,
        ForwardError::TooLarge => TOO_LARGE,
        ForwardError::Invalid => MALFORMED,
    }
    .to_string()
}

fn parse_decimal(text: &str) -> Result<u64, String> {
    serde_json::from_value::<DecimalU64>(Value::String(text.to_string()))
        .map(|value| value.0)
        .map_err(|_| MALFORMED.to_string())
}

fn stream_id(text: &str) -> Result<StreamId, String> {
    parse_decimal(text).map(StreamId)
}

/// A structured argument, read here so that a bad one gets `malformed` rather than the IPC
/// layer's own text.
fn structured<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|_| MALFORMED.to_string())
}

fn header(headers: &HeaderMap, name: &str) -> Result<u64, String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| MALFORMED.to_string())
        .and_then(parse_decimal)
}

fn raw_body(body: &InvokeBody) -> Result<Vec<u8>, String> {
    match body {
        InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        InvokeBody::Json(_) => Err(MALFORMED.to_string()),
    }
}

/// The stream and the message of a raw-body `terminal_input` request.
fn input_message(
    headers: &HeaderMap,
    body: &InvokeBody,
) -> Result<(StreamId, ClientMessage), String> {
    let stream = StreamId(header(headers, STREAM_HEADER)?);
    let attachment = AttachmentId(header(headers, ATTACHMENT_HEADER)?);
    let bytes = raw_body(body)?;
    Ok((stream, ClientMessage::Input(Input { attachment, bytes })))
}

/// The stream and the message of a raw-body `terminal_automatic_reply` request.
fn automatic_reply_message(
    headers: &HeaderMap,
    body: &InvokeBody,
) -> Result<(StreamId, ClientMessage), String> {
    let stream = StreamId(header(headers, STREAM_HEADER)?);
    let reply = AutomaticReply {
        attachment: AttachmentId(header(headers, ATTACHMENT_HEADER)?),
        epoch: Epoch(header(headers, EPOCH_HEADER)?),
        ownership_generation: OwnershipGeneration(header(headers, OWNERSHIP_GENERATION_HEADER)?),
        bytes: raw_body(body)?,
    };
    Ok((stream, ClientMessage::AutomaticReply(reply)))
}

fn send(service: &HostsService, stream: StreamId, message: ClientMessage) -> Result<(), String> {
    service
        .attachments()
        .forward(&service.outbound(), stream, message)
        .map_err(forward_error)
}

/// Opens a panel stream on `host_id` and returns its ID, a decimal string.
#[tauri::command]
pub fn terminal_open_stream(
    service: State<'_, HostsService>,
    sink: State<'_, Arc<TauriTerminalSink>>,
    host_id: String,
    channel: Channel<InvokeResponseBody>,
) -> String {
    let stream = sink.register(channel);
    service.attachments().open_stream(stream, HostId(host_id));
    stream.0.to_string()
}

#[tauri::command]
pub fn terminal_close_stream(
    service: State<'_, HostsService>,
    sink: State<'_, Arc<TauriTerminalSink>>,
    stream: String,
) -> Result<(), String> {
    let stream = stream_id(&stream)?;
    service
        .attachments()
        .close_stream(&service.outbound(), stream);
    sink.unregister(stream);
    Ok(())
}

/// `instance` names the backend instance a pinned panel expects, and is `null` for a panel
/// that follows the active terminal.
#[tauri::command]
pub fn terminal_attach(
    service: State<'_, HostsService>,
    stream: String,
    terminal: String,
    options: Value,
    instance: Option<String>,
) -> Result<(), String> {
    let stream = stream_id(&stream)?;
    let options: AttachRequestOptions = structured(options)?;
    service
        .attachments()
        .attach(
            &service.outbound(),
            stream,
            TermId(terminal),
            options,
            instance,
        )
        .map_err(forward_error)
}

#[tauri::command]
pub fn terminal_resume(
    service: State<'_, HostsService>,
    stream: String,
    cursor: Value,
    instance: Option<String>,
) -> Result<(), String> {
    let stream = stream_id(&stream)?;
    let cursor: IpcCursor = structured(cursor)?;
    service
        .attachments()
        .resume(&service.outbound(), stream, cursor.into(), instance)
        .map_err(forward_error)
}

#[tauri::command]
pub fn terminal_detach(
    service: State<'_, HostsService>,
    stream: String,
    attachment: String,
) -> Result<(), String> {
    let message = ClientMessage::Detach(Detach {
        attachment: AttachmentId(parse_decimal(&attachment)?),
    });
    send(&service, stream_id(&stream)?, message)
}

#[tauri::command]
pub fn terminal_ack(
    service: State<'_, HostsService>,
    stream: String,
    cursor: Value,
) -> Result<(), String> {
    let cursor: IpcCursor = structured(cursor)?;
    let message = ClientMessage::Ack(Ack {
        cursor: cursor.into(),
    });
    send(&service, stream_id(&stream)?, message)
}

#[tauri::command]
pub fn terminal_resize(
    service: State<'_, HostsService>,
    stream: String,
    attachment: String,
    cols: u16,
    rows: u16,
    ownership_generation: String,
) -> Result<(), String> {
    let message = ClientMessage::Resize(Resize {
        attachment: AttachmentId(parse_decimal(&attachment)?),
        cols,
        rows,
        ownership_generation: OwnershipGeneration(parse_decimal(&ownership_generation)?),
    });
    send(&service, stream_id(&stream)?, message)
}

#[tauri::command]
pub fn terminal_focus(
    service: State<'_, HostsService>,
    stream: String,
    attachment: String,
) -> Result<(), String> {
    let message = ClientMessage::Focus(Focus {
        attachment: AttachmentId(parse_decimal(&attachment)?),
    });
    send(&service, stream_id(&stream)?, message)
}

#[tauri::command]
pub fn terminal_set_display(
    service: State<'_, HostsService>,
    stream: String,
    attachment: String,
    display: Value,
) -> Result<(), String> {
    let message = ClientMessage::SetDisplay(SetDisplay {
        attachment: AttachmentId(parse_decimal(&attachment)?),
        display: structured::<Display>(display)?,
    });
    send(&service, stream_id(&stream)?, message)
}

#[tauri::command]
pub fn terminal_request_ownership(
    service: State<'_, HostsService>,
    stream: String,
    attachment: String,
    if_vacant: Option<String>,
) -> Result<(), String> {
    let message = request_ownership_message(&attachment, if_vacant.as_deref())?;
    send(&service, stream_id(&stream)?, message)
}

/// `if_vacant` is the ownership generation, a decimal string, under which a panel saw the
/// terminal with no owner: its automatic claim. `None` is a manual "Take input".
fn request_ownership_message(
    attachment: &str,
    if_vacant: Option<&str>,
) -> Result<ClientMessage, String> {
    Ok(ClientMessage::RequestOwnership(RequestOwnership {
        attachment: AttachmentId(parse_decimal(attachment)?),
        if_vacant: if_vacant
            .map(parse_decimal)
            .transpose()?
            .map(OwnershipGeneration),
    }))
}

/// Typed input: the body is the bytes, and the headers name the stream and the attachment.
#[tauri::command]
pub fn terminal_input(
    service: State<'_, HostsService>,
    request: Request<'_>,
) -> Result<(), String> {
    let (stream, message) = input_message(request.headers(), request.body())?;
    send(&service, stream, message)
}

/// An emulator's reply to a terminal query: the body is the bytes, and the headers name the
/// stream, the attachment, the epoch and the ownership generation.
#[tauri::command]
pub fn terminal_automatic_reply(
    service: State<'_, HostsService>,
    request: Request<'_>,
) -> Result<(), String> {
    let (stream, message) = automatic_reply_message(request.headers(), request.body())?;
    send(&service, stream, message)
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
                    instance: ipc["instance"].as_str(),
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
        let up = || StreamDelivery::Connection {
            connected: true,
            instance: Some("core-a"),
        };
        sink.deliver(StreamId(6), up());
        assert!(rx.try_recv().is_err());
        sink.deliver(StreamId(5), up());
        let received = rx.try_recv().expect("delivered");
        assert_eq!(
            json_of(received),
            json!({"type": "connection", "connected": true, "instance": "core-a"})
        );
        sink.clear();
        sink.deliver(
            StreamId(5),
            StreamDelivery::Connection {
                connected: false,
                instance: None,
            },
        );
        assert!(rx.try_recv().is_err());
    }

    const MAX: &str = "18446744073709551615";

    #[test]
    fn a_claim_names_the_generation_it_saw_vacant_and_a_manual_take_names_none() {
        assert_eq!(
            request_ownership_message("7", Some(MAX)),
            Ok(ClientMessage::RequestOwnership(RequestOwnership {
                attachment: AttachmentId(7),
                if_vacant: Some(OwnershipGeneration(u64::MAX)),
            }))
        );
        assert_eq!(
            request_ownership_message("7", None),
            Ok(ClientMessage::RequestOwnership(RequestOwnership {
                attachment: AttachmentId(7),
                if_vacant: None,
            }))
        );
        for bad in ["", "-1", "1.5", "18446744073709551616", "07"] {
            assert_eq!(
                request_ownership_message("7", Some(bad)),
                Err(MALFORMED.to_string()),
                "{bad}"
            );
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                tauri::http::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
                tauri::http::HeaderValue::from_str(value).expect("a header value"),
            );
        }
        map
    }

    fn raw(bytes: &[u8]) -> InvokeBody {
        InvokeBody::Raw(bytes.to_vec())
    }

    #[test]
    fn command_names_match_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web/test/fixtures/ipc-contract.json"
        );
        let text = std::fs::read_to_string(path).expect("fixture is readable");
        let fixture: Value = serde_json::from_str(&text).expect("fixture is JSON");
        let commands = &fixture["commands"];
        let ours = [
            ("terminalOpenStream", "terminal_open_stream"),
            ("terminalCloseStream", "terminal_close_stream"),
            ("terminalAttach", "terminal_attach"),
            ("terminalResume", "terminal_resume"),
            ("terminalDetach", "terminal_detach"),
            ("terminalAck", "terminal_ack"),
            ("terminalResize", "terminal_resize"),
            ("terminalFocus", "terminal_focus"),
            ("terminalSetDisplay", "terminal_set_display"),
            ("terminalRequestOwnership", "terminal_request_ownership"),
            ("terminalInput", "terminal_input"),
            ("terminalAutomaticReply", "terminal_automatic_reply"),
        ];
        for (key, name) in ours {
            assert_eq!(commands[key], name, "{key}");
        }
        let terminal_keys = commands
            .as_object()
            .expect("commands")
            .keys()
            .filter(|key| key.starts_with("terminal"))
            .count();
        assert_eq!(terminal_keys, ours.len());
    }

    #[test]
    fn stream_ids_start_at_one_and_cross_as_decimal_strings() {
        let sink = TauriTerminalSink::default();
        let first = sink.register(Channel::new(|_| Ok(())));
        let second = sink.register(Channel::new(|_| Ok(())));
        assert_eq!((first, second), (StreamId(1), StreamId(2)));
        sink.unregister(first);
        assert!(!sink.channels().contains_key(&1));
        assert!(sink.channels().contains_key(&2));
        assert_eq!(stream_id(&second.0.to_string()), Ok(second));
        assert_eq!(stream_id(MAX), Ok(StreamId(u64::MAX)));
    }

    #[test]
    fn input_dtos_cross_as_decimal_strings() {
        let cursor: IpcCursor = structured(json!({
            "attachment": MAX, "epoch": MAX, "seq": "9007199254740993"
        }))
        .expect("a cursor");
        let cursor = Cursor::from(cursor);
        assert_eq!(
            (cursor.attachment.0, cursor.epoch.0, cursor.seq.0),
            (u64::MAX, u64::MAX, 9_007_199_254_740_993)
        );
        let number: Result<IpcCursor, _> =
            structured(json!({ "attachment": 1, "epoch": "1", "seq": "1" }));
        assert_eq!(number.map(|_| ()), Err("malformed".to_string()));

        let (stream, message) = automatic_reply_message(
            &headers(&[
                ("Combine-Stream", MAX),
                ("Combine-Attachment", MAX),
                ("Combine-Epoch", "9007199254740993"),
                ("Combine-Ownership-Generation", "3"),
            ]),
            &raw(b"\x1b[0n"),
        )
        .expect("a reply");
        assert_eq!(stream, StreamId(u64::MAX));
        assert_eq!(
            message,
            ClientMessage::AutomaticReply(AutomaticReply {
                attachment: AttachmentId(u64::MAX),
                epoch: Epoch(9_007_199_254_740_993),
                ownership_generation: OwnershipGeneration(3),
                bytes: b"\x1b[0n".to_vec(),
            })
        );
    }

    #[test]
    fn a_malformed_decimal_gets_malformed() {
        for bad in [
            "",
            "01",
            "-1",
            "+1",
            " 1",
            "1.5",
            "1e3",
            "18446744073709551616",
            "x",
        ] {
            assert_eq!(parse_decimal(bad), Err("malformed".to_string()), "{bad:?}");
            assert_eq!(stream_id(bad), Err("malformed".to_string()), "{bad:?}");
        }
        assert_eq!(parse_decimal("0"), Ok(0));
        let bad = headers(&[("Combine-Stream", "1"), ("Combine-Attachment", "01")]);
        assert_eq!(
            input_message(&bad, &raw(b"a")).map(|_| ()),
            Err("malformed".to_string())
        );
    }

    #[test]
    fn input_reads_its_headers_and_raw_body() {
        let good = headers(&[("Combine-Stream", "7"), ("Combine-Attachment", "9")]);
        let (stream, message) = input_message(&good, &raw(b"ls\r")).expect("input");
        assert_eq!(stream, StreamId(7));
        assert_eq!(
            message,
            ClientMessage::Input(Input {
                attachment: AttachmentId(9),
                bytes: b"ls\r".to_vec(),
            })
        );
        let missing = headers(&[("Combine-Stream", "7")]);
        assert_eq!(
            input_message(&missing, &raw(b"a")).map(|_| ()),
            Err("malformed".to_string())
        );
        assert_eq!(
            input_message(&good, &InvokeBody::Json(json!([1, 2]))).map(|_| ()),
            Err("malformed".to_string())
        );
    }

    #[test]
    fn automatic_reply_needs_all_four_headers() {
        let all = [
            ("Combine-Stream", "1"),
            ("Combine-Attachment", "2"),
            ("Combine-Epoch", "3"),
            ("Combine-Ownership-Generation", "4"),
        ];
        assert!(automatic_reply_message(&headers(&all), &raw(b"r")).is_ok());
        for skipped in 0..all.len() {
            let partial: Vec<_> = all
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skipped)
                .map(|(_, pair)| *pair)
                .collect();
            assert_eq!(
                automatic_reply_message(&headers(&partial), &raw(b"r")).map(|_| ()),
                Err("malformed".to_string()),
                "without {}",
                all[skipped].0
            );
        }
    }

    #[test]
    fn errors_use_the_documented_strings() {
        let texts = [
            (ForwardError::NotConnected, "not connected"),
            (ForwardError::UnknownStream, "unknown stream"),
            (ForwardError::UnknownAttachment, "unknown attachment"),
            (ForwardError::TooLarge, "too large"),
            (ForwardError::Invalid, "malformed"),
        ];
        for (error, text) in texts {
            assert_eq!(forward_error(error), text);
        }
    }

    #[test]
    fn display_and_options_deserialize_from_the_web_shape() {
        let display = json!({
            "foreground": "#ffffff", "background": "#000000", "cursor_color": "#ff0000",
            "cell_width": 8, "cell_height": 16
        });
        assert!(structured::<Display>(display.clone()).is_ok());
        let options = json!({
            "cols": 80, "rows": 24, "display": display, "observe": false, "history": 100
        });
        assert!(structured::<AttachRequestOptions>(options).is_ok());
        assert_eq!(
            structured::<Display>(json!({ "foreground": 1 })).map(|_| ()),
            Err("malformed".to_string())
        );
    }

    #[test]
    fn v5_daemon_messages_are_not_terminal_events() {
        let space = "/Users/me/src/trellis";
        let messages = [
            json!({
                "type": "panels", "request": 5, "space": space,
                "panels": [{
                    "id": "classes", "title": "Class diagram", "renderer": "mermaid",
                    "revision": 7, "state": "validated", "error": null,
                    "created_by": { "type": "agent", "slot": "claude" }
                }],
                "skipped": [], "error": null
            }),
            json!({
                "type": "panel", "request": 12, "space": space, "id": "classes",
                "revision": 7, "renderer": "mermaid",
                "source": "classDiagram\n  Animal <|-- Duck\n",
                "state": "validated", "error": null
            }),
            json!({ "type": "panel_closed", "request": 13, "space": space, "panel": "scratch-1" }),
            json!({ "type": "panel_pinned", "request": 14, "space": space, "panel": "classes" }),
            json!({ "type": "mcp_opened", "request": 1, "space": space }),
            json!({
                "type": "mcp",
                "message": { "jsonrpc": "2.0", "id": 1, "result": { "isError": false } }
            }),
        ];
        for message in &messages {
            let decoded = decode(message);
            assert!(IpcTerminalEvent::from_message(&decoded).is_none());
        }
    }
}
