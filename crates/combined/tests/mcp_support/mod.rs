#![cfg(feature = "test-support")]
#![allow(dead_code)]
//! Test support for the MCP channel: a wire client that opens the channel and speaks JSON-RPC
//! through `mcp` frames.

use std::time::{Duration, Instant};

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorMessage, JsonRpcMessage, Mcp, McpCaller, McpOpen, RequestId,
};
use combined::rpc::ServeOptions;
use serde_json::{json, Value};

use crate::daemon_support::WireClient;
use crate::panel_support::PanelServer;

/// The options the MCP wire tests serve with: a short wait, so every tool reply is well inside
/// the client's receive timeout.
pub fn wire_options() -> ServeOptions {
    ServeOptions {
        mcp_wait: Duration::from_millis(200),
        mcp_poll: Duration::from_millis(10),
        ..ServeOptions::default()
    }
}

/// A caller that names the server's project folder as its working directory.
pub fn cwd_caller(server: &PanelServer) -> McpCaller {
    McpCaller {
        terminal_id: None,
        terminal_socket: None,
        cwd: Some(server.space.0.clone()),
    }
}

/// What a tool call answered: whether it is an error, and the JSON the text carries.
#[derive(Debug)]
pub struct ToolResult {
    pub is_error: bool,
    pub body: Value,
}

impl ToolResult {
    /// The error's code, for an error result.
    pub fn code(&self) -> &str {
        assert!(self.is_error, "not an error: {}", self.body);
        self.body["error"]["code"].as_str().unwrap()
    }
}

/// A connection whose MCP channel is open.
pub struct McpWire {
    client: WireClient,
    next_request: u64,
    next_id: u64,
    pub space: String,
}

impl McpWire {
    /// Handshakes and opens the channel, trying again while the server has not yet listed the
    /// caller's space: a terminal that just started is listed a moment later.
    pub fn open(server: &PanelServer, caller: McpCaller) -> McpWire {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (client, _) = WireClient::open(&server.socket);
        let mut wire = McpWire {
            client,
            next_request: 0,
            next_id: 0,
            space: String::new(),
        };
        loop {
            match wire.try_open(caller.clone()) {
                Ok(space) => {
                    wire.space = space;
                    return wire;
                }
                Err(error) if Instant::now() < deadline => {
                    assert_eq!(error.message, "no terminal or space matches the caller");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("the channel never opened: {error:?}"),
            }
        }
    }

    /// Wraps a handshaken client without opening its channel.
    pub fn unopened(client: WireClient) -> McpWire {
        McpWire {
            client,
            next_request: 0,
            next_id: 0,
            space: String::new(),
        }
    }

    /// One `mcp_open`: the space it named, or the error that refused it.
    pub fn try_open(&mut self, caller: McpCaller) -> Result<String, ErrorMessage> {
        self.next_request += 1;
        let request = RequestId(self.next_request);
        self.client
            .send(&ClientMessage::McpOpen(McpOpen { request, caller }));
        match self.client.recv() {
            DaemonMessage::McpOpened(opened) => {
                assert_eq!(opened.request, request);
                Ok(opened.space)
            }
            DaemonMessage::Error(error) => {
                assert_eq!(error.request, Some(request));
                Err(error)
            }
            other => panic!("expected mcp_opened or an error, got {other:?}"),
        }
    }

    /// Sends one JSON-RPC message.
    pub fn send(&mut self, message: &Value) {
        let message = JsonRpcMessage::new(message.to_string()).unwrap();
        self.client.send(&ClientMessage::Mcp(Mcp { message }));
    }

    /// Sends a `tools/call` without waiting for its reply, and returns the request's id.
    pub fn send_call(&mut self, tool: &str, arguments: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        }));
        id
    }

    /// The next JSON-RPC message the daemon sent.
    pub fn recv(&self) -> Value {
        match self.client.recv() {
            DaemonMessage::Mcp(mcp) => serde_json::from_str(mcp.message.as_str()).unwrap(),
            other => panic!("expected an mcp frame, got {other:?}"),
        }
    }

    /// The next JSON-RPC message, or `None` when none comes within `wait`.
    pub fn recv_within(&self, wait: Duration) -> Option<Value> {
        match self.client.recv_within(wait)? {
            DaemonMessage::Mcp(mcp) => Some(serde_json::from_str(mcp.message.as_str()).unwrap()),
            other => panic!("expected an mcp frame, got {other:?}"),
        }
    }

    /// Sends a JSON-RPC request with a new id and returns its reply.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let reply = self.recv();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// Calls a tool and returns its result, with the text parsed.
    pub fn tool(&mut self, tool: &str, arguments: Value) -> ToolResult {
        let id = self.send_call(tool, arguments);
        let reply = self.recv();
        assert_eq!(reply["id"], id, "{reply}");
        tool_result(&reply)
    }

    /// Waits until the caller's space has read its panels: before that, `list_panels` answers
    /// `space_loading`.
    pub fn wait_loaded(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let listed = self.tool("list_panels", json!({}));
            if !listed.is_error {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the space never loaded: {listed:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The connection, for a test that closes it or reads other frames.
    pub fn into_client(self) -> WireClient {
        self.client
    }
}

/// The result of a `tools/call` reply.
pub fn tool_result(reply: &Value) -> ToolResult {
    let result = &reply["result"];
    assert!(result.is_object(), "not a tool result: {reply}");
    let text = result["content"][0]["text"].as_str().unwrap();
    ToolResult {
        is_error: result["isError"].as_bool().unwrap(),
        body: serde_json::from_str(text).unwrap(),
    }
}
