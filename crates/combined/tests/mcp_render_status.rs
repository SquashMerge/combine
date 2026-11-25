#![cfg(feature = "test-support")]
//! `panel_status` over a real channel shows what each connected client reported drawing.

mod common;
mod daemon_support;
mod mcp_support;
mod panel_support;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, GetPanel, PanelError, PanelState, RenderStatus,
    ReportRender, RequestId,
};
use mcp_support::{cwd_caller, wire_options, McpWire};
use panel_support::PanelRig;
use serde_json::json;

#[test]
fn panel_status_shows_a_clients_render_failed() {
    let rig = PanelRig::with_options(wire_options());
    rig.spawn_terminal();
    let mut wire = McpWire::open(&rig, cwd_caller(&rig));
    wire.wait_loaded();

    let created = wire.tool(
        "create_panel",
        json!({"id": "p", "renderer": "markdown", "content": "one"}),
    );
    assert!(!created.is_error, "{created:?}");
    let (view, _) = rig.wait_state("p", PanelState::Validated);

    let before = wire.tool("panel_status", json!({"panel": "p"}));
    assert_eq!(before.body["render_status"], json!([]));

    let mut client = rig.client();
    client.send(&ClientMessage::ReportRender(ReportRender {
        space: rig.space.0.clone(),
        panel: "p".to_string(),
        revision: view.revision,
        status: RenderStatus::RenderFailed,
        error: Some(PanelError {
            code: "render_error".to_string(),
            message: "the diagram has a problem".to_string(),
            line: Some(2),
            column: Some(4),
            snippet: Some("  A -> B".to_string()),
        }),
    }));
    client.send(&ClientMessage::GetPanel(GetPanel {
        request: RequestId(900),
        space: rig.space.0.clone(),
        panel: "a/b".to_string(),
    }));
    match client.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, ErrorCode::InvalidRequest);
            assert_eq!(error.request, Some(RequestId(900)), "a report was answered");
        }
        other => panic!("expected the refusal that ends the sync, got {other:?}"),
    }

    let status = wire.tool("panel_status", json!({"panel": "p"}));
    assert!(!status.is_error, "{status:?}");
    assert_eq!(
        status.body["render_status"],
        json!([{
            "revision": view.revision,
            "status": "render_failed",
            "error": {
                "code": "render_error",
                "message": "the diagram has a problem",
                "line": 2,
                "column": 4,
                "snippet": "  A -> B",
            },
        }])
    );
}
