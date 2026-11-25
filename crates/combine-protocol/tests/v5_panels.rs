//! Protocol version 5: panel messages and the 200 KiB source limit.

mod common;

use combine_protocol::*;
use common::*;
use serde_json::json;

/// SPEC's 200 KB source limit: 200 KiB.
const SOURCE_LIMIT: usize = 204_800;

fn panels_golden() -> Panels {
    match daemon_goldens()
        .into_iter()
        .find(|(k, _, _)| *k == "panels")
    {
        Some((_, DaemonMessage::Panels(m), _)) => m,
        _ => panic!("panels golden"),
    }
}

fn panel_golden() -> Panel {
    match daemon_goldens().into_iter().find(|(k, _, _)| *k == "panel") {
        Some((_, DaemonMessage::Panel(m), _)) => m,
        _ => panic!("panel golden"),
    }
}

fn panel_with(source: String) -> DaemonMessage {
    DaemonMessage::Panel(Panel {
        source,
        ..panel_golden()
    })
}

fn panels_message(panels: Panels) -> DaemonMessage {
    DaemonMessage::Panels(panels)
}

fn decoded_panels(json: &str) -> Panels {
    match decode_daemon(json) {
        Ok(DaemonMessage::Panels(m)) => m,
        other => panic!("{other:?}: {json}"),
    }
}

fn decoded_panel(json: &str) -> Panel {
    match decode_daemon(json) {
        Ok(DaemonMessage::Panel(m)) => m,
        other => panic!("{other:?}: {json}"),
    }
}

fn decoded_report(json: &str) -> ReportRender {
    match decode_client(json) {
        Ok(ClientMessage::ReportRender(m)) => m,
        other => panic!("{other:?}: {json}"),
    }
}

fn entry(state: PanelState, renderer: Renderer) -> PanelSummary {
    PanelSummary {
        id: s("p"),
        title: None,
        renderer,
        revision: 1,
        state,
        error: None,
        created_by: None,
        lifetime: None,
    }
}

fn text_of(message: &DaemonMessage) -> String {
    String::from_utf8(message.encode().unwrap()[5..].to_vec()).unwrap()
}

#[test]
fn panel_states_have_their_wire_spellings() {
    for (state, name) in [
        (PanelState::Validating, "validating"),
        (PanelState::Fetching, "fetching"),
        (PanelState::Validated, "validated"),
        (PanelState::ValidatedParserOnly, "validated_parser_only"),
        (PanelState::Failed, "failed"),
        (PanelState::Untrusted, "untrusted"),
        (PanelState::Removed, "removed"),
    ] {
        let message = panels_message(Panels {
            panels: vec![entry(state, Renderer::Mermaid)],
            ..panels_golden()
        });
        let text = text_of(&message);
        assert!(text.contains(&format!(r#""state":"{name}""#)), "{text}");
        assert_eq!(decode_daemon(&text).unwrap(), message, "{name}");
    }
}

#[test]
fn renderers_have_their_wire_spellings() {
    for (renderer, name) in [
        (Renderer::Mermaid, "mermaid"),
        (Renderer::Markdown, "markdown"),
    ] {
        let message = panels_message(Panels {
            panels: vec![entry(PanelState::Validated, renderer)],
            ..panels_golden()
        });
        let text = text_of(&message);
        assert!(text.contains(&format!(r#""renderer":"{name}""#)), "{text}");
        assert_eq!(decode_daemon(&text).unwrap(), message, "{name}");
    }
}

#[test]
fn render_statuses_have_their_wire_spellings() {
    for (status, name) in [
        (RenderStatus::Rendered, "rendered"),
        (RenderStatus::RenderFailed, "render_failed"),
    ] {
        let message = ClientMessage::ReportRender(ReportRender {
            space: s(SPACE),
            panel: s("p"),
            revision: 1,
            status,
            error: None,
        });
        let text = String::from_utf8(message.encode().unwrap()[5..].to_vec()).unwrap();
        assert!(text.contains(&format!(r#""status":"{name}""#)), "{text}");
        assert_eq!(decode_client(&text).unwrap(), message, "{name}");
    }
}

#[test]
fn lifetimes_have_their_wire_spellings() {
    for (lifetime, spelling) in [
        (
            Some(PanelLifetime::Persistent),
            r#""lifetime":"persistent""#,
        ),
        (Some(PanelLifetime::Scratch), r#""lifetime":"scratch""#),
        (None, r#""lifetime":null"#),
    ] {
        let message = panels_message(Panels {
            panels: vec![PanelSummary {
                lifetime,
                ..entry(PanelState::Validated, Renderer::Mermaid)
            }],
            ..panels_golden()
        });
        let text = text_of(&message);
        assert!(text.contains(spelling), "{text}");
        assert_eq!(decode_daemon(&text).unwrap(), message, "{spelling}");
    }
}

#[test]
fn a_panels_entry_without_lifetime_decodes_as_none() {
    let base = golden_json("panels");
    let text = patched(&base, "/panels/0/lifetime", None);
    assert!(!text.contains(r#""lifetime":"persistent""#), "{text}");
    assert_eq!(decoded_panels(&text).panels[0].lifetime, None);
    assert_eq!(
        decoded_panels(&base).panels[0].lifetime,
        Some(PanelLifetime::Persistent)
    );
}

#[test]
fn created_by_names_the_user_or_an_agent_slot() {
    for (created_by, json) in [
        (CreatedBy::User, r#"{"type":"user"}"#),
        (
            CreatedBy::Agent {
                slot: Some(s("claude")),
            },
            r#"{"type":"agent","slot":"claude"}"#,
        ),
        (
            CreatedBy::Agent { slot: None },
            r#"{"type":"agent","slot":null}"#,
        ),
    ] {
        let message = panels_message(Panels {
            panels: vec![PanelSummary {
                created_by: Some(created_by),
                ..entry(PanelState::Validated, Renderer::Mermaid)
            }],
            ..panels_golden()
        });
        let text = text_of(&message);
        assert!(text.contains(&format!(r#""created_by":{json}"#)), "{text}");
        assert_eq!(decode_daemon(&text).unwrap(), message);
    }
    let text = patched(&golden_json("panels"), "/panels/0/created_by/slot", None);
    assert_eq!(
        decoded_panels(&text).panels[0].created_by,
        Some(CreatedBy::Agent { slot: None })
    );
}

#[test]
fn missing_optional_panel_fields_read_as_null() {
    let base = golden_json("panels");
    let mut expected = panels_golden();
    for pointer in ["/request", "/error"] {
        let text = patched(&base, pointer, None);
        let null = patched(&base, pointer, Some(json!(null)));
        assert_eq!(decoded_panels(&text), decoded_panels(&null), "{pointer}");
    }
    assert_eq!(
        decoded_panels(&patched(&base, "/request", None)).request,
        None
    );
    assert_eq!(decoded_panels(&patched(&base, "/error", None)).error, None);

    expected.panels[0].title = None;
    expected.panels[0].error = None;
    expected.panels[0].created_by = None;
    for how in [None, Some(json!(null))] {
        let mut text = base.clone();
        for field in ["title", "error", "created_by"] {
            text = patched(&text, &format!("/panels/0/{field}"), how.clone());
        }
        assert_eq!(decoded_panels(&text), expected);
    }

    let render = golden_json("report_render");
    for how in [None, Some(json!(null))] {
        let text = patched(&render, "/error", how);
        assert_eq!(decoded_report(&text).error, None);
    }
    let panel = golden_json("panel");
    for how in [None, Some(json!(null))] {
        let text = patched(&panel, "/error", how);
        assert_eq!(decoded_panel(&text).error, None);
    }
    let failed = golden_json("report_render_failed");
    for how in [None, Some(json!(null))] {
        let text = patched(
            &patched(&failed, "/error/line", how.clone()),
            "/error/snippet",
            how,
        );
        let error = decoded_report(&text).error.unwrap();
        assert_eq!(
            (error.line, error.column, error.snippet),
            (None, None, None)
        );
    }
}

#[test]
fn panels_messages_without_a_required_field_are_malformed() {
    let base = golden_json("panels");
    for pointer in [
        "/space",
        "/panels",
        "/skipped",
        "/panels/0/renderer",
        "/panels/0/id",
        "/panels/0/revision",
        "/panels/0/state",
        "/panels/1/error/code",
        "/panels/1/error/message",
    ] {
        daemon_detail(&patched(&base, pointer, None));
    }
}

#[test]
fn panels_messages_with_mistyped_fields_are_malformed() {
    let base = golden_json("panels");
    for (pointer, value) in [
        ("/request", json!("5")),
        ("/panels", json!({})),
        ("/panels/0/state", json!("pending")),
        ("/panels/0/renderer", json!("svg")),
        ("/panels/0/revision", json!(-1)),
        ("/panels/0/title", json!(7)),
        ("/panels/0/created_by", json!("agent")),
        ("/panels/0/created_by", json!({"slot": "x"})),
        ("/skipped", json!([{"panel": "p"}])),
        ("/error", json!(7)),
        ("/panels/1/error/line", json!("2")),
    ] {
        daemon_detail(&patched(&base, pointer, Some(value.clone())));
    }
    let detail = daemon_detail(&patched(
        &base,
        "/panels/0/created_by",
        Some(json!({"type": "robot"})),
    ));
    assert!(detail.contains("unknown creator `robot`"), "{detail}");
}

#[test]
fn an_unknown_creator_is_malformed_and_its_name_is_clipped() {
    let long = "x".repeat(200);
    let text = patched(
        &golden_json("panels"),
        "/panels/0/created_by",
        Some(json!({ "type": long })),
    );
    let detail = daemon_detail(&text);
    assert!(detail.contains(&format!("unknown creator `{}`", "x".repeat(64))));
    assert!(!detail.contains(&"x".repeat(65)), "{detail}");
}

#[test]
fn unknown_fields_in_panel_messages_are_ignored() {
    let mut text = golden_json("panels");
    for pointer in [
        "/panels/0/extra",
        "/panels/0/created_by/extra",
        "/panels/1/error/extra",
        "/extra",
    ] {
        text = patched(&text, pointer, Some(json!({"nested": [1, 2]})));
    }
    assert_eq!(decoded_panels(&text), panels_golden());
}

#[test]
fn panel_messages_without_a_required_field_are_malformed() {
    let base = golden_json("panel");
    for field in [
        "request", "space", "id", "revision", "renderer", "source", "state",
    ] {
        daemon_detail(&patched(&base, &format!("/{field}"), None));
    }
    for (field, value) in [
        ("request", json!(null)),
        ("renderer", json!("svg")),
        ("source", json!(7)),
        ("source", json!(null)),
        ("state", json!("pending")),
        ("error", json!({"code": "c"})),
    ] {
        daemon_detail(&patched(&base, &format!("/{field}"), Some(value)));
    }
    let failed = patched(
        &base,
        "/error",
        Some(json!({"code":"c","message":"m","line":1,"column":-1,"snippet":null})),
    );
    daemon_detail(&failed);
}

#[test]
fn request_replies_without_a_required_field_are_malformed() {
    for key in ["panel_closed", "panel_pinned"] {
        let base = golden_json(key);
        for field in ["request", "space", "panel"] {
            daemon_detail(&patched(&base, &format!("/{field}"), None));
        }
        daemon_detail(&patched(&base, "/request", Some(json!(null))));
        daemon_detail(&patched(&base, "/panel", Some(json!(7))));
    }
    let base = golden_json("mcp_opened");
    for field in ["request", "space"] {
        daemon_detail(&patched(&base, &format!("/{field}"), None));
    }
    daemon_detail(&patched(&base, "/request", Some(json!("1"))));
    daemon_detail(&patched(&base, "/space", Some(json!(null))));
}

#[test]
fn panel_requests_without_a_required_field_are_malformed() {
    let get = golden_json("get_panel");
    for field in ["request", "space", "panel"] {
        client_detail(&patched(&get, &format!("/{field}"), None));
    }
    for (field, value) in [
        ("request", json!("12")),
        ("request", json!(-1)),
        ("space", json!(7)),
        ("panel", json!(null)),
    ] {
        client_detail(&patched(&get, &format!("/{field}"), Some(value)));
    }

    let render = golden_json("report_render_failed");
    for field in ["space", "panel", "revision", "status"] {
        client_detail(&patched(&render, &format!("/{field}"), None));
    }
    for (pointer, value) in [
        ("/revision", json!("7")),
        ("/revision", json!(1.5)),
        ("/revision", json!(-1)),
        ("/status", json!("drawn")),
        ("/status", json!(7)),
        ("/error", json!("x")),
        ("/error", json!({"message": "m"})),
        ("/error/line", json!("2")),
    ] {
        client_detail(&patched(&render, pointer, Some(value)));
    }

    let close = golden_json("close_panel");
    for field in ["request", "space", "panel"] {
        client_detail(&patched(&close, &format!("/{field}"), None));
    }
    client_detail(&patched(&close, "/request", Some(json!("13"))));
    client_detail(&patched(&close, "/panel", Some(json!(7))));

    let pin = golden_json("pin_panel");
    for field in ["request", "space", "panel"] {
        client_detail(&patched(&pin, &format!("/{field}"), None));
    }
    client_detail(&patched(&pin, "/request", Some(json!(null))));
    client_detail(&patched(&pin, "/space", Some(json!([]))));
}

#[test]
fn every_v5_panel_message_rejects_bad_json() {
    for key in [
        "get_panel",
        "report_render",
        "report_render_failed",
        "close_panel",
        "pin_panel",
    ] {
        let text = golden_json(key);
        client_detail(&text[..text.len() - 1]);
        client_detail(&format!("{text} x"));
    }
    for key in [
        "panels",
        "panels_push",
        "panel",
        "panel_closed",
        "panel_pinned",
        "mcp_opened",
    ] {
        let text = golden_json(key);
        daemon_detail(&text[..text.len() - 1]);
        daemon_detail(&format!("{text} x"));
    }
}

fn source_of_length(limit: usize) -> String {
    let mut source = s("classDiagram\n");
    let mut i = 0;
    while source.len() < limit {
        source.push_str(&format!("  Class{i} <|-- Class{} : extends\n", i + 1));
        i += 1;
    }
    source.truncate(limit);
    source
}

#[test]
fn a_typical_200_kib_source_round_trips_in_one_panel_frame() {
    let source = source_of_length(SOURCE_LIMIT);
    assert_eq!(source.len(), 204_800);
    let message = panel_with(source);
    let frame = message.encode().unwrap();
    assert!(frame.len() <= 4 + MAX_FRAME_LEN as usize);
    let (decoded, used) = DaemonMessage::decode(&frame).unwrap();
    assert_eq!(decoded, message);
    assert_eq!(used, frame.len());
    assert_eq!(decoded.encode().unwrap(), frame);
}

#[test]
fn a_200_kib_source_of_two_byte_escapes_fits_one_panel_frame() {
    let empty = panel_with(String::new()).encode().unwrap();
    let source: String = ['"', '\\', '\n', '\t']
        .into_iter()
        .cycle()
        .take(SOURCE_LIMIT)
        .collect();
    let message = panel_with(source);
    let frame = message.encode().unwrap();
    assert_eq!(frame.len(), empty.len() + 2 * SOURCE_LIMIT);
    assert!(frame.len() <= 4 + MAX_FRAME_LEN as usize);
    assert_eq!(DaemonMessage::decode(&frame).unwrap().0, message);
}

#[test]
fn a_200_kib_source_of_six_byte_escapes_is_frame_too_large() {
    let empty = panel_with(String::new()).encode().unwrap();
    let source: String = std::iter::repeat_n(char::from(1u8), SOURCE_LIMIT).collect();
    let message = panel_with(source);
    assert_eq!(
        message.encode(),
        Err(EncodeError::FrameTooLarge {
            length: (empty.len() - 4) + 6 * SOURCE_LIMIT,
            max: MAX_FRAME_LEN,
        })
    );
    let mut sink = Vec::new();
    assert!(matches!(
        write_message(&mut sink, &message),
        Err(WriteError::Encode(EncodeError::FrameTooLarge { .. }))
    ));
    assert!(sink.is_empty());
}
