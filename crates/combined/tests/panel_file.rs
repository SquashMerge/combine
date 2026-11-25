//! `panel.json`, `scratch.json` and the encoded size of a `panel` reply.

use combine_protocol::{
    CreatedBy, DaemonMessage, Panel, PanelError, PanelState, Renderer, RequestId, WireMessage,
    MAX_FRAME_LEN,
};
use combined::core::{
    content_file, encode_scratch_meta, encode_view_panel, panel_file_bound, panel_reply_len,
    parse_scratch_meta, parse_view_panel, PanelSkip, PANEL_MAX_BYTES,
};
use combined::model::{SpaceId, ViewPanelFile};

fn space() -> SpaceId {
    SpaceId("/work/app".to_owned())
}

#[test]
fn panel_reply_len_matches_the_encoded_frame() {
    let source = "graph TD\n  a --> b\n".repeat(50);
    let reply = DaemonMessage::Panel(Panel {
        request: RequestId(u64::MAX),
        space: space().0,
        id: "flow".to_owned(),
        revision: u64::MAX,
        renderer: Renderer::Mermaid,
        source: source.clone(),
        state: PanelState::ValidatedParserOnly,
        error: Some(PanelError {
            code: "unsupported_diagram".to_owned(),
            message: "\u{1}".repeat(1024),
            line: Some(u32::MAX),
            column: Some(u32::MAX),
            snippet: Some("\u{1}".repeat(1024)),
        }),
    });
    let frame = reply.encode().unwrap();
    assert_eq!(
        panel_reply_len(&space(), "flow", Renderer::Mermaid, &source),
        frame.len() - 4
    );
}

#[test]
fn a_204800_byte_control_source_does_not_fit_and_a_plain_one_does() {
    let controls = "\u{1}".repeat(204_800);
    assert!(panel_reply_len(&space(), "p", Renderer::Markdown, &controls) > MAX_FRAME_LEN as usize);
    let plain = "a".repeat(204_800);
    assert!(panel_reply_len(&space(), "p", Renderer::Markdown, &plain) <= MAX_FRAME_LEN as usize);
}

#[test]
fn the_largest_view_panel_file_encodes_within_8192() {
    let file = ViewPanelFile {
        renderer: Renderer::Markdown,
        title: Some("\"".repeat(1024)),
        idempotency_key: Some("\"".repeat(256)),
        created_by: Some(CreatedBy::Agent {
            slot: Some("\"".repeat(256)),
        }),
        placement: Some("\"".repeat(256)),
    };
    let bytes = encode_view_panel(&file);
    // 163 bytes of fixed text plus 3,584 of fully escaped fields; far below the 8,192 bound.
    assert_eq!(bytes.len(), 3_747);
    assert!(bytes.len() <= PANEL_MAX_BYTES as usize);
    assert_eq!(parse_view_panel("p", &bytes), Ok(file));
}

#[test]
fn encode_writes_the_fields_in_order_with_a_trailing_newline() {
    let file = ViewPanelFile {
        renderer: Renderer::Mermaid,
        title: Some("Flow".to_owned()),
        idempotency_key: None,
        created_by: Some(CreatedBy::User),
        placement: Some("main".to_owned()),
    };
    let text = String::from_utf8(encode_view_panel(&file)).unwrap();
    assert!(text.ends_with("}\n"));
    let version = text.find("\"version\"").unwrap();
    let renderer = text.find("\"renderer\"").unwrap();
    let title = text.find("\"title\"").unwrap();
    let created_by = text.find("\"created_by\"").unwrap();
    let placement = text.find("\"placement\"").unwrap();
    assert!(version < renderer && renderer < title && title < created_by && created_by < placement);
    assert!(!text.contains("idempotency_key"));
    assert!(!text.contains("\"id\""));
}

#[test]
fn placement_round_trips_and_a_file_without_it_parses() {
    let file = ViewPanelFile {
        renderer: Renderer::Markdown,
        title: None,
        idempotency_key: None,
        created_by: None,
        placement: Some("main".to_owned()),
    };
    let bytes = encode_view_panel(&file);
    assert!(String::from_utf8(bytes.clone())
        .unwrap()
        .contains("\"placement\": \"main\""));
    assert_eq!(parse_view_panel("p", &bytes), Ok(file));
    let bare = parse(r#"{"version":1,"renderer":"markdown"}"#).unwrap();
    assert_eq!(bare.placement, None);
    assert!(!String::from_utf8(encode_view_panel(&bare))
        .unwrap()
        .contains("placement"));
    assert!(parse(r#"{"version":1,"renderer":"markdown","placement":""}"#).is_err());
}

fn parse(json: &str) -> Result<ViewPanelFile, PanelSkip> {
    parse_view_panel("p", json.as_bytes())
}

#[test]
fn view_panel_parse_checks_run_in_order_with_their_texts() {
    assert!(matches!(parse("{"), Err(PanelSkip::BadJson(_))));
    assert_eq!(parse("[]"), Err(PanelSkip::NotObject));
    assert_eq!(parse(r#"{"version":9}"#), Err(PanelSkip::NotView));
    assert_eq!(
        parse(r#"{"renderer":"markdown"}"#),
        Err(PanelSkip::NoVersion)
    );
    assert_eq!(
        parse(r#"{"version":2,"renderer":"markdown"}"#)
            .unwrap_err()
            .to_string(),
        "unsupported version 2; this daemon reads version 1"
    );
    assert_eq!(
        parse(r#"{"version":1,"id":"q","renderer":"nope"}"#),
        Err(PanelSkip::IdMismatch)
    );
    assert_eq!(
        parse(r#"{"version":1,"renderer":"nope"}"#)
            .unwrap_err()
            .to_string(),
        "renderer must be \"mermaid\" or \"markdown\""
    );
    assert_eq!(
        parse(r#"{"version":1,"renderer":"markdown","title":""}"#)
            .unwrap_err()
            .to_string(),
        "title must be a string of 1 to 1024 bytes with no control characters"
    );
    assert_eq!(
        parse(r#"{"version":1,"renderer":"markdown","idempotency_key":"a\nb"}"#)
            .unwrap_err()
            .to_string(),
        "idempotency_key must be a string of 1 to 256 bytes with no control characters"
    );
    assert_eq!(
        parse(r#"{"version":1,"renderer":"markdown","created_by":{"type":"robot"}}"#)
            .unwrap_err()
            .to_string(),
        "created_by must be {\"type\":\"user\"} or {\"type\":\"agent\",\"slot\":<slot or null>}"
    );
    assert_eq!(
        parse(r#"{"version":1,"renderer":"markdown","created_by":{"type":"agent","slot":""}}"#)
            .unwrap_err()
            .to_string(),
        "created_by.slot must be 1 to 256 bytes with no control characters"
    );
    assert_eq!(
        parse(r#"{"version":1,"renderer":"markdown","placement":"a\nb"}"#)
            .unwrap_err()
            .to_string(),
        "placement must be a tile ID of 1 to 256 bytes with no control characters"
    );
    let file = parse(
        r#"{"version":1,"id":"p","renderer":"markdown","extra":true,
            "created_by":{"type":"agent","slot":null},"placement":"main"}"#,
    )
    .unwrap();
    assert_eq!(file.created_by, Some(CreatedBy::Agent { slot: None }));
    assert_eq!(file.placement.as_deref(), Some("main"));
}

#[test]
fn a_terminal_template_with_a_bad_version_is_not_a_view_panel() {
    assert_eq!(
        parse(r#"{"version":7,"content":{"type":"terminal"}}"#),
        Err(PanelSkip::NotView)
    );
}

#[test]
fn scratch_meta_round_trips_and_refuses_with_its_reasons() {
    let key = space().sha256_hex();
    let bytes = encode_scratch_meta(&space(), 1_700_000_000_000);
    assert!(bytes.ends_with(b"\n"));
    let meta = parse_scratch_meta(&key, &bytes).unwrap();
    assert_eq!(meta.space, space());
    assert_eq!(meta.changed_at_ms, 1_700_000_000_000);

    let err = |json: &str| parse_scratch_meta(&key, json.as_bytes()).unwrap_err();
    assert!(err("{").starts_with("scratch.json isn't valid JSON: "));
    assert_eq!(err("[]"), "scratch.json isn't a JSON object");
    assert_eq!(err("{}"), "scratch.json has no version");
    assert_eq!(
        err(r#"{"version":3}"#),
        "unsupported version 3; this daemon reads version 1"
    );
    assert_eq!(
        err(r#"{"version":1,"space":"rel","changed_at_ms":1}"#),
        "space must be an absolute folder path"
    );
    assert_eq!(
        err(r#"{"version":1,"space":"/a","changed_at_ms":"x"}"#),
        "changed_at_ms must be an integer"
    );
    assert_eq!(
        err(r#"{"version":1,"space":"/a","changed_at_ms":1}"#),
        "scratch.json names another space"
    );
}

#[test]
fn files_and_bounds() {
    assert_eq!(content_file(Renderer::Mermaid), "diagram.mmd");
    assert_eq!(content_file(Renderer::Markdown), "content.md");
    assert_eq!(panel_file_bound("panel.json"), 8_192);
    assert_eq!(panel_file_bound("scratch.json"), 8_192);
    assert_eq!(panel_file_bound("content.md"), 1_048_576);
}
