//! The daemon's MCP server: JSON-RPC over the MCP channel, and the six tools that make
//! Mermaid and Markdown panels. It is a hand-written protocol subset, bounded in what it reads
//! and in what it answers. It never listens and never blocks the server loop: one tool thread
//! per channel calls `McpServer::handle`.

mod api;
mod audit;
mod calls;
mod jsonrpc;
mod tools;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use combine_protocol::JsonRpcMessage;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::core::{PanelApiError, ResolvedCaller};
use jsonrpc::{
    clip, echoable_id, error_reply, nests_deeper_than, result_reply, salvage_id, Envelope,
    INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, NAME_TEXT_MAX, SERDE_TEXT_MAX,
};

pub use api::PanelApi;
pub use audit::{McpAudit, StderrAudit};

/// The most bytes of one JSON-RPC message: the frame cap.
pub const MCP_MESSAGE_MAX_BYTES: usize = 1 << 20;
/// The deepest nesting of objects and arrays a message may have, its own object counting as 1.
pub const MCP_MAX_DEPTH: usize = 64;
/// The most id text a reply echoes.
pub const MCP_ID_MAX_BYTES: usize = 1_024;
pub const TOOL_NAMES: [&str; 6] = [
    "create_panel",
    "update_panel",
    "show_diagram",
    "panel_status",
    "list_panels",
    "remove_panel",
];
/// The MCP protocol versions the server answers to, newest first.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Combine shows Mermaid diagrams and Markdown notes as panels next to this terminal. Create one with show_diagram or create_panel, read its validation result, and fix the same panel with update_panel until it is validated.";

/// How long a create, update or show_diagram call waits for a verdict, and how often it asks.
#[derive(Debug, Clone, Copy)]
pub struct McpOptions {
    pub wait: Duration,
    pub poll: Duration,
}

/// The MCP server of one channel: it answers for one caller's space.
pub struct McpServer {
    caller: ResolvedCaller,
    options: McpOptions,
    audit: Arc<dyn McpAudit>,
}

#[derive(Deserialize, Default)]
struct InitializeParams {
    #[serde(rename = "protocolVersion", default)]
    protocol_version: Option<String>,
}

#[derive(Deserialize)]
struct CallParams<'a> {
    #[serde(default)]
    name: Option<String>,
    #[serde(borrow, default)]
    arguments: Option<&'a RawValue>,
}

impl McpServer {
    pub fn new(caller: ResolvedCaller, options: McpOptions, audit: Arc<dyn McpAudit>) -> McpServer {
        McpServer {
            caller,
            options,
            audit,
        }
    }

    /// Answers one JSON-RPC message: `None` for a notification, a response, or a call
    /// abandoned because `closed` was set (its connection is gone).
    pub fn handle(
        &self,
        message: &str,
        api: &dyn PanelApi,
        closed: &AtomicBool,
    ) -> Option<JsonRpcMessage> {
        if closed.load(Ordering::SeqCst) {
            return None;
        }
        if message.len() > MCP_MESSAGE_MAX_BYTES {
            return Some(error_reply(
                None,
                INVALID_REQUEST,
                &format!(
                    "Invalid Request: the message is larger than {MCP_MESSAGE_MAX_BYTES} bytes"
                ),
            ));
        }
        if nests_deeper_than(message, MCP_MAX_DEPTH) {
            return Some(error_reply(
                salvage_id(message),
                INVALID_REQUEST,
                &format!("Invalid Request: the message nests deeper than {MCP_MAX_DEPTH} levels"),
            ));
        }
        let envelope: Envelope<'_> = match serde_json::from_str(message) {
            Ok(envelope) => envelope,
            Err(error) => {
                return Some(error_reply(
                    salvage_id(message),
                    INVALID_REQUEST,
                    &format!(
                        "Invalid Request: {}",
                        clip(&error.to_string(), SERDE_TEXT_MAX)
                    ),
                ));
            }
        };
        if envelope.method.is_none() && (envelope.result.is_some() || envelope.error.is_some()) {
            return None;
        }
        let Some(raw_id) = envelope.id else {
            return match envelope.method {
                Some(_) => None,
                None => Some(error_reply(
                    None,
                    INVALID_REQUEST,
                    "Invalid Request: a request needs a method",
                )),
            };
        };
        let id = match echoable_id(raw_id) {
            Ok(id) => id,
            Err(refusal) => {
                return Some(error_reply(None, INVALID_REQUEST, &refusal.message()));
            }
        };
        if envelope.jsonrpc.as_deref() != Some("2.0") {
            return Some(error_reply(
                Some(id),
                INVALID_REQUEST,
                r#"Invalid Request: jsonrpc must be "2.0""#,
            ));
        }
        let Some(method) = envelope.method.as_deref() else {
            return Some(error_reply(
                Some(id),
                INVALID_REQUEST,
                "Invalid Request: a request needs a method",
            ));
        };
        match method {
            "initialize" => {
                let params: InitializeParams = envelope
                    .params
                    .and_then(|params| serde_json::from_str(params.get()).ok())
                    .unwrap_or_default();
                Some(result_reply(Some(id), &initialize_result(&params)))
            }
            "ping" => Some(result_reply(Some(id), "{}")),
            "tools/list" => Some(result_reply(Some(id), &tools::list_result())),
            "tools/call" => self.tools_call(id, envelope.params, api, closed),
            other => Some(error_reply(
                Some(id),
                METHOD_NOT_FOUND,
                &format!("Method not found: {}", clip(other, NAME_TEXT_MAX)),
            )),
        }
    }

    fn tools_call(
        &self,
        id: &str,
        params: Option<&RawValue>,
        api: &dyn PanelApi,
        closed: &AtomicBool,
    ) -> Option<JsonRpcMessage> {
        const NEEDS_NAME: &str = "Invalid params: params must be an object with a name";
        let Some(params) = params.filter(|p| p.get().trim_start().starts_with('{')) else {
            return Some(error_reply(Some(id), INVALID_PARAMS, NEEDS_NAME));
        };
        let call: CallParams<'_> = match serde_json::from_str(params.get()) {
            Ok(call) => call,
            Err(error) => {
                return Some(error_reply(
                    Some(id),
                    INVALID_PARAMS,
                    &format!(
                        "Invalid params: {}",
                        clip(&error.to_string(), SERDE_TEXT_MAX)
                    ),
                ));
            }
        };
        let Some(name) = call.name else {
            return Some(error_reply(
                Some(id),
                INVALID_PARAMS,
                "Invalid params: name is required",
            ));
        };
        let Some(tool) = calls::Tool::from_name(&name) else {
            return Some(error_reply(
                Some(id),
                INVALID_PARAMS,
                &format!("Unknown tool: {}", clip(&name, NAME_TEXT_MAX)),
            ));
        };
        let result = self.run_tool(tool, call.arguments, api, closed, id)?;
        Some(result_reply(Some(id), &result))
    }
}

fn initialize_result(params: &InitializeParams) -> String {
    let version = params
        .protocol_version
        .as_deref()
        .filter(|requested| PROTOCOL_VERSIONS.contains(requested))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    format!(
        r#"{{"protocolVersion":"{version}","capabilities":{{"tools":{{"listChanged":false}}}},"serverInfo":{{"name":"combine","version":"{}"}},"instructions":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        jsonrpc::quoted(INSTRUCTIONS)
    )
}

/// The tool error code of a panel API error. The match is exhaustive, so a new variant
/// can't go unmapped.
pub fn tool_error_code(error: &PanelApiError) -> &'static str {
    match error {
        PanelApiError::UnknownSpace => "unknown_space",
        PanelApiError::SpaceLoading => "space_loading",
        PanelApiError::UnknownPanel { .. } => "unknown_panel",
        PanelApiError::Removed { .. } => "removed",
        PanelApiError::ReadOnly => "read_only",
        PanelApiError::InvalidId { .. } => "invalid_id",
        PanelApiError::IdTaken { .. } => "id_taken",
        PanelApiError::InvalidField { .. } => "invalid_field",
        PanelApiError::PanelLimit { .. } => "panel_limit",
        PanelApiError::TooLarge { .. } => "frame_too_large",
        PanelApiError::NotScratch { .. } => "not_scratch",
        PanelApiError::ListingFailed { .. } => "listing_failed",
        PanelApiError::ScratchListingFailed { .. } => "scratch_listing_failed",
        PanelApiError::PanelFileInvalid { .. } => "panel_file_invalid",
        PanelApiError::Unreadable { .. } => "unreadable",
        PanelApiError::Stopped => "stopped",
    }
}
