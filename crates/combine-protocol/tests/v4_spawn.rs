//! Protocol version 4: listing templates and spawning terminals.

mod common;

use std::collections::BTreeMap;

use combine_protocol::*;
use common::control_frame;

const LIST_JSON: &str = r#"{"type":"list_templates","request":9,"space":"/Users/me/src/trellis"}"#;
const SHELL_JSON: &str = r#"{"type":"spawn_terminal","request":10,"space":"/Users/me/src/trellis","source":{"type":"shell"}}"#;
const TEMPLATE_JSON: &str = r#"{"type":"spawn_terminal","request":11,"space":"/Users/me/src/trellis","source":{"type":"template","panel":"claude","hash":"9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"}}"#;
const TEMPLATES_JSON: &str = r#"{"type":"templates","request":9,"space":"/Users/me/src/trellis","templates":[{"panel":"claude","title":"Claude","slot":"claude","argv":["claude","--resume"],"env":{"FOO":"bar"},"cwd":"src","hash":"9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"}],"skipped":[{"panel":"notes","reason":"not a terminal panel"}],"error":null}"#;
const SPAWNED_JSON: &str =
    r#"{"type":"spawned","request":11,"space":"/Users/me/src/trellis","terminal":"t12"}"#;
const ERROR_JSON: &str =
    r#"{"type":"error","code":"cwd_refused","message":"no","request":11,"attachment":null}"#;
const CHANGED_JSON: &str = r#"{"type":"error","code":"template_changed","message":"the panel claude changed since it was listed; review it and choose again","request":11,"attachment":null}"#;
const SPACE: &str = "/Users/me/src/trellis";
/// An example hash: the SHA-256 of `test`.
const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

fn s(text: &str) -> String {
    text.to_owned()
}

fn template() -> TemplateInfo {
    TemplateInfo {
        panel: s("claude"),
        title: Some(s("Claude")),
        slot: s("claude"),
        argv: vec![s("claude"), s("--resume")],
        env: BTreeMap::from([(s("FOO"), s("bar"))]),
        cwd: s("src"),
        hash: s(HASH),
    }
}

fn client_round_trip(json: &str, expected: ClientMessage, name: &str) {
    let frame = control_frame(json);
    let (message, used) = ClientMessage::decode(&frame).unwrap();
    assert_eq!(message, expected);
    assert_eq!(used, frame.len());
    assert_eq!(message.type_name(), name);
    assert_eq!(message.encode().unwrap(), frame);
}

fn is_malformed(json: &str) -> bool {
    matches!(
        ClientMessage::decode(&control_frame(json)),
        Err(DecodeError::BadJson { .. })
    )
}

#[test]
fn list_templates_round_trips() {
    client_round_trip(
        LIST_JSON,
        ClientMessage::ListTemplates(ListTemplates {
            request: RequestId(9),
            space: s(SPACE),
        }),
        "list_templates",
    );
}

#[test]
fn spawn_terminal_shell_and_template_round_trip() {
    client_round_trip(
        SHELL_JSON,
        ClientMessage::SpawnTerminal(SpawnTerminal {
            request: RequestId(10),
            space: s(SPACE),
            source: SpawnSource::Shell,
        }),
        "spawn_terminal",
    );
    client_round_trip(
        TEMPLATE_JSON,
        ClientMessage::SpawnTerminal(SpawnTerminal {
            request: RequestId(11),
            space: s(SPACE),
            source: SpawnSource::Template {
                panel: s("claude"),
                hash: s(HASH),
            },
        }),
        "spawn_terminal",
    );
}

#[test]
fn templates_round_trips_with_null_title_and_skips() {
    let frame = control_frame(TEMPLATES_JSON);
    let (message, used) = DaemonMessage::decode(&frame).unwrap();
    assert_eq!(
        message,
        DaemonMessage::Templates(Templates {
            request: RequestId(9),
            space: s(SPACE),
            templates: vec![template()],
            skipped: vec![SkippedPanel {
                panel: s("notes"),
                reason: s("not a terminal panel"),
            }],
            error: None,
        })
    );
    assert_eq!(used, frame.len());
    assert_eq!(message.type_name(), "templates");
    assert_eq!(message.encode().unwrap(), frame);

    let untitled = DaemonMessage::Templates(Templates {
        request: RequestId(9),
        space: s(SPACE),
        templates: vec![TemplateInfo {
            title: None,
            ..template()
        }],
        skipped: vec![],
        error: Some(s("couldn't list")),
    });
    let encoded = untitled.encode().unwrap();
    let json = std::str::from_utf8(&encoded[5..]).unwrap();
    assert!(json.contains(r#""title":null"#), "{json}");
    assert!(json.contains(r#""error":"couldn't list""#), "{json}");
    assert_eq!(DaemonMessage::decode(&encoded).unwrap().0, untitled);
}

#[test]
fn spawned_round_trips() {
    let frame = control_frame(SPAWNED_JSON);
    let (message, _) = DaemonMessage::decode(&frame).unwrap();
    assert_eq!(
        message,
        DaemonMessage::Spawned(Spawned {
            request: RequestId(11),
            space: s(SPACE),
            terminal: TermId(s("t12")),
        })
    );
    assert_eq!(message.type_name(), "spawned");
    assert_eq!(message.encode().unwrap(), frame);
}

#[test]
fn unknown_source_type_is_malformed() {
    let json = r#"{"type":"spawn_terminal","request":1,"space":"s","source":{"type":"vim"}}"#;
    assert!(is_malformed(json));
    let long = "x".repeat(200);
    let json = format!(
        r#"{{"type":"spawn_terminal","request":1,"space":"s","source":{{"type":"{long}"}}}}"#
    );
    match ClientMessage::decode(&control_frame(&json)) {
        Err(DecodeError::BadJson { detail }) => {
            assert!(detail.contains(&format!("unknown spawn source `{}`", "x".repeat(64))));
            assert!(!detail.contains(&"x".repeat(65)), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn template_source_without_panel_is_malformed() {
    let json = r#"{"type":"spawn_terminal","request":1,"space":"s","source":{"type":"template"}}"#;
    match ClientMessage::decode(&control_frame(json)) {
        Err(DecodeError::BadJson { detail }) => {
            assert!(detail.contains("`template` is missing `panel`"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn template_source_without_hash_is_malformed() {
    let json = r#"{"type":"spawn_terminal","request":1,"space":"s","source":{"type":"template","panel":"claude"}}"#;
    match ClientMessage::decode(&control_frame(json)) {
        Err(DecodeError::BadJson { detail }) => {
            assert!(detail.contains("`template` is missing `hash`"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn template_info_without_hash_is_malformed() {
    let json = TEMPLATES_JSON.replace(&format!(r#","hash":"{HASH}""#), "");
    assert!(!json.contains("hash"), "{json}");
    assert!(DaemonMessage::decode(&control_frame(&json)).is_err());
}

#[test]
fn new_error_codes_round_trip() {
    for (code, name) in [
        (ErrorCode::UnknownTemplate, "unknown_template"),
        (ErrorCode::TemplateChanged, "template_changed"),
        (ErrorCode::CwdRefused, "cwd_refused"),
        (ErrorCode::SpawnFailed, "spawn_failed"),
    ] {
        let message = DaemonMessage::Error(ErrorMessage {
            code,
            message: s("no"),
            request: Some(RequestId(11)),
            attachment: None,
        });
        let frame = message.encode().unwrap();
        let json = std::str::from_utf8(&frame[5..]).unwrap();
        assert!(json.contains(&format!(r#""code":"{name}""#)), "{json}");
        assert_eq!(DaemonMessage::decode(&frame).unwrap().0, message);
    }
    for json in [ERROR_JSON, CHANGED_JSON] {
        let frame = control_frame(json);
        let (message, _) = DaemonMessage::decode(&frame).unwrap();
        assert_eq!(message.encode().unwrap(), frame);
    }
    let (changed, _) = DaemonMessage::decode(&control_frame(CHANGED_JSON)).unwrap();
    assert!(matches!(
        changed,
        DaemonMessage::Error(ErrorMessage {
            code: ErrorCode::TemplateChanged,
            request: Some(RequestId(11)),
            ..
        })
    ));
}

#[test]
fn templates_and_spawned_are_control_traffic() {
    for json in [TEMPLATES_JSON, SPAWNED_JSON] {
        let (message, _) = DaemonMessage::decode(&control_frame(json)).unwrap();
        assert_eq!(message.stream_attachment(), None);
        assert_eq!(message.cursor(), None);
    }
}
