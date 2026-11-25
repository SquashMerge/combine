//! Core's fallbacks for the version 5 messages the server handles: `mcp_open` and `mcp`,
//! `get_panel`, `close_panel`, `pin_panel` and `report_render`. A real connection never reaches
//! them.

use combine_protocol as wire;
use combined::core::{Core, CoreConfig};
use combined::model::Millis;
use combined::ports::Capabilities;

const PANEL: &str = "panel requests are answered by the server, not by core";

fn core() -> Core {
    let capabilities = Capabilities {
        ordered_attach: true,
        byte_credit: false,
        initial_credit: 0,
        display_queries: true,
        automatic_replies: true,
        resync_signal: true,
        agent_state: false,
    };
    Core::new(CoreConfig::default(), capabilities).unwrap()
}

fn error(message: &str, request: u64) -> wire::DaemonMessage {
    wire::DaemonMessage::Error(wire::ErrorMessage {
        code: wire::ErrorCode::InvalidRequest,
        message: message.to_string(),
        request: Some(wire::RequestId(request)),
        attachment: None,
    })
}

fn panel_request(request: u64) -> (wire::GetPanel, wire::ClosePanel, wire::PinPanel) {
    let space = "/work/repo".to_string();
    let panel = "classes".to_string();
    (
        wire::GetPanel {
            request: wire::RequestId(request),
            space: space.clone(),
            panel: panel.clone(),
        },
        wire::ClosePanel {
            request: wire::RequestId(request + 1),
            space: space.clone(),
            panel: panel.clone(),
        },
        wire::PinPanel {
            request: wire::RequestId(request + 2),
            space,
            panel,
        },
    )
}

#[test]
fn v5_requests_get_invalid_request_and_v5_frames_get_nothing() {
    let (get, close, pin) = panel_request(12);
    let requests = [
        (wire::ClientMessage::GetPanel(get), error(PANEL, 12)),
        (wire::ClientMessage::ClosePanel(close), error(PANEL, 13)),
        (wire::ClientMessage::PinPanel(pin), error(PANEL, 14)),
    ];
    for (message, reply) in requests {
        let mut core = core();
        let connection = core.connect();
        core.client_message(connection, message, Millis(0));
        assert_eq!(core.pull(connection, usize::MAX, Millis(0)), vec![reply]);
        assert!(core.take_effects().is_empty());
    }

    let silent = [wire::ClientMessage::ReportRender(wire::ReportRender {
        space: "/work/repo".to_string(),
        panel: "classes".to_string(),
        revision: 7,
        status: wire::RenderStatus::Rendered,
        error: None,
    })];
    for message in silent {
        let mut core = core();
        let connection = core.connect();
        core.client_message(connection, message, Millis(0));
        assert!(core.pull(connection, usize::MAX, Millis(0)).is_empty());
        assert!(core.take_effects().is_empty());
    }
}

#[test]
fn core_leaves_the_mcp_channel_to_the_server() {
    let messages = [
        wire::ClientMessage::McpOpen(wire::McpOpen {
            request: wire::RequestId(1),
            caller: wire::McpCaller {
                terminal_id: Some("t8".to_string()),
                terminal_socket: None,
                cwd: None,
            },
        }),
        wire::ClientMessage::Mcp(wire::Mcp {
            message: wire::JsonRpcMessage::new(r#"{"jsonrpc":"2.0","id":1}"#.to_string()).unwrap(),
        }),
    ];
    for message in messages {
        let mut core = core();
        let connection = core.connect();
        core.client_message(connection, message, Millis(0));
        assert!(core.pull(connection, usize::MAX, Millis(0)).is_empty());
        assert!(core.take_effects().is_empty());
    }
}
