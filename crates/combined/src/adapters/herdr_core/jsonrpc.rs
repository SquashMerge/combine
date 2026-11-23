//! herdr-core's JSON-RPC messages, as serde types and pure mappings to Combine's own types.
//!
//! Payload shapes follow `hc schema` for calls, and the committed capture for events (see
//! `events.rs`). Unknown fields are ignored everywhere so a newer core can add them.

use std::collections::BTreeMap;

use combine_protocol::{Display, Fidelity, TermId};
use serde::Deserialize;
use serde_json::{json, Value};

use super::PREFIX;
use crate::model::{SpawnSpec, TermInfo, TermState};
use crate::ports::{AttachEvent, BackendError};

/// The protocol generation this client speaks.
pub(super) const PROTOCOL_GENERATION: u64 = 1;

/// Methods the adapter calls; `hello` must list them all.
const REQUIRED_METHODS: [&str; 5] = [
    "terminal.attach",
    "terminal.list",
    "terminal.run",
    "terminal.kill",
    "events.subscribe",
];

/// An error reply: the JSON-RPC code and message, and herdr-core's stable `data.kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RpcError {
    pub code: i64,
    pub message: String,
    pub kind: Option<String>,
}

impl RpcError {
    pub(super) fn to_backend_error(&self, id: &TermId) -> BackendError {
        match self.kind.as_deref() {
            Some("not_found") => BackendError::UnknownTerminal(id.clone()),
            Some("machine_unknown") => BackendError::RemoteTerminal(id.clone()),
            _ => self.to_failure(),
        }
    }

    pub(super) fn to_failure(&self) -> BackendError {
        match &self.kind {
            Some(kind) => BackendError::Failed(format!("{kind}: {}", self.message)),
            None => BackendError::Failed(self.message.clone()),
        }
    }
}

pub(super) fn request(id: u64, method: &str, params: Value) -> Vec<u8> {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        .to_string()
        .into_bytes()
}

/// The plain-text line that opens every connection and switches it to framed mode.
pub(super) fn hello_line() -> String {
    let mut line = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "hello",
        "params": {"framed": true, "generation": PROTOCOL_GENERATION},
    })
    .to_string();
    line.push('\n');
    line
}

/// A reply to a call: its id, and the result or the error.
pub(super) fn parse_response(message: &[u8]) -> Option<(u64, Result<Value, RpcError>)> {
    let value: Value = serde_json::from_slice(message).ok()?;
    let id = value.get("id")?.as_u64()?;
    if let Some(error) = value.get("error") {
        return Some((id, Err(parse_error(error))));
    }
    Some((id, Ok(value.get("result").cloned().unwrap_or(Value::Null))))
}

fn parse_error(error: &Value) -> RpcError {
    RpcError {
        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string(),
        kind: error
            .pointer("/data/kind")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// What `hello` told us about the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hello {
    pub version: String,
    pub features: Vec<String>,
}

impl Hello {
    pub(super) fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

#[derive(Deserialize)]
struct HelloResult {
    generation: u64,
    version: String,
    framed: bool,
    #[serde(default)]
    methods: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
}

/// Reads the reply line to `hello`. Anything but generation 1, framed, with every required
/// method, is a refusal.
pub(super) fn parse_hello(line: &[u8]) -> Result<Hello, BackendError> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|error| BackendError::Failed(format!("unreadable hello reply: {error}")))?;
    if let Some(error) = value.get("error") {
        let error = parse_error(error);
        return Err(BackendError::Failed(match error.kind {
            Some(kind) => format!("herdr-core refused hello: {kind}"),
            None => format!("herdr-core refused hello: {}", error.message),
        }));
    }
    let result = value
        .get("result")
        .cloned()
        .ok_or_else(|| BackendError::Failed("hello reply has no result".to_string()))?;
    let hello: HelloResult = serde_json::from_value(result)
        .map_err(|error| BackendError::Failed(format!("unreadable hello result: {error}")))?;
    if hello.generation != PROTOCOL_GENERATION {
        return Err(BackendError::Failed(format!(
            "herdr-core speaks protocol generation {}, not {PROTOCOL_GENERATION}",
            hello.generation
        )));
    }
    if !hello.framed {
        return Err(BackendError::Failed(
            "herdr-core did not switch to framed mode".to_string(),
        ));
    }
    if let Some(missing) = REQUIRED_METHODS
        .iter()
        .find(|method| !hello.methods.iter().any(|m| m == *method))
    {
        return Err(BackendError::Failed(format!(
            "herdr-core lacks the method {missing}"
        )));
    }
    Ok(Hello {
        version: hello.version,
        features: hello.features,
    })
}

/// A terminal as herdr-core reports it.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct WireTerminal {
    pub address: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub created_ms: u64,
    #[serde(default)]
    pub screen: String,
}

/// `hc:<address>`.
pub(super) fn term_id(address: &str) -> TermId {
    TermId(format!("{PREFIX}:{address}"))
}

/// The address of an `hc:` ID; `None` for another prefix or an empty address.
pub(super) fn address_of(id: &TermId) -> Option<&str> {
    id.0.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_prefix(':'))
        .filter(|address| !address.is_empty())
}

/// An address on another machine, like `work/t1`.
pub(super) fn is_remote(address: &str) -> bool {
    address.contains('/')
}

impl WireTerminal {
    pub(super) fn to_info(&self) -> TermInfo {
        TermInfo {
            id: term_id(&self.address),
            cwd: self.cwd.clone(),
            argv: self.argv.clone(),
            state: if self.state == "finished" {
                TermState::Exited {
                    exit_code: self.exit_code,
                }
            } else {
                TermState::Running
            },
            labels: self.labels.clone(),
            cols: self.cols,
            rows: self.rows,
        }
    }
}

/// `inexact` after a core crash; anything else is exact.
pub(super) fn fidelity_of(screen: &str) -> Fidelity {
    if screen == "inexact" {
        Fidelity::Inexact
    } else {
        Fidelity::Exact
    }
}

/// The local terminals of a `terminal.list` result, in the core's order.
pub(super) fn list_result(result: &Value) -> Result<Vec<TermInfo>, BackendError> {
    let terminals = result
        .get("terminals")
        .cloned()
        .ok_or_else(|| BackendError::Failed("terminal.list has no terminals".to_string()))?;
    let terminals: Vec<WireTerminal> = serde_json::from_value(terminals)
        .map_err(|error| BackendError::Failed(format!("unreadable terminal list: {error}")))?;
    Ok(terminals
        .iter()
        .filter(|terminal| !is_remote(&terminal.address))
        .map(WireTerminal::to_info)
        .collect())
}

/// Parameters of `terminal.run`. Never `on`: the daemon starts terminals on its own machine
/// only.
pub(super) fn run_params(spec: &SpawnSpec) -> Value {
    json!({
        "argv": spec.argv,
        "cwd": spec.cwd,
        "env": spec.env,
        "labels": spec.labels,
        "size": {"cols": spec.cols, "rows": spec.rows},
        "no_parent": true,
    })
}

/// The address `terminal.run` started.
pub(super) fn run_result(result: &Value) -> Result<String, BackendError> {
    let address = result
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(|| BackendError::Failed("terminal.run returned no address".to_string()))?;
    if is_remote(address) {
        return Err(BackendError::Failed(format!(
            "terminal.run started {address} on another machine"
        )));
    }
    Ok(address.to_string())
}

fn is_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Combine's display facts as herdr-core's `Display`: a colour that isn't `#rrggbb` and a cell
/// size with a zero side become `null`.
pub(super) fn display_json(display: &Display) -> Value {
    let color = |value: &str| {
        if is_color(value) {
            Value::String(value.to_ascii_lowercase())
        } else {
            Value::Null
        }
    };
    let cell = if display.cell_width > 0 && display.cell_height > 0 {
        json!([display.cell_width, display.cell_height])
    } else {
        Value::Null
    };
    json!({
        "fg": color(&display.foreground),
        "bg": color(&display.background),
        "cursor": color(&display.cursor_color),
        "cell": cell,
    })
}

pub(super) struct AttachParams<'a> {
    pub address: &'a str,
    pub cols: u16,
    pub rows: u16,
    pub display: &'a Display,
    pub history: u32,
    pub observe: bool,
    pub resize_notifications: bool,
    pub resumed: bool,
}

pub(super) fn attach_params(params: &AttachParams<'_>) -> Value {
    json!({
        "terminal": params.address,
        "cols": params.cols,
        "rows": params.rows,
        "display": display_json(params.display),
        "history": params.history,
        "observe": params.observe,
        "resize_notifications": params.resize_notifications,
        "resumed": params.resumed,
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct AttachResult {
    pub channel: u32,
    pub terminal: WireTerminal,
    #[serde(default)]
    pub resize_notifications: bool,
    #[serde(default)]
    pub automatic_input: bool,
}

pub(super) fn parse_attach_result(result: Value) -> Result<AttachResult, BackendError> {
    serde_json::from_value(result)
        .map_err(|error| BackendError::Failed(format!("unreadable attach result: {error}")))
}

/// What a CLOSE frame from the core means. A holder that died took the program with it:
/// that is `Lost`. Any other reason is the program ending.
pub(super) fn close_event(payload: &[u8]) -> AttachEvent {
    let reason = serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    match reason.as_deref() {
        Some("holder_lost") => AttachEvent::Lost,
        _ => AttachEvent::Closed(combine_protocol::CloseReason::Exited),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello_reply(generation: u64, framed: bool, features: &[&str], methods: &[&str]) -> Vec<u8> {
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {
                "generation": generation,
                "version": "0.1.21",
                "framed": framed,
                "core": {"id": "x", "name": "y"},
                "methods": methods,
                "features": features,
            }
        })
        .to_string()
        .into_bytes()
    }

    fn all_methods() -> Vec<&'static str> {
        let mut methods = REQUIRED_METHODS.to_vec();
        methods.push("hello");
        methods
    }

    fn terminal(address: &str, state: &str) -> Value {
        json!({
            "address": address,
            "argv": ["/bin/sh"],
            "cwd": "/tmp",
            "labels": {"a": "b"},
            "cols": 80,
            "rows": 24,
            "state": state,
            "exit_code": null,
            "created_ms": 7,
            "screen": "exact",
            "pid": 5,
            "unknown_field": [1, 2],
        })
    }

    #[test]
    fn hello_accepts_generation_1() {
        let hello = parse_hello(&hello_reply(1, true, &["framed"], &all_methods())).expect("hello");
        assert_eq!(hello.version, "0.1.21");
        assert!(hello.has_feature("framed"));
        assert!(hello_line().contains(r#""framed":true"#));
        assert!(hello_line().contains(r#""generation":1"#));
        assert!(hello_line().ends_with('\n'));
    }

    #[test]
    fn hello_refuses_an_unknown_generation() {
        assert!(parse_hello(&hello_reply(2, true, &[], &all_methods())).is_err());
        assert!(parse_hello(&hello_reply(1, false, &[], &all_methods())).is_err());
        assert!(parse_hello(&hello_reply(1, true, &[], &["terminal.attach"])).is_err());
        let refusal = br#"{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"unsupported_generation","data":{"kind":"unsupported_generation"}}}"#;
        let Err(BackendError::Failed(message)) = parse_hello(refusal) else {
            panic!("an unsupported generation must be refused");
        };
        assert!(message.contains("unsupported_generation"));
    }

    #[test]
    fn hello_reads_automatic_input_and_resize_notifications_features() {
        let features = ["automatic_input", "resize_notifications"];
        let hello = parse_hello(&hello_reply(1, true, &features, &all_methods())).expect("hello");
        assert!(hello.has_feature("automatic_input"));
        assert!(hello.has_feature("resize_notifications"));
        let bare = parse_hello(&hello_reply(1, true, &[], &all_methods())).expect("hello");
        assert!(!bare.has_feature("automatic_input"));
    }

    #[test]
    fn terminal_maps_to_term_info() {
        let wire: WireTerminal = serde_json::from_value(terminal("t7", "live")).expect("terminal");
        let info = wire.to_info();
        assert_eq!(info.id, TermId("hc:t7".to_string()));
        assert_eq!(info.state, TermState::Running);
        assert_eq!((info.cols, info.rows), (80, 24));
        assert_eq!(info.cwd, "/tmp");
        let finished: WireTerminal =
            serde_json::from_value(terminal("t8", "finished")).expect("terminal");
        assert_eq!(
            finished.to_info().state,
            TermState::Exited { exit_code: None }
        );
    }

    #[test]
    fn list_drops_machine_qualified_addresses() {
        let captured: Value =
            serde_json::from_str(include_str!("fixtures/subscribe-2.json")).expect("a capture");
        let mut terminals = captured["terminals"].as_array().expect("terminals").clone();
        let mut remote = terminals[0].clone();
        remote["address"] = json!("work/t1");
        terminals.push(remote);
        let list = list_result(&json!({ "terminals": terminals })).expect("list");
        assert_eq!(list.len(), terminals.len() - 1);
        assert!(list.iter().all(|info| !info.id.0.contains('/')));
        let result = json!({"terminals": [terminal("t1", "live"), terminal("work/t1", "live")]});
        assert_eq!(list_result(&result).expect("list").len(), 1);
    }

    #[test]
    fn run_params_never_name_a_machine() {
        let spec = SpawnSpec {
            argv: vec!["x".to_string()],
            cwd: "/tmp".to_string(),
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
            cols: 100,
            rows: 30,
        };
        let params = run_params(&spec);
        assert!(params.get("on").is_none());
        assert_eq!(params["no_parent"], json!(true));
        assert_eq!(params["size"], json!({"cols": 100, "rows": 30}));
        assert!(run_result(&json!({"address": "work/t1"})).is_err());
        assert_eq!(run_result(&json!({"address": "t1"})), Ok("t1".to_string()));
    }

    #[test]
    fn error_kinds_map_to_backend_errors() {
        let id = TermId("hc:t1".to_string());
        let error = |kind: &str| RpcError {
            code: -32000,
            message: kind.to_string(),
            kind: Some(kind.to_string()),
        };
        assert_eq!(
            error("not_found").to_backend_error(&id),
            BackendError::UnknownTerminal(id.clone())
        );
        assert_eq!(
            error("machine_unknown").to_backend_error(&id),
            BackendError::RemoteTerminal(id.clone())
        );
        assert!(matches!(
            error("unavailable").to_backend_error(&id),
            BackendError::Failed(_)
        ));
        let parsed = parse_response(
            br#"{"id":4,"error":{"code":-32000,"message":"no","data":{"kind":"not_found"}}}"#,
        )
        .expect("a reply");
        assert_eq!(parsed.0, 4);
        assert_eq!(
            parsed.1.expect_err("an error").kind.as_deref(),
            Some("not_found")
        );
    }

    #[test]
    fn inexact_screen_is_inexact_fidelity() {
        assert_eq!(fidelity_of("inexact"), Fidelity::Inexact);
        assert_eq!(fidelity_of("exact"), Fidelity::Exact);
        assert_eq!(fidelity_of(""), Fidelity::Exact);
    }

    #[test]
    fn close_reason_maps_holder_lost_to_lost() {
        assert_eq!(
            close_event(br#"{"reason":"holder_lost"}"#),
            AttachEvent::Lost
        );
        assert_eq!(
            close_event(br#"{"reason":"killed","signal":1}"#),
            AttachEvent::Closed(combine_protocol::CloseReason::Exited)
        );
        assert_eq!(
            close_event(b""),
            AttachEvent::Closed(combine_protocol::CloseReason::Exited)
        );
    }

    #[test]
    fn display_maps_valid_colors_only() {
        let display = Display {
            foreground: "#DDEEFF".to_string(),
            background: "blue".to_string(),
            cursor_color: "#aabbcc".to_string(),
            cell_width: 9,
            cell_height: 0,
        };
        let wire = display_json(&display);
        assert_eq!(wire["fg"], json!("#ddeeff"));
        assert_eq!(wire["bg"], Value::Null);
        assert_eq!(wire["cursor"], json!("#aabbcc"));
        assert_eq!(wire["cell"], Value::Null);
    }
}
