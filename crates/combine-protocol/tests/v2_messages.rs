//! Protocol version 2: the terminal list messages, the version check and the
//! trimming of a peer's error text.

mod common;

use combine_protocol::*;
use common::control_frame;

const WATCH_JSON: &str = r#"{"type":"watch_terminals","request":7}"#;
const TERMINALS_JSON: &str = r#"{"type":"terminals","request":7,"terminals":[{"id":"hc:t8","cwd":"/Users/me/src/trellis/crates","argv":["claude"],"state":"running","exit_code":null,"labels":{},"cols":120,"rows":40},{"id":"hc:t9","cwd":"/Users/me/src/trellis","argv":["zsh"],"state":"exited","exit_code":0,"labels":{"slot":"prime-1"},"cols":80,"rows":24}],"spaces":[{"id":"/Users/me/src/trellis","kind":"repository","terminals":["hc:t8","hc:t9"]}]}"#;

fn terminals() -> Terminals {
    Terminals {
        request: Some(RequestId(7)),
        terminals: vec![
            TerminalInfo {
                id: TermId("hc:t8".into()),
                cwd: "/Users/me/src/trellis/crates".into(),
                argv: vec!["claude".into()],
                state: TerminalState::Running,
                exit_code: None,
                labels: Default::default(),
                cols: 120,
                rows: 40,
            },
            TerminalInfo {
                id: TermId("hc:t9".into()),
                cwd: "/Users/me/src/trellis".into(),
                argv: vec!["zsh".into()],
                state: TerminalState::Exited,
                exit_code: Some(0),
                labels: [("slot".to_string(), "prime-1".to_string())].into(),
                cols: 80,
                rows: 24,
            },
        ],
        spaces: vec![SpaceInfo {
            id: "/Users/me/src/trellis".into(),
            kind: SpaceKind::Repository,
            terminals: vec![TermId("hc:t8".into()), TermId("hc:t9".into())],
        }],
    }
}

fn error_with(message: String) -> ErrorMessage {
    ErrorMessage {
        code: ErrorCode::Internal,
        message,
        request: None,
        attachment: None,
    }
}

fn hello_frame(version: u32) -> Vec<u8> {
    ClientMessage::Hello(Hello {
        protocol_version: version,
    })
    .encode()
    .unwrap()
}

#[test]
fn protocol_version_is_2() {
    assert_eq!(PROTOCOL_VERSION, 2);
}

#[test]
fn watch_terminals_decodes_from_the_spec_example() {
    let (message, used) = ClientMessage::decode(&control_frame(WATCH_JSON)).unwrap();
    assert_eq!(
        message,
        ClientMessage::WatchTerminals(WatchTerminals {
            request: RequestId(7)
        })
    );
    assert_eq!(used, control_frame(WATCH_JSON).len());
    assert_eq!(message.type_name(), "watch_terminals");
    assert_eq!(message.encode().unwrap(), control_frame(WATCH_JSON));
}

#[test]
fn terminals_decodes_from_the_spec_example_and_round_trips() {
    let frame = control_frame(TERMINALS_JSON);
    let (message, used) = DaemonMessage::decode(&frame).unwrap();
    assert_eq!(used, frame.len());
    assert_eq!(message, DaemonMessage::Terminals(terminals()));
    assert_eq!(message.type_name(), "terminals");
    let again = DaemonMessage::decode(&message.encode().unwrap()).unwrap().0;
    assert_eq!(again, message);
}

#[test]
fn terminals_encodes_fields_in_spec_order() {
    let frame = DaemonMessage::Terminals(terminals()).encode().unwrap();
    assert_eq!(frame, control_frame(TERMINALS_JSON));
}

#[test]
fn terminals_is_control_traffic() {
    let message = DaemonMessage::Terminals(terminals());
    assert_eq!(message.cursor(), None);
    assert_eq!(message.stream_attachment(), None);
    let pushed = DaemonMessage::Terminals(Terminals {
        request: None,
        ..terminals()
    });
    assert_eq!(pushed.stream_attachment(), None);
}

#[test]
fn exited_terminal_without_exit_code_reads_null() {
    let info = TerminalInfo {
        state: TerminalState::Exited,
        exit_code: None,
        ..terminals().terminals.remove(0)
    };
    let json = serde_json::to_string(&info).unwrap();
    assert!(
        json.contains(r#""state":"exited","exit_code":null"#),
        "{json}"
    );
    let back: TerminalInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back, info);
}

#[test]
fn daemon_refuses_a_v1_hello_with_the_exact_text() {
    let host = HostId("3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b".into());
    let mut reader = &hello_frame(1)[..];
    let mut writer = Vec::new();
    let error = daemon_handshake(&mut reader, &mut writer, &host).unwrap_err();
    assert!(matches!(
        error,
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 1,
            daemon: 2
        })
    ));
    match DaemonMessage::decode(&writer).unwrap().0 {
        DaemonMessage::Error(e) => {
            assert_eq!(e.code, ErrorCode::VersionMismatch);
            assert_eq!(e.message, "protocol version mismatch: client 1, daemon 2");
        }
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn client_handshake_reports_a_v1_daemon_as_mismatch() {
    let welcome = DaemonMessage::Welcome(Welcome {
        protocol_version: 1,
        host_id: HostId("3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b".into()),
    })
    .encode()
    .unwrap();
    let mut reader = &welcome[..];
    let mut writer = Vec::new();
    assert!(matches!(
        client_handshake(&mut reader, &mut writer).unwrap_err(),
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 2,
            daemon: 1
        })
    ));
}

#[test]
fn error_message_trimmed_message_keeps_at_most_max_error_detail_bytes() {
    let short = error_with("short".into());
    assert_eq!(short.trimmed_message(), "short");
    let exact = error_with("x".repeat(MAX_ERROR_DETAIL));
    assert_eq!(exact.trimmed_message().len(), MAX_ERROR_DETAIL);
    let long = error_with("x".repeat(MAX_ERROR_DETAIL + 500));
    assert_eq!(long.trimmed_message(), "x".repeat(MAX_ERROR_DETAIL));
}

#[test]
fn trimmed_message_cuts_on_a_char_boundary() {
    let long = error_with("€".repeat(100));
    assert_eq!(long.trimmed_message(), "€".repeat(85));
}

#[test]
fn refused_handshake_display_is_trimmed() {
    let error = HandshakeError::Refused(error_with("y".repeat(10_000)));
    assert_eq!(error.to_string(), "y".repeat(MAX_ERROR_DETAIL));
}
