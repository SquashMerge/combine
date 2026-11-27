//! Protocol version 6: the expected backend `instance` of `attach` and `resume`, and the
//! `backend_mismatch` error that refuses one naming another instance.

mod common;

use combine_protocol::*;
use common::*;

fn attach_json(instance: &str) -> String {
    format!(
        r#"{{"type":"attach","request":1,"terminal":"hc:t8","options":{{"cols":80,"rows":24,"display":{DISPLAY_JSON},"observe":false,"history":1000}}{instance}}}"#
    )
}

fn attach(instance: Option<&str>) -> ClientMessage {
    ClientMessage::Attach(Attach {
        request: RequestId(1),
        terminal: TermId("hc:t8".into()),
        options: AttachRequestOptions {
            cols: 80,
            rows: 24,
            display: display(),
            observe: false,
            history: 1000,
        },
        instance: instance.map(str::to_string),
    })
}

fn resume(instance: Option<&str>) -> ClientMessage {
    ClientMessage::Resume(Resume {
        request: RequestId(2),
        cursor: cursor(3, 41),
        instance: instance.map(str::to_string),
    })
}

const RESUME_CORE_A: &str = r#"{"type":"resume","request":2,"cursor":{"attachment":7,"epoch":3,"seq":41},"instance":"core-a"}"#;

#[test]
fn an_attach_naming_an_instance_round_trips() {
    let golden = control_frame(&attach_json(r#","instance":"core-a""#));
    let message = attach(Some("core-a"));
    assert_eq!(message.encode().unwrap(), golden);
    assert_eq!(
        ClientMessage::decode(&golden).unwrap(),
        (message, golden.len())
    );
}

#[test]
fn a_resume_naming_an_instance_round_trips() {
    let golden = control_frame(RESUME_CORE_A);
    let message = resume(Some("core-a"));
    assert_eq!(message.encode().unwrap(), golden);
    assert_eq!(
        ClientMessage::decode(&golden).unwrap(),
        (message, golden.len())
    );
}

#[test]
fn an_attach_or_resume_without_an_instance_leaves_it_out() {
    let attach_text = String::from_utf8(attach(None).encode().unwrap()[5..].to_vec()).unwrap();
    assert!(!attach_text.contains("instance"), "{attach_text}");
    let resume_text = String::from_utf8(resume(None).encode().unwrap()[5..].to_vec()).unwrap();
    assert!(!resume_text.contains("instance"), "{resume_text}");
}

#[test]
fn a_null_or_missing_instance_reads_as_none() {
    for tail in ["", r#","instance":null"#] {
        let frame = control_frame(&attach_json(tail));
        assert_eq!(
            ClientMessage::decode(&frame).unwrap().0,
            attach(None),
            "{tail}"
        );
    }
    let frame = control_frame(
        r#"{"type":"resume","request":2,"cursor":{"attachment":7,"epoch":3,"seq":41},"instance":null}"#,
    );
    assert_eq!(ClientMessage::decode(&frame).unwrap().0, resume(None));
}

#[test]
fn a_mistyped_instance_is_bad_json() {
    for tail in [
        r#","instance":7"#,
        r#","instance":["core-a"]"#,
        r#","instance":{}"#,
    ] {
        let frame = control_frame(&attach_json(tail));
        assert!(
            matches!(
                ClientMessage::decode(&frame),
                Err(DecodeError::BadJson { .. })
            ),
            "attach {tail}"
        );
    }
    let frame = control_frame(
        r#"{"type":"resume","request":2,"cursor":{"attachment":7,"epoch":3,"seq":41},"instance":false}"#,
    );
    assert!(matches!(
        ClientMessage::decode(&frame),
        Err(DecodeError::BadJson { .. })
    ));
}

#[test]
fn backend_mismatch_round_trips() {
    let golden = control_frame(
        r#"{"type":"error","code":"backend_mismatch","message":"the terminal belongs to another backend instance","request":1,"attachment":null}"#,
    );
    let message = DaemonMessage::Error(ErrorMessage {
        code: ErrorCode::BackendMismatch,
        message: "the terminal belongs to another backend instance".into(),
        request: Some(RequestId(1)),
        attachment: None,
    });
    assert_eq!(message.encode().unwrap(), golden);
    assert_eq!(
        DaemonMessage::decode(&golden).unwrap(),
        (message, golden.len())
    );
}

#[test]
fn backend_mismatch_is_daemon_only_and_unknown_codes_stay_bad_json() {
    let frame = control_frame(
        r#"{"type":"error","code":"backend_mismatched","message":"","request":1,"attachment":null}"#,
    );
    assert!(matches!(
        DaemonMessage::decode(&frame),
        Err(DecodeError::BadJson { .. })
    ));
    let frame = control_frame(
        r#"{"type":"error","code":"backend_mismatch","message":"","request":1,"attachment":null}"#,
    );
    assert!(matches!(
        ClientMessage::decode(&frame),
        Err(DecodeError::UnknownMessageType { .. })
    ));
}
