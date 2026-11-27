#![allow(dead_code)]

use combine_protocol::*;

pub const DISPLAY_JSON: &str = r##"{"foreground":"#ddeeff","background":"#112233","cursor_color":"#aabbcc","cell_width":9,"cell_height":18}"##;

pub const SPACE: &str = "/Users/me/src/trellis";
pub const MCP_CLIENT_MESSAGE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_diagram","arguments":{"source":"classDiagram\n  Animal <|-- Duck\n"}}}"#;
pub const MCP_DAEMON_MESSAGE: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"classes: validated at revision 7"}],"isError":false}}"#;
pub const CLASS_SOURCE: &str = "classDiagram\n  Animal <|-- Duck\n";

pub fn s(text: &str) -> String {
    text.to_owned()
}

pub fn json_rpc(text: &str) -> JsonRpcMessage {
    JsonRpcMessage::new(text.to_owned()).unwrap()
}

/// Parses `json`, applies `edit` to its top-level object, and writes it back.
pub fn edited(
    json: &str,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> String {
    let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
    edit(value.as_object_mut().unwrap());
    serde_json::to_string(&value).unwrap()
}

/// The JSON text of the golden `key`, without its frame header.
pub fn golden_json(key: &str) -> String {
    let frame = client_goldens()
        .into_iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, _, frame)| frame)
        .or_else(|| {
            daemon_goldens()
                .into_iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, _, frame)| frame)
        })
        .expect("golden exists");
    String::from_utf8(frame[5..].to_vec()).unwrap()
}

/// Sets the value at the JSON pointer `pointer` in `json`, or removes the key when `value` is
/// `None`. The pointer's last segment is an object key.
pub fn patched(json: &str, pointer: &str, value: Option<serde_json::Value>) -> String {
    let mut root: serde_json::Value = serde_json::from_str(json).unwrap();
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    let target = if parent.is_empty() {
        &mut root
    } else {
        root.pointer_mut(parent).unwrap()
    };
    let object = target.as_object_mut().unwrap();
    match value {
        Some(value) => object.insert(key.to_owned(), value),
        None => object.remove(key),
    };
    serde_json::to_string(&root).unwrap()
}

pub fn decode_client(json: &str) -> Result<ClientMessage, DecodeError> {
    ClientMessage::decode(&control_frame(json)).map(|(m, _)| m)
}

pub fn decode_daemon(json: &str) -> Result<DaemonMessage, DecodeError> {
    DaemonMessage::decode(&control_frame(json)).map(|(m, _)| m)
}

pub fn client_detail(json: &str) -> String {
    match decode_client(json) {
        Err(DecodeError::BadJson { detail }) => detail,
        other => panic!("expected BadJson, got {other:?}: {json}"),
    }
}

pub fn daemon_detail(json: &str) -> String {
    match decode_daemon(json) {
        Err(DecodeError::BadJson { detail }) => detail,
        other => panic!("expected BadJson, got {other:?}: {json}"),
    }
}

pub fn display() -> Display {
    Display {
        foreground: "#ddeeff".into(),
        background: "#112233".into(),
        cursor_color: "#aabbcc".into(),
        cell_width: 9,
        cell_height: 18,
    }
}

pub fn cursor(epoch: u64, seq: u64) -> Cursor {
    Cursor {
        attachment: AttachmentId(7),
        epoch: Epoch(epoch),
        seq: Seq(seq),
    }
}

/// `be32(1 + json.len()) ++ 0x01 ++ json`.
pub fn control_frame(json: &str) -> Vec<u8> {
    frame(1, json.as_bytes())
}

pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

pub fn words(words: &[u64], tail: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    out.extend_from_slice(tail);
    out
}

pub fn client_goldens() -> Vec<(&'static str, ClientMessage, Vec<u8>)> {
    let attach = format!(
        r#"{{"type":"attach","request":1,"terminal":"fake:t1","options":{{"cols":80,"rows":24,"display":{DISPLAY_JSON},"observe":false,"history":1000}}}}"#
    );
    let set_display =
        format!(r#"{{"type":"set_display","attachment":7,"display":{DISPLAY_JSON}}}"#);
    vec![
        (
            "hello",
            ClientMessage::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
            }),
            control_frame(&format!(
                r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION}}}"#
            )),
        ),
        (
            "attach",
            ClientMessage::Attach(Attach {
                request: RequestId(1),
                terminal: TermId("fake:t1".into()),
                options: AttachRequestOptions {
                    cols: 80,
                    rows: 24,
                    display: display(),
                    observe: false,
                    history: 1000,
                },
                instance: None,
            }),
            control_frame(&attach),
        ),
        (
            "resume",
            ClientMessage::Resume(Resume {
                request: RequestId(2),
                cursor: cursor(3, 41),
                instance: None,
            }),
            control_frame(
                r#"{"type":"resume","request":2,"cursor":{"attachment":7,"epoch":3,"seq":41}}"#,
            ),
        ),
        (
            "detach",
            ClientMessage::Detach(Detach {
                attachment: AttachmentId(7),
            }),
            control_frame(r#"{"type":"detach","attachment":7}"#),
        ),
        (
            "ack",
            ClientMessage::Ack(Ack {
                cursor: cursor(3, 42),
            }),
            control_frame(r#"{"type":"ack","cursor":{"attachment":7,"epoch":3,"seq":42}}"#),
        ),
        (
            "resize",
            ClientMessage::Resize(Resize {
                attachment: AttachmentId(7),
                cols: 120,
                rows: 40,
                ownership_generation: OwnershipGeneration(5),
            }),
            control_frame(
                r#"{"type":"resize","attachment":7,"cols":120,"rows":40,"ownership_generation":5}"#,
            ),
        ),
        (
            "focus",
            ClientMessage::Focus(Focus {
                attachment: AttachmentId(7),
            }),
            control_frame(r#"{"type":"focus","attachment":7}"#),
        ),
        (
            "set_display",
            ClientMessage::SetDisplay(SetDisplay {
                attachment: AttachmentId(7),
                display: display(),
            }),
            control_frame(&set_display),
        ),
        (
            "request_ownership",
            ClientMessage::RequestOwnership(RequestOwnership {
                attachment: AttachmentId(7),
                if_vacant: None,
            }),
            control_frame(r#"{"type":"request_ownership","attachment":7}"#),
        ),
        (
            "input",
            ClientMessage::Input(Input {
                attachment: AttachmentId(7),
                bytes: b"ls\r".to_vec(),
            }),
            vec![0, 0, 0, 0x0c, 3, 0, 0, 0, 0, 0, 0, 0, 7, 0x6c, 0x73, 0x0d],
        ),
        (
            "automatic_reply",
            ClientMessage::AutomaticReply(AutomaticReply {
                attachment: AttachmentId(7),
                epoch: Epoch(3),
                ownership_generation: OwnershipGeneration(5),
                bytes: b"\x1b[1;1R".to_vec(),
            }),
            frame(4, &words(&[7, 3, 5], b"\x1b[1;1R")),
        ),
        (
            "get_panel",
            ClientMessage::GetPanel(GetPanel {
                request: RequestId(12),
                space: s(SPACE),
                panel: s("classes"),
            }),
            control_frame(
                r#"{"type":"get_panel","request":12,"space":"/Users/me/src/trellis","panel":"classes"}"#,
            ),
        ),
        (
            "report_render",
            ClientMessage::ReportRender(ReportRender {
                space: s(SPACE),
                panel: s("classes"),
                revision: 7,
                status: RenderStatus::Rendered,
                error: None,
            }),
            control_frame(
                r#"{"type":"report_render","space":"/Users/me/src/trellis","panel":"classes","revision":7,"status":"rendered","error":null}"#,
            ),
        ),
        (
            "report_render_failed",
            ClientMessage::ReportRender(ReportRender {
                space: s(SPACE),
                panel: s("classes"),
                revision: 8,
                status: RenderStatus::RenderFailed,
                error: Some(PanelError {
                    code: s("render_error"),
                    message: s("the diagram couldn't be drawn"),
                    line: None,
                    column: None,
                    snippet: None,
                }),
            }),
            control_frame(
                r#"{"type":"report_render","space":"/Users/me/src/trellis","panel":"classes","revision":8,"status":"render_failed","error":{"code":"render_error","message":"the diagram couldn't be drawn","line":null,"column":null,"snippet":null}}"#,
            ),
        ),
        (
            "close_panel",
            ClientMessage::ClosePanel(ClosePanel {
                request: RequestId(13),
                space: s(SPACE),
                panel: s("scratch-1"),
            }),
            control_frame(
                r#"{"type":"close_panel","request":13,"space":"/Users/me/src/trellis","panel":"scratch-1"}"#,
            ),
        ),
        (
            "pin_panel",
            ClientMessage::PinPanel(PinPanel {
                request: RequestId(14),
                space: s(SPACE),
                panel: s("classes"),
            }),
            control_frame(
                r#"{"type":"pin_panel","request":14,"space":"/Users/me/src/trellis","panel":"classes"}"#,
            ),
        ),
        (
            "mutate_layout_panel_ops",
            ClientMessage::MutateLayout(MutateLayout {
                request: RequestId(15),
                space: s(SPACE),
                base_layout_revision: 4,
                ops: vec![
                    LayoutOp::AddPanel {
                        panel: s("classes"),
                        tile: Some(s("main")),
                    },
                    LayoutOp::AddPanel {
                        panel: s("notes"),
                        tile: None,
                    },
                    LayoutOp::RemovePanel {
                        panel: s("scratch-1"),
                    },
                ],
            }),
            control_frame(
                r#"{"type":"mutate_layout","request":15,"space":"/Users/me/src/trellis","base_layout_revision":4,"ops":[{"op":"add_panel","panel":"classes","tile":"main"},{"op":"add_panel","panel":"notes","tile":null},{"op":"remove_panel","panel":"scratch-1"}]}"#,
            ),
        ),
        (
            "mcp_open",
            ClientMessage::McpOpen(McpOpen {
                request: RequestId(1),
                caller: McpCaller {
                    terminal_id: Some(s("t8")),
                    terminal_socket: Some(s("/tmp/core/demo.sock")),
                    cwd: Some(s("/Users/me/src/trellis/crates")),
                },
            }),
            control_frame(
                r#"{"type":"mcp_open","request":1,"caller":{"terminal_id":"t8","terminal_socket":"/tmp/core/demo.sock","cwd":"/Users/me/src/trellis/crates"}}"#,
            ),
        ),
        (
            "mcp_open_cwd_only",
            ClientMessage::McpOpen(McpOpen {
                request: RequestId(2),
                caller: McpCaller {
                    terminal_id: None,
                    terminal_socket: None,
                    cwd: Some(s(SPACE)),
                },
            }),
            control_frame(
                r#"{"type":"mcp_open","request":2,"caller":{"terminal_id":null,"terminal_socket":null,"cwd":"/Users/me/src/trellis"}}"#,
            ),
        ),
        (
            "mcp_client",
            ClientMessage::Mcp(Mcp {
                message: json_rpc(MCP_CLIENT_MESSAGE),
            }),
            control_frame(&format!(
                r#"{{"type":"mcp","message":{MCP_CLIENT_MESSAGE}}}"#
            )),
        ),
    ]
}

pub fn daemon_goldens() -> Vec<(&'static str, DaemonMessage, Vec<u8>)> {
    let facts = format!(
        r#"{{"type":"facts","cursor":{{"attachment":7,"epoch":4,"seq":13}},"display":{DISPLAY_JSON}}}"#
    );
    vec![
        (
            "welcome",
            DaemonMessage::Welcome(Welcome {
                protocol_version: PROTOCOL_VERSION,
                host_id: HostId("3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b".into()),
            }),
            control_frame(&format!(
                r#"{{"type":"welcome","protocol_version":{PROTOCOL_VERSION},"host_id":"3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b"}}"#
            )),
        ),
        (
            "attached",
            DaemonMessage::Attached(Attached {
                request: RequestId(1),
                attachment: AttachmentId(7),
            }),
            control_frame(r#"{"type":"attached","request":1,"attachment":7}"#),
        ),
        (
            "resumed",
            DaemonMessage::Resumed(Resumed {
                request: RequestId(2),
                attachment: AttachmentId(7),
                replay_through: Some(cursor(3, 45)),
            }),
            control_frame(
                r#"{"type":"resumed","request":2,"attachment":7,"replay_through":{"attachment":7,"epoch":3,"seq":45}}"#,
            ),
        ),
        (
            "resumed_none",
            DaemonMessage::Resumed(Resumed {
                request: RequestId(2),
                attachment: AttachmentId(7),
                replay_through: None,
            }),
            control_frame(r#"{"type":"resumed","request":2,"attachment":7,"replay_through":null}"#),
        ),
        (
            "gap",
            DaemonMessage::Gap(Gap {
                attachment: AttachmentId(7),
                request: None,
            }),
            control_frame(r#"{"type":"gap","attachment":7,"request":null}"#),
        ),
        (
            "gap_answering_resume",
            DaemonMessage::Gap(Gap {
                attachment: AttachmentId(7),
                request: Some(RequestId(2)),
            }),
            control_frame(r#"{"type":"gap","attachment":7,"request":2}"#),
        ),
        (
            "ownership",
            DaemonMessage::Ownership(Ownership {
                attachment: AttachmentId(7),
                ownership_generation: OwnershipGeneration(5),
                owner: Some(AttachmentId(9)),
            }),
            control_frame(
                r#"{"type":"ownership","attachment":7,"ownership_generation":5,"owner":9}"#,
            ),
        ),
        (
            "ownership_none",
            DaemonMessage::Ownership(Ownership {
                attachment: AttachmentId(7),
                ownership_generation: OwnershipGeneration(5),
                owner: None,
            }),
            control_frame(
                r#"{"type":"ownership","attachment":7,"ownership_generation":5,"owner":null}"#,
            ),
        ),
        (
            "error",
            DaemonMessage::Error(ErrorMessage {
                code: ErrorCode::StaleGeneration,
                message: "resize from ownership generation 4; current is 5".into(),
                request: None,
                attachment: Some(AttachmentId(7)),
            }),
            control_frame(
                r#"{"type":"error","code":"stale_generation","message":"resize from ownership generation 4; current is 5","request":null,"attachment":7}"#,
            ),
        ),
        (
            "started",
            DaemonMessage::Started(Started {
                cursor: cursor(4, 10),
                backend_generation: BackendGeneration(2),
                cols: 80,
                rows: 24,
                fidelity: Fidelity::Exact,
                ordered_resizes: true,
            }),
            control_frame(
                r#"{"type":"started","cursor":{"attachment":7,"epoch":4,"seq":10},"backend_generation":2,"cols":80,"rows":24,"fidelity":"exact","ordered_resizes":true}"#,
            ),
        ),
        (
            "data",
            DaemonMessage::Data(Data {
                cursor: cursor(4, 11),
                bytes: b"hi".to_vec(),
            }),
            frame(2, &words(&[7, 4, 11], b"hi")),
        ),
        (
            "resized",
            DaemonMessage::Resized(Resized {
                cursor: cursor(4, 12),
                cols: 120,
                rows: 40,
            }),
            control_frame(
                r#"{"type":"resized","cursor":{"attachment":7,"epoch":4,"seq":12},"cols":120,"rows":40}"#,
            ),
        ),
        (
            "facts",
            DaemonMessage::Facts(Facts {
                cursor: cursor(4, 13),
                display: display(),
            }),
            control_frame(&facts),
        ),
        (
            "resynced",
            DaemonMessage::Resynced(Resynced {
                cursor: cursor(4, 14),
            }),
            control_frame(r#"{"type":"resynced","cursor":{"attachment":7,"epoch":4,"seq":14}}"#),
        ),
        (
            "closed",
            DaemonMessage::Closed(Closed {
                cursor: cursor(4, 15),
                reason: CloseReason::Detached,
            }),
            control_frame(
                r#"{"type":"closed","cursor":{"attachment":7,"epoch":4,"seq":15},"reason":"detached"}"#,
            ),
        ),
        (
            "lost",
            DaemonMessage::Lost(Lost {
                cursor: cursor(4, 16),
            }),
            control_frame(r#"{"type":"lost","cursor":{"attachment":7,"epoch":4,"seq":16}}"#),
        ),
        (
            "panels",
            DaemonMessage::Panels(Panels {
                request: Some(RequestId(5)),
                space: s(SPACE),
                panels: vec![
                    PanelSummary {
                        id: s("classes"),
                        title: Some(s("Class diagram")),
                        renderer: Renderer::Mermaid,
                        revision: 7,
                        state: PanelState::Validated,
                        error: None,
                        created_by: Some(CreatedBy::Agent {
                            slot: Some(s("claude")),
                        }),
                        lifetime: PanelLifetime::Persistent,
                    },
                    PanelSummary {
                        id: s("flow"),
                        title: None,
                        renderer: Renderer::Mermaid,
                        revision: 9,
                        state: PanelState::Failed,
                        error: Some(PanelError {
                            code: s("parse_error"),
                            message: s("unexpected token"),
                            line: Some(2),
                            column: Some(11),
                            snippet: Some(s("  A -> B")),
                        }),
                        created_by: Some(CreatedBy::User),
                        lifetime: PanelLifetime::Scratch,
                    },
                ],
                skipped: vec![],
                error: None,
            }),
            control_frame(
                r#"{"type":"panels","request":5,"space":"/Users/me/src/trellis","panels":[{"id":"classes","title":"Class diagram","renderer":"mermaid","revision":7,"state":"validated","error":null,"created_by":{"type":"agent","slot":"claude"},"lifetime":"persistent"},{"id":"flow","title":null,"renderer":"mermaid","revision":9,"state":"failed","error":{"code":"parse_error","message":"unexpected token","line":2,"column":11,"snippet":"  A -> B"},"created_by":{"type":"user"},"lifetime":"scratch"}],"skipped":[],"error":null}"#,
            ),
        ),
        (
            "terminals",
            DaemonMessage::Terminals(Terminals {
                request: Some(RequestId(7)),
                terminals: vec![TerminalInfo {
                    id: TermId(s("t8")),
                    cwd: s("/Users/me/src/trellis"),
                    argv: vec![s("zsh")],
                    state: TerminalState::Running,
                    exit_code: None,
                    labels: Default::default(),
                    cols: 80,
                    rows: 24,
                }],
                spaces: vec![SpaceInfo {
                    id: s("/Users/me/src/trellis"),
                    kind: SpaceKind::Repository,
                    terminals: vec![TermId(s("t8"))],
                }],
                backend: BackendInfo {
                    id: s("backend-a"),
                    instance: Some(s("0123abcd")),
                },
            }),
            control_frame(
                r#"{"type":"terminals","request":7,"terminals":[{"id":"t8","cwd":"/Users/me/src/trellis","argv":["zsh"],"state":"running","exit_code":null,"labels":{},"cols":80,"rows":24}],"spaces":[{"id":"/Users/me/src/trellis","kind":"repository","terminals":["t8"]}],"backend":{"id":"backend-a","instance":"0123abcd"}}"#,
            ),
        ),
        (
            "panels_push",
            DaemonMessage::Panels(Panels {
                request: None,
                space: s(SPACE),
                panels: vec![PanelSummary {
                    id: s("notes"),
                    title: Some(s("Notes")),
                    renderer: Renderer::Markdown,
                    revision: 3,
                    state: PanelState::Validating,
                    error: None,
                    created_by: None,
                    lifetime: PanelLifetime::Scratch,
                }],
                skipped: vec![SkippedPanel {
                    panel: s("build"),
                    reason: s("not a view panel"),
                }],
                error: None,
            }),
            control_frame(
                r#"{"type":"panels","request":null,"space":"/Users/me/src/trellis","panels":[{"id":"notes","title":"Notes","renderer":"markdown","revision":3,"state":"validating","error":null,"created_by":null,"lifetime":"scratch"}],"skipped":[{"panel":"build","reason":"not a view panel"}],"error":null}"#,
            ),
        ),
        (
            "panel",
            DaemonMessage::Panel(Panel {
                request: RequestId(12),
                space: s(SPACE),
                id: s("classes"),
                revision: 7,
                renderer: Renderer::Mermaid,
                source: s(CLASS_SOURCE),
                state: PanelState::Validated,
                error: None,
            }),
            control_frame(
                r#"{"type":"panel","request":12,"space":"/Users/me/src/trellis","id":"classes","revision":7,"renderer":"mermaid","source":"classDiagram\n  Animal <|-- Duck\n","state":"validated","error":null}"#,
            ),
        ),
        (
            "panel_closed",
            DaemonMessage::PanelClosed(PanelClosed {
                request: RequestId(13),
                space: s(SPACE),
                panel: s("scratch-1"),
            }),
            control_frame(
                r#"{"type":"panel_closed","request":13,"space":"/Users/me/src/trellis","panel":"scratch-1"}"#,
            ),
        ),
        (
            "panel_pinned",
            DaemonMessage::PanelPinned(PanelPinned {
                request: RequestId(14),
                space: s(SPACE),
                panel: s("classes"),
            }),
            control_frame(
                r#"{"type":"panel_pinned","request":14,"space":"/Users/me/src/trellis","panel":"classes"}"#,
            ),
        ),
        (
            "mcp_opened",
            DaemonMessage::McpOpened(McpOpened {
                request: RequestId(1),
                space: s(SPACE),
            }),
            control_frame(r#"{"type":"mcp_opened","request":1,"space":"/Users/me/src/trellis"}"#),
        ),
        (
            "mcp_daemon",
            DaemonMessage::Mcp(Mcp {
                message: json_rpc(MCP_DAEMON_MESSAGE),
            }),
            control_frame(&format!(
                r#"{{"type":"mcp","message":{MCP_DAEMON_MESSAGE}}}"#
            )),
        ),
        (
            "error_unknown_panel",
            DaemonMessage::Error(ErrorMessage {
                code: ErrorCode::UnknownPanel,
                message: s("the space has no panel classes"),
                request: Some(RequestId(12)),
                attachment: None,
            }),
            control_frame(
                r#"{"type":"error","code":"unknown_panel","message":"the space has no panel classes","request":12,"attachment":null}"#,
            ),
        ),
        (
            "error_unknown_caller",
            DaemonMessage::Error(ErrorMessage {
                code: ErrorCode::UnknownCaller,
                message: s("no terminal or space matches the caller"),
                request: Some(RequestId(1)),
                attachment: None,
            }),
            control_frame(
                r#"{"type":"error","code":"unknown_caller","message":"no terminal or space matches the caller","request":1,"attachment":null}"#,
            ),
        ),
        (
            "error_panel_limit",
            DaemonMessage::Error(ErrorMessage {
                code: ErrorCode::PanelLimit,
                message: s("the space already has 100 panels; the limit is 100"),
                request: None,
                attachment: None,
            }),
            control_frame(
                r#"{"type":"error","code":"panel_limit","message":"the space already has 100 panels; the limit is 100","request":null,"attachment":null}"#,
            ),
        ),
    ]
}
