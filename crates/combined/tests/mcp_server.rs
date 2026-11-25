#![cfg(feature = "test-support")]
//! The MCP server's JSON-RPC bounds, its six tools, the wait, the remove read-back and the
//! audit line, over a fake panel API and a recording audit.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use combine_protocol::{
    CreatedBy, DaemonMessage, Mcp, PanelError, PanelState, RenderStatus, Renderer, SkippedPanel,
    WireMessage,
};
use combined::core::{
    CallerMatch, ClientRender, PanelApiError, PanelCreate, PanelList, PanelUpdate, PanelView,
    ResolvedCaller,
};
use combined::mcp_server::{
    tool_error_code, McpAudit, McpOptions, McpServer, PanelApi, MCP_MAX_DEPTH, PROTOCOL_VERSIONS,
    TOOL_NAMES,
};
use combined::model::{Lifetime, SpaceId, ViewPanelFile};
use serde_json::Value;

type Status = Box<dyn Fn() -> Result<PanelView, PanelApiError>>;

struct Fake {
    create: Result<PanelView, PanelApiError>,
    update: Result<PanelView, PanelApiError>,
    remove: Result<PanelView, PanelApiError>,
    status: Status,
    list: Result<PanelList, PanelApiError>,
    renders: Vec<ClientRender>,
    calls: Mutex<Vec<&'static str>>,
    created: Mutex<Vec<PanelCreate>>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            create: Ok(view("p", 1, PanelState::Validated, Lifetime::Persistent)),
            update: Ok(view("p", 2, PanelState::Validated, Lifetime::Persistent)),
            remove: Ok(view("p", 3, PanelState::Removed, Lifetime::Persistent)),
            status: Box::new(|| Ok(view("p", 1, PanelState::Validated, Lifetime::Persistent))),
            list: Ok(PanelList {
                panels: Vec::new(),
                skipped: Vec::new(),
                error: None,
            }),
            renders: Vec::new(),
            calls: Mutex::new(Vec::new()),
            created: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn note(&self, call: &'static str) {
        self.calls.lock().unwrap().push(call);
    }
}

impl PanelApi for Fake {
    fn create(&self, create: PanelCreate) -> Result<PanelView, PanelApiError> {
        self.note("create");
        self.created.lock().unwrap().push(create);
        self.create.clone()
    }
    fn update(&self, _: &str, _: PanelUpdate) -> Result<PanelView, PanelApiError> {
        self.note("update");
        self.update.clone()
    }
    fn remove(&self, _: &str) -> Result<PanelView, PanelApiError> {
        self.note("remove");
        self.remove.clone()
    }
    fn status(&self, _: &str) -> Result<PanelView, PanelApiError> {
        self.note("status");
        (self.status)()
    }
    fn list(&self) -> Result<PanelList, PanelApiError> {
        self.note("list");
        self.list.clone()
    }
    fn render_status(&self, _: &str) -> Result<Vec<ClientRender>, PanelApiError> {
        self.note("render_status");
        Ok(self.renders.clone())
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl McpAudit for Recorder {
    fn record(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

impl Recorder {
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

fn view(id: &str, revision: u64, state: PanelState, lifetime: Lifetime) -> PanelView {
    PanelView {
        space: SpaceId("/work".to_owned()),
        id: id.to_owned(),
        lifetime,
        file: ViewPanelFile {
            renderer: Renderer::Mermaid,
            title: None,
            idempotency_key: None,
            created_by: None,
            placement: None,
        },
        revision,
        state,
        error: None,
        file_error: None,
        dropped: None,
        read_only: false,
    }
}

fn server_with(wait_ms: u64) -> (McpServer, Arc<Recorder>) {
    let audit = Arc::new(Recorder::default());
    let caller = ResolvedCaller {
        space: SpaceId("/work".to_owned()),
        slot: None,
        by: CallerMatch::Terminal,
    };
    let options = McpOptions {
        wait: Duration::from_millis(wait_ms),
        poll: Duration::from_millis(10),
    };
    (McpServer::new(caller, options, audit.clone()), audit)
}

fn server() -> (McpServer, Arc<Recorder>) {
    server_with(100)
}

fn send(server: &McpServer, api: &Fake, message: &str) -> Option<String> {
    let closed = AtomicBool::new(false);
    server
        .handle(message, api, &closed)
        .map(|reply| reply.as_str().to_owned())
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn request(id: &str, method: &str, params: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
}

fn call_message(name: &str, arguments: &str) -> String {
    request(
        "1",
        "tools/call",
        &format!(r#"{{"name":"{name}","arguments":{arguments}}}"#),
    )
}

/// Calls a tool and returns the parsed text of its result and the isError flag.
fn tool(server: &McpServer, api: &Fake, name: &str, arguments: &str) -> (Value, bool) {
    let reply = send(server, api, &call_message(name, arguments)).expect("a reply");
    let reply = json(&reply);
    let result = &reply["result"];
    let text = result["content"][0]["text"].as_str().expect("text content");
    (json(text), result["isError"].as_bool().unwrap())
}

fn error_of(reply: &str) -> (i64, Value) {
    let reply = json(reply);
    (
        reply["error"]["code"].as_i64().unwrap(),
        reply["id"].clone(),
    )
}

fn nested(levels: usize) -> String {
    format!("{}{}", "[".repeat(levels), "]".repeat(levels))
}

fn fits_one_frame(reply: &str) -> bool {
    let message = combine_protocol::JsonRpcMessage::new(reply.to_owned()).unwrap();
    DaemonMessage::Mcp(Mcp { message }).encode().is_ok()
}

#[test]
fn tools_list_is_the_six_tools_in_order_with_closed_schemas() {
    let (server, _) = server();
    let reply = send(&server, &Fake::new(), &request("1", "tools/list", "{}")).unwrap();
    let tools = json(&reply)["result"]["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOL_NAMES);
    for tool in &tools {
        let mut keys: Vec<&String> = tool.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["description", "inputSchema", "name"]);
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
}

#[test]
fn descriptions_state_the_rules() {
    let (server, _) = server();
    let reply = send(&server, &Fake::new(), &request("1", "tools/list", "{}")).unwrap();
    let tools = json(&reply)["result"]["tools"].as_array().unwrap().clone();
    let description = |name: &str| -> String {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap();
        tool["description"].as_str().unwrap().to_owned()
    };
    let create = description("create_panel");
    for needle in [
        "Mermaid 12.1.0",
        "204,800",
        "idempotency_key",
        "placement",
        "M3",
        "M4",
    ] {
        assert!(create.contains(needle), "{needle}");
    }
    assert!(description("update_panel").contains("204,800"));
    assert!(description("show_diagram").contains("Mermaid 12.1.0"));
    assert!(description("remove_panel").contains("remove_conflict"));
}

#[test]
fn a_create_answers_validating_after_the_wait() {
    let (server, _) = server_with(200);
    let mut api = Fake::new();
    api.create = Ok(view("p", 1, PanelState::Validating, Lifetime::Persistent));
    api.status = Box::new(|| Ok(view("p", 1, PanelState::Validating, Lifetime::Persistent)));
    let started = Instant::now();
    let (result, is_error) = tool(
        &server,
        &api,
        "create_panel",
        r#"{"renderer":"mermaid","content":"graph TD"}"#,
    );
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(!is_error);
    assert_eq!(result["state"], "validating");
    assert_eq!(result["superseded"], false);
}

#[test]
fn a_create_returns_the_panel_result_in_order_and_passes_placement_and_creator() {
    let (server, _) = server();
    let api = Fake::new();
    let reply = send(
        &server,
        &api,
        &call_message(
            "create_panel",
            r#"{"renderer":"markdown","content":"hi","placement":"main","id":"p"}"#,
        ),
    )
    .unwrap();
    let text = json(&reply)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let fields = [
        "panel",
        "revision",
        "state",
        "error",
        "renderer",
        "lifetime",
        "title",
        "placement",
        "created_by",
        "file_error",
        "read_only",
        "superseded",
    ];
    let positions: Vec<usize> = fields
        .iter()
        .map(|field| text.find(&format!("\"{field}\":")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    let created = api.created.lock().unwrap();
    assert_eq!(created[0].placement.as_deref(), Some("main"));
    assert_eq!(created[0].renderer, Renderer::Markdown);
    assert_eq!(created[0].lifetime, Lifetime::Persistent);
    assert_eq!(created[0].created_by, Some(CreatedBy::Agent { slot: None }));
}

#[test]
fn show_diagram_makes_a_scratch_mermaid_panel() {
    let (server, _) = server();
    let api = Fake::new();
    tool(&server, &api, "show_diagram", r#"{"source":"graph TD"}"#);
    let created = api.created.lock().unwrap();
    assert_eq!(created[0].lifetime, Lifetime::Scratch);
    assert_eq!(created[0].renderer, Renderer::Mermaid);
}

#[test]
fn create_refuses_later_milestones_and_terminals() {
    let (server, audit) = server();
    let api = Fake::new();
    for (field, milestone) in [
        ("kind", "M4"),
        ("group", "M4"),
        ("loader", "M3"),
        ("sources", "M3"),
        ("refresh", "M3"),
        ("params", "M3"),
    ] {
        let arguments = format!(r#"{{"renderer":"mermaid","content":"x","{field}":{{}}}}"#);
        let (result, is_error) = tool(&server, &api, "create_panel", &arguments);
        assert!(is_error);
        assert_eq!(result["error"]["code"], "not_supported");
        assert!(result["error"]["message"]
            .as_str()
            .unwrap()
            .contains(milestone));
    }
    let (result, _) = tool(
        &server,
        &api,
        "create_panel",
        r#"{"renderer":"terminal","content":"x"}"#,
    );
    assert_eq!(result["error"]["code"], "not_supported");
    assert!(api.calls().is_empty());
    assert_eq!(audit.lines().len(), 7);
}

#[test]
fn a_refused_call_clips_a_long_id_in_its_audit_line() {
    let (server, audit) = server();
    let api = Fake::new();
    let id = format!("{}\u{e9}", "a".repeat(254));
    let arguments = format!(r#"{{"renderer":"terminal","content":"x","id":"{id}"}}"#);
    let (result, is_error) = tool(&server, &api, "create_panel", &arguments);
    assert!(is_error);
    assert_eq!(result["error"]["code"], "not_supported");
    let long = "b".repeat(100_000);
    let arguments = format!(r#"{{"renderer":"terminal","content":"x","id":"{long}"}}"#);
    tool(&server, &api, "create_panel", &arguments);
    let lines = audit.lines();
    assert_eq!(lines.len(), 2);
    let expected = format!(r#"panel="{}" error=not_supported"#, "a".repeat(254));
    assert!(lines[0].contains(&expected), "{}", lines[0]);
    let clipped = format!(r#"panel="{}" error=not_supported"#, "b".repeat(255));
    assert!(lines[1].ends_with(&clipped), "{}", lines[1].len());
}

#[test]
fn argument_problems_are_invalid_arguments() {
    let (server, _) = server();
    let api = Fake::new();
    for (tool_name, arguments) in [
        ("create_panel", r#"{"content":"x"}"#),
        ("create_panel", r#"{"renderer":"svg","content":"x"}"#),
        ("create_panel", r#"{"renderer":"mermaid"}"#),
        (
            "create_panel",
            r#"{"renderer":"mermaid","content":"x","lifetime":"forever"}"#,
        ),
        (
            "create_panel",
            r#"{"renderer":"mermaid","content":"x","bogus":1}"#,
        ),
        ("create_panel", r#"{"renderer":"mermaid","content":5}"#),
        ("create_panel", "[1]"),
        ("show_diagram", "{}"),
        ("update_panel", "{}"),
        ("panel_status", "{}"),
        ("remove_panel", "{}"),
        ("list_panels", r#"{"x":1}"#),
    ] {
        let (result, is_error) = tool(&server, &api, tool_name, arguments);
        assert!(is_error, "{tool_name} {arguments}");
        assert_eq!(
            result["error"]["code"], "invalid_arguments",
            "{tool_name} {arguments}"
        );
    }
    assert!(api.calls().is_empty());
}

#[test]
fn an_update_can_clear_the_title_with_null() {
    let (server, _) = server();
    let api = Fake::new();
    let (result, is_error) = tool(
        &server,
        &api,
        "update_panel",
        r#"{"panel":"p","title":null}"#,
    );
    assert!(!is_error);
    assert_eq!(result["panel"], "p");
}

#[test]
fn panel_status_ends_with_render_status() {
    let (server, _) = server();
    let mut api = Fake::new();
    api.renders = vec![
        ClientRender {
            revision: 1,
            status: RenderStatus::Rendered,
            error: None,
        },
        ClientRender {
            revision: 1,
            status: RenderStatus::RenderFailed,
            error: Some(PanelError {
                code: "render_error".to_owned(),
                message: "bad".to_owned(),
                line: Some(2),
                column: None,
                snippet: None,
            }),
        },
    ];
    let reply = send(
        &server,
        &api,
        &call_message("panel_status", r#"{"panel":"p"}"#),
    )
    .unwrap();
    let text = json(&reply)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let superseded = text.find("\"superseded\":").unwrap();
    let render = text.find("\"render_status\":").unwrap();
    assert!(superseded < render, "{text}");
    assert!(text.ends_with("}]}"), "{text}");
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["render_status"][0]["status"], "rendered");
    assert_eq!(result["render_status"][0]["error"], Value::Null);
    assert_eq!(result["render_status"][1]["revision"], 1);
    assert_eq!(result["render_status"][1]["status"], "render_failed");
    assert_eq!(result["render_status"][1]["error"]["code"], "render_error");

    let mut none = Fake::new();
    none.renders = Vec::new();
    let (result, _) = tool(&server, &none, "panel_status", r#"{"panel":"p"}"#);
    assert_eq!(result["render_status"], serde_json::json!([]));

    let (result, _) = tool(&server, &api, "list_panels", "{}");
    assert!(result.get("render_status").is_none());
    let (result, _) = tool(&server, &api, "remove_panel", r#"{"panel":"p"}"#);
    assert!(result.get("render_status").is_none());
    let asked = api
        .calls()
        .iter()
        .filter(|c| **c == "render_status")
        .count();
    assert_eq!(asked, 1, "only panel_status reads the render status");
}

#[test]
fn panel_status_reports_superseded_and_unknown_revisions() {
    let (server, audit) = server();
    let mut api = Fake::new();
    api.status = Box::new(|| Ok(view("p", 5, PanelState::Validated, Lifetime::Persistent)));
    let (result, is_error) = tool(
        &server,
        &api,
        "panel_status",
        r#"{"panel":"p","revision":3}"#,
    );
    assert!(!is_error);
    assert_eq!(result["superseded"], true);
    let (result, _) = tool(
        &server,
        &api,
        "panel_status",
        r#"{"panel":"p","revision":5}"#,
    );
    assert_eq!(result["superseded"], false);
    let (result, is_error) = tool(
        &server,
        &api,
        "panel_status",
        r#"{"panel":"p","revision":6}"#,
    );
    assert!(is_error);
    assert_eq!(result["error"]["code"], "unknown_revision");
    assert_eq!(
        result["error"]["message"],
        "the panel p has no revision 6; its current revision is 5"
    );
    assert!(audit.lines().is_empty());
}

#[test]
fn list_panels_sorts_by_id_and_counts() {
    let (server, _) = server();
    let mut api = Fake::new();
    api.list = Ok(PanelList {
        panels: vec![
            view("b", 1, PanelState::Validated, Lifetime::Persistent),
            view("a", 1, PanelState::Failed, Lifetime::Scratch),
        ],
        skipped: vec![SkippedPanel {
            panel: "x".to_owned(),
            reason: "empty".to_owned(),
        }],
        error: None,
    });
    let (result, _) = tool(&server, &api, "list_panels", "{}");
    assert_eq!(result["space"], "/work");
    assert_eq!(result["panels"][0]["panel"], "a");
    assert_eq!(result["panels"][1]["panel"], "b");
    assert_eq!(result["count"], 2);
    assert_eq!(result["truncated"], false);
    assert_eq!(result["skipped"][0]["reason"], "empty");
}

#[test]
fn deep_nesting_is_an_invalid_request_with_the_id() {
    let (server, _) = server();
    let message = format!(
        r#"{{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{{"name":"create_panel","arguments":{{"x":{}}}}}}}"#,
        nested(100_000)
    );
    let reply = send(&server, &Fake::new(), &message).unwrap();
    assert_eq!(error_of(&reply), (-32600, Value::from(9)));
}

#[test]
fn nesting_of_64_is_accepted_and_65_refused() {
    let (server, _) = server();
    let api = Fake::new();
    let ping = |total: usize| {
        let arrays = total - 2;
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"a":{}}}}}"#,
            nested(arrays)
        )
    };
    assert_eq!(MCP_MAX_DEPTH, 64);
    let accepted = send(&server, &api, &ping(64)).unwrap();
    assert_eq!(json(&accepted)["result"], Value::Object(Default::default()));
    let refused = send(&server, &api, &ping(65)).unwrap();
    assert_eq!(error_of(&refused), (-32600, Value::from(1)));
}

#[test]
fn a_message_over_1_mib_is_refused() {
    let (server, _) = server();
    let padding = "a".repeat(1 << 20);
    let message = request("1", "ping", &format!(r#"{{"p":"{padding}"}}"#));
    let reply = send(&server, &Fake::new(), &message).unwrap();
    assert_eq!(error_of(&reply), (-32600, Value::Null));
}

#[test]
fn malformed_envelopes_get_json_rpc_errors() {
    let (server, _) = server();
    let api = Fake::new();
    let table = [
        (
            r#"{"jsonrpc":"1.0","id":4,"method":"ping"}"#,
            Value::from(4),
        ),
        (r#"{"jsonrpc":"2.0","id":4,"method":5}"#, Value::from(4)),
        (r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#, Value::Null),
        (
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            Value::Null,
        ),
        (
            r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
            Value::Null,
        ),
        (r#"{"jsonrpc":"2.0","id":4}"#, Value::from(4)),
        (r#"{"jsonrpc":"2.0"}"#, Value::Null),
        (
            r#"{"jsonrpc":2,"id":"a","method":"ping"}"#,
            Value::from("a"),
        ),
    ];
    for (message, id) in table {
        let reply = send(&server, &api, message).unwrap_or_else(|| panic!("{message}"));
        assert_eq!(error_of(&reply), (-32600, id), "{message}");
    }
}

#[test]
fn an_id_of_1024_bytes_is_echoed_and_1025_is_refused_with_a_null_id() {
    let (server, _) = server();
    let api = Fake::new();
    let id = |len: usize| format!("\"{}\"", "i".repeat(len - 2));
    let echoed = send(&server, &api, &request(&id(1024), "ping", "{}")).unwrap();
    assert_eq!(json(&echoed)["id"], json(&id(1024)));
    let refused = send(&server, &api, &request(&id(1025), "ping", "{}")).unwrap();
    assert_eq!(error_of(&refused), (-32600, Value::Null));
}

#[test]
fn a_near_1_mib_id_gets_one_invalid_request_with_a_null_id_that_fits() {
    let (server, _) = server();
    let id = format!("\"{}\"", "i".repeat(1_048_000));
    let reply = send(&server, &Fake::new(), &request(&id, "ping", "{}")).unwrap();
    assert_eq!(error_of(&reply), (-32600, Value::Null));
    assert!(fits_one_frame(&reply));
}

#[test]
fn notifications_never_get_a_reply() {
    let (server, audit) = server();
    let api = Fake::new();
    for message in [
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"1.0","method":"x"}"#,
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"remove_panel","arguments":{"panel":"p"}}}"#,
        r#"{"method":5}"#,
    ] {
        let reply = send(&server, &api, message);
        // A method that isn't a string is an envelope error, which has no id to answer.
        if message == r#"{"method":5}"# {
            assert_eq!(error_of(&reply.unwrap()), (-32600, Value::Null));
        } else {
            assert!(reply.is_none(), "{message}");
        }
    }
    assert!(api.calls().is_empty());
    assert!(audit.lines().is_empty());
}

#[test]
fn client_responses_are_ignored() {
    let (server, _) = server();
    let api = Fake::new();
    for message in [
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"no"}}"#,
    ] {
        assert!(send(&server, &api, message).is_none());
    }
}

#[test]
fn unknown_method_is_32601() {
    let (server, _) = server();
    let reply = send(&server, &Fake::new(), &request("2", "resources/list", "{}")).unwrap();
    assert_eq!(error_of(&reply), (-32601, Value::from(2)));
}

#[test]
fn tools_call_param_errors_are_32602() {
    let (server, _) = server();
    let api = Fake::new();
    for params in ["[]", "null", "{}", r#"{"name":"nope"}"#, r#"{"name":5}"#] {
        let reply = send(&server, &api, &request("3", "tools/call", params)).unwrap();
        assert_eq!(error_of(&reply), (-32602, Value::from(3)), "{params}");
    }
}

#[test]
fn initialize_echoes_a_known_version_and_offers_the_latest_otherwise() {
    let (server, _) = server();
    let api = Fake::new();
    for version in PROTOCOL_VERSIONS {
        let params = format!(r#"{{"protocolVersion":"{version}","capabilities":{{}}}}"#);
        let reply = send(&server, &api, &request("1", "initialize", &params)).unwrap();
        assert_eq!(json(&reply)["result"]["protocolVersion"], version);
    }
    let reply = send(
        &server,
        &api,
        &request("1", "initialize", r#"{"protocolVersion":"1999-01-01"}"#),
    )
    .unwrap();
    let result = &json(&reply)["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(result["serverInfo"]["name"], "combine");
}

#[test]
fn ping_answers_empty() {
    let (server, _) = server();
    let reply = send(&server, &Fake::new(), &request("7", "ping", "{}")).unwrap();
    assert_eq!(
        json(&reply),
        json(r#"{"jsonrpc":"2.0","id":7,"result":{}}"#)
    );
}

#[test]
fn a_reply_too_big_for_one_frame_is_32603_with_the_id() {
    let (server, _) = server();
    let mut api = Fake::new();
    api.list = Ok(PanelList {
        panels: Vec::new(),
        skipped: (0..5_000)
            .map(|n| SkippedPanel {
                panel: format!("folder-{n}"),
                reason: "r".repeat(250),
            })
            .collect(),
        error: None,
    });
    let reply = send(&server, &api, &call_message("list_panels", "{}")).unwrap();
    assert_eq!(error_of(&reply), (-32603, Value::from(1)));
    assert!(fits_one_frame(&reply));
}

#[test]
fn list_panels_truncates_to_fit_one_frame() {
    let (server, _) = server();
    let mut api = Fake::new();
    let noise = "\u{1}".repeat(1_024);
    api.list = Ok(PanelList {
        panels: (0..100)
            .map(|n| {
                let mut panel = view(
                    &format!("p{n:03}"),
                    1,
                    PanelState::Failed,
                    Lifetime::Persistent,
                );
                panel.error = Some(PanelError {
                    code: "syntax".to_owned(),
                    message: noise.clone(),
                    line: Some(1),
                    column: Some(1),
                    snippet: Some(noise.clone()),
                });
                panel
            })
            .collect(),
        skipped: Vec::new(),
        error: None,
    });
    let reply = send(&server, &api, &call_message("list_panels", "{}")).unwrap();
    assert!(fits_one_frame(&reply));
    let text = json(&reply)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let list = json(&text);
    assert_eq!(list["truncated"], true);
    assert_eq!(list["count"], 100);
    let kept = list["panels"].as_array().unwrap().len();
    assert!(kept > 0 && kept < 100);
    assert_eq!(list["panels"][0]["panel"], "p000");
}

fn scan(dir: &Path, found: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(&path, found);
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for banned in ["serde_json::Value", "json!", "untagged", "flatten", "tag ="] {
            if text.contains(banned) {
                found.push(format!("{} has {banned}", path.display()));
            }
        }
    }
}

#[test]
fn mcp_server_builds_no_json_tree() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp_server");
    let mut found = Vec::new();
    scan(&dir, &mut found);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn remove_reads_back_and_reports_a_conflict() {
    let (server, audit) = server();
    let mut api = Fake::new();
    api.remove = Ok(view("p", 4, PanelState::Removed, Lifetime::Persistent));
    api.status = Box::new(|| Ok(view("p", 4, PanelState::Validated, Lifetime::Persistent)));
    let (result, is_error) = tool(&server, &api, "remove_panel", r#"{"panel":"p"}"#);
    assert!(is_error);
    assert_eq!(
        result,
        json(
            r#"{"error":{"code":"remove_conflict","message":"the panel's files changed outside Combine, so it wasn't removed"},"panel":{"id":"p","revision":4,"state":"validated"}}"#
        )
    );
    assert_eq!(api.calls(), ["remove", "status"]);
    assert_eq!(
        audit.lines(),
        [r#"mcp remove_panel space="/work" slot=null panel="p" error=remove_conflict"#]
    );
}

#[test]
fn a_clean_remove_reads_back_removed() {
    let (server, audit) = server();
    let mut api = Fake::new();
    api.status = Box::new(|| Ok(view("p", 3, PanelState::Removed, Lifetime::Persistent)));
    let (result, is_error) = tool(&server, &api, "remove_panel", r#"{"panel":"p"}"#);
    assert!(!is_error);
    assert_eq!(result["state"], "removed");
    assert_eq!(api.calls(), ["remove", "status"]);
    assert_eq!(
        audit.lines(),
        [r#"mcp remove_panel space="/work" slot=null panel="p" revision=3 state=removed"#]
    );
}

#[test]
fn a_scratch_remove_is_not_read_back() {
    let (server, _) = server();
    let mut api = Fake::new();
    api.remove = Ok(view("s", 2, PanelState::Removed, Lifetime::Scratch));
    api.status = Box::new(|| Ok(view("s", 2, PanelState::Validated, Lifetime::Persistent)));
    let (result, is_error) = tool(&server, &api, "remove_panel", r#"{"panel":"s"}"#);
    assert!(!is_error);
    assert_eq!(result["state"], "removed");
    assert_eq!(api.calls(), ["remove"]);
}

#[test]
fn a_remove_cut_short_by_the_closed_flag_audits_removed() {
    let (server, audit) = server();
    let api = Fake::new();
    struct SetsClosed<'a>(&'a Fake, &'a AtomicBool);
    impl PanelApi for SetsClosed<'_> {
        fn create(&self, c: PanelCreate) -> Result<PanelView, PanelApiError> {
            self.0.create(c)
        }
        fn update(&self, id: &str, u: PanelUpdate) -> Result<PanelView, PanelApiError> {
            self.0.update(id, u)
        }
        fn remove(&self, id: &str) -> Result<PanelView, PanelApiError> {
            let removed = self.0.remove(id);
            self.1.store(true, Ordering::SeqCst);
            removed
        }
        fn status(&self, id: &str) -> Result<PanelView, PanelApiError> {
            self.0.status(id)
        }
        fn list(&self) -> Result<PanelList, PanelApiError> {
            self.0.list()
        }
        fn render_status(&self, id: &str) -> Result<Vec<ClientRender>, PanelApiError> {
            self.0.render_status(id)
        }
    }
    let closed = AtomicBool::new(false);
    let api_closing = SetsClosed(&api, &closed);
    let reply = server.handle(
        &call_message("remove_panel", r#"{"panel":"p"}"#),
        &api_closing,
        &closed,
    );
    assert!(reply.is_none());
    assert_eq!(api.calls(), ["remove"]);
    assert_eq!(
        audit.lines(),
        [r#"mcp remove_panel space="/work" slot=null panel="p" revision=3 state=removed"#]
    );
}

#[test]
fn every_panel_api_error_maps_to_its_tool_code() {
    let table: [(PanelApiError, &str, &str); 16] = [
        (PanelApiError::UnknownSpace, "unknown_space", "the host lists no such space"),
        (
            PanelApiError::SpaceLoading,
            "space_loading",
            "the space's panels are still being read; try again",
        ),
        (
            PanelApiError::UnknownPanel { id: "p".into() },
            "unknown_panel",
            "the space has no panel p",
        ),
        (PanelApiError::Removed { id: "p".into() }, "removed", "the panel p was removed"),
        (
            PanelApiError::ReadOnly,
            "read_only",
            "the panel's folder is read-only; its files can't be changed",
        ),
        (
            PanelApiError::InvalidId { why: "no".into() },
            "invalid_id",
            "the panel ID isn't valid: no",
        ),
        (
            PanelApiError::IdTaken { id: "p".into() },
            "id_taken",
            "the space already has a panel folder p",
        ),
        (
            PanelApiError::InvalidField { field: "title", rule: "is too long".into() },
            "invalid_field",
            "title is too long",
        ),
        (
            PanelApiError::PanelLimit { count: 100, limit: 100 },
            "panel_limit",
            "the space already has 100 panels; the limit is 100",
        ),
        (
            PanelApiError::TooLarge { encoded: 2_000_000 },
            "frame_too_large",
            "the source can't be sent in one frame: its panel reply would be 2000000 bytes; the limit is 1048576",
        ),
        (
            PanelApiError::NotScratch { id: "p".into() },
            "not_scratch",
            "the panel p is already in .workspace/",
        ),
        (
            PanelApiError::ListingFailed { detail: "boom".into() },
            "listing_failed",
            "the space's panels couldn't be listed: boom",
        ),
        (
            PanelApiError::ScratchListingFailed { detail: "boom".into() },
            "scratch_listing_failed",
            "the scratch panels couldn't be listed: boom",
        ),
        (
            PanelApiError::PanelFileInvalid { reason: "bad".into() },
            "panel_file_invalid",
            "panel.json can't be used (bad), so the daemon doesn't rewrite it",
        ),
        (
            PanelApiError::Unreadable { path: "a".into(), detail: "denied".into() },
            "unreadable",
            "couldn't read a (denied), so the panel isn't changed until it can be read",
        ),
        (PanelApiError::Stopped, "stopped", "the server has stopped"),
    ];
    let (server, _) = server();
    for (error, code, message) in table {
        assert_eq!(tool_error_code(&error), code);
        let mut api = Fake::new();
        api.status = Box::new({
            let error = error.clone();
            move || Err(error.clone())
        });
        let (result, is_error) = tool(&server, &api, "panel_status", r#"{"panel":"p"}"#);
        assert!(is_error);
        assert_eq!(result["error"]["code"], code);
        assert_eq!(result["error"]["message"], message);
        if let PanelApiError::InvalidField { field, .. } = &error {
            assert_eq!(result["error"]["field"], *field);
        } else {
            assert!(result["error"].get("field").is_none());
        }
    }
}

#[test]
fn the_wait_reads_the_panel_again_before_trusting_the_call() {
    let (server, _) = server();
    let mut api = Fake::new();
    api.create = Ok(view("p", 1, PanelState::Validated, Lifetime::Persistent));
    api.status = Box::new(|| Ok(view("p", 1, PanelState::Removed, Lifetime::Persistent)));
    let (result, _) = tool(
        &server,
        &api,
        "create_panel",
        r#"{"renderer":"markdown","content":"x"}"#,
    );
    assert_eq!(result["state"], "removed");
}

#[test]
fn a_newer_revision_is_answered_superseded() {
    let (server, _) = server();
    let mut api = Fake::new();
    api.status = Box::new(|| Ok(view("p", 2, PanelState::Validated, Lifetime::Persistent)));
    let (result, _) = tool(
        &server,
        &api,
        "create_panel",
        r#"{"renderer":"markdown","content":"x"}"#,
    );
    assert_eq!(result["revision"], 2);
    assert_eq!(result["superseded"], true);
}

#[test]
fn a_closed_flag_set_during_a_wait_ends_it_with_no_reply_and_one_audit_line() {
    let (server, audit) = server_with(5_000);
    let mut api = Fake::new();
    api.create = Ok(view("p", 1, PanelState::Validating, Lifetime::Persistent));
    api.status = Box::new(|| Ok(view("p", 1, PanelState::Validating, Lifetime::Persistent)));
    let closed = Arc::new(AtomicBool::new(false));
    let setter = {
        let closed = closed.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            closed.store(true, Ordering::SeqCst);
        })
    };
    let started = Instant::now();
    let message = call_message(
        "create_panel",
        r#"{"renderer":"markdown","content":"SECRET"}"#,
    );
    let reply = server.handle(&message, &api, &closed);
    setter.join().unwrap();
    assert!(reply.is_none());
    // Far under the 5 s wait; the bound is loose because sleeps overshoot on a loaded machine.
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        audit.lines(),
        [r#"mcp create_panel space="/work" slot=null panel="p" revision=1 state=validating"#]
    );
}

#[test]
fn a_closed_flag_set_before_a_call_runs_nothing() {
    let (server, audit) = server();
    let api = Fake::new();
    let closed = AtomicBool::new(true);
    let message = call_message("create_panel", r#"{"renderer":"markdown","content":"x"}"#);
    assert!(server.handle(&message, &api, &closed).is_none());
    assert!(api.calls().is_empty());
    assert!(audit.lines().is_empty());
}

#[test]
fn audit_lines_hold_no_arguments_and_only_mutating_calls_write_them() {
    let (server, audit) = server();
    let mut api = Fake::new();
    tool(
        &server,
        &api,
        "create_panel",
        r#"{"renderer":"markdown","content":"SECRET","title":"SECRET","id":"p"}"#,
    );
    tool(
        &server,
        &api,
        "update_panel",
        r#"{"panel":"p","content":"SECRET"}"#,
    );
    tool(&server, &api, "show_diagram", r#"{"source":"SECRET"}"#);
    tool(
        &server,
        &api,
        "create_panel",
        r#"{"renderer":"mermaid","id":"q"}"#,
    );
    api.update = Err(PanelApiError::ReadOnly);
    tool(
        &server,
        &api,
        "update_panel",
        r#"{"panel":"p","content":"SECRET"}"#,
    );
    tool(&server, &api, "panel_status", r#"{"panel":"p"}"#);
    tool(&server, &api, "list_panels", "{}");
    send(&server, &api, &request("1", "ping", "{}"));
    send(&server, &api, &request("1", "nope", "{}"));
    send(&server, &api, &call_message("nope", "{}"));
    let lines = audit.lines();
    assert_eq!(
        lines,
        [
            r#"mcp create_panel space="/work" slot=null panel="p" revision=1 state=validated"#,
            r#"mcp update_panel space="/work" slot=null panel="p" revision=1 state=validated"#,
            r#"mcp show_diagram space="/work" slot=null panel="p" revision=1 state=validated"#,
            r#"mcp create_panel space="/work" slot=null panel="q" error=invalid_arguments"#,
            r#"mcp update_panel space="/work" slot=null panel="p" error=read_only"#,
        ]
    );
    assert!(lines.iter().all(|line| !line.contains("SECRET")));
}
