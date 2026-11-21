use combine_protocol::*;

const MISMATCH_JSON: &str = r#"{"type":"error","code":"version_mismatch","message":"protocol version mismatch: client 2, daemon 1","request":null,"attachment":null}"#;

fn host() -> HostId {
    HostId("3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b".into())
}

fn frame(json: &str) -> Vec<u8> {
    let mut out = ((json.len() + 1) as u32).to_be_bytes().to_vec();
    out.push(1);
    out.extend_from_slice(json.as_bytes());
    out
}

fn client_stream(messages: &[ClientMessage]) -> Vec<u8> {
    messages.iter().flat_map(|m| m.encode().unwrap()).collect()
}

fn daemon_stream(messages: &[DaemonMessage]) -> Vec<u8> {
    messages.iter().flat_map(|m| m.encode().unwrap()).collect()
}

fn hello(version: u32) -> ClientMessage {
    ClientMessage::Hello(Hello {
        protocol_version: version,
    })
}

fn welcome(version: u32) -> DaemonMessage {
    DaemonMessage::Welcome(Welcome {
        protocol_version: version,
        host_id: host(),
    })
}

#[test]
fn client_handshake_sends_hello_first_and_returns_welcome() {
    let mut reader = &daemon_stream(&[welcome(1)])[..];
    let mut writer = Vec::new();
    let got = client_handshake(&mut reader, &mut writer).unwrap();
    assert_eq!(
        got,
        Welcome {
            protocol_version: 1,
            host_id: host()
        }
    );
    assert_eq!(writer, hello(1).encode().unwrap());
}

#[test]
fn daemon_handshake_accepts_matching_hello_and_sends_welcome() {
    let mut reader = &client_stream(&[hello(1)])[..];
    let mut writer = Vec::new();
    let got = daemon_handshake(&mut reader, &mut writer, &host()).unwrap();
    assert_eq!(
        got,
        Hello {
            protocol_version: 1
        }
    );
    assert_eq!(writer, welcome(1).encode().unwrap());
}

#[test]
fn daemon_handshake_refuses_other_version_with_both_versions_named() {
    let mut reader = &client_stream(&[hello(2)])[..];
    let mut writer = Vec::new();
    let error = daemon_handshake(&mut reader, &mut writer, &host()).unwrap_err();
    assert!(matches!(
        error,
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 2,
            daemon: 1
        })
    ));
    assert_eq!(writer, frame(MISMATCH_JSON));
}

#[test]
fn client_handshake_reports_daemon_refusal() {
    let refusal = DaemonMessage::Error(
        VersionMismatch {
            client: 1,
            daemon: 2,
        }
        .into(),
    );
    let mut reader = &daemon_stream(&[refusal])[..];
    let mut writer = Vec::new();
    match client_handshake(&mut reader, &mut writer).unwrap_err() {
        HandshakeError::Refused(e) => {
            assert_eq!(e.code, ErrorCode::VersionMismatch);
            assert_eq!(e.message, "protocol version mismatch: client 1, daemon 2");
        }
        other => panic!("expected Refused, got {other:?}"),
    }
}

#[test]
fn client_handshake_rejects_welcome_with_other_version() {
    let mut reader = &daemon_stream(&[welcome(2)])[..];
    let mut writer = Vec::new();
    assert!(matches!(
        client_handshake(&mut reader, &mut writer).unwrap_err(),
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 1,
            daemon: 2
        })
    ));
}

#[test]
fn version_mismatch_message_names_both_versions() {
    let mismatch = VersionMismatch {
        client: 2,
        daemon: 1,
    };
    assert_eq!(
        mismatch.to_string(),
        "protocol version mismatch: client 2, daemon 1"
    );
    let message = ErrorMessage::from(mismatch);
    assert_eq!(message.code, ErrorCode::VersionMismatch);
    assert_eq!(message.message, mismatch.to_string());
    assert_eq!(message.request, None);
    assert_eq!(message.attachment, None);
}

#[test]
fn daemon_handshake_rejects_non_hello_first_message() {
    let attach = ClientMessage::Focus(Focus {
        attachment: AttachmentId(7),
    });
    let mut reader = &client_stream(&[attach])[..];
    let mut writer = Vec::new();
    assert!(matches!(
        daemon_handshake(&mut reader, &mut writer, &host()).unwrap_err(),
        HandshakeError::UnexpectedMessage { type_name: "focus" }
    ));
    assert!(writer.is_empty());
}

#[test]
fn client_handshake_rejects_non_welcome_reply() {
    let reply = DaemonMessage::Attached(Attached {
        request: RequestId(1),
        attachment: AttachmentId(7),
    });
    let mut reader = &daemon_stream(&[reply])[..];
    let mut writer = Vec::new();
    assert!(matches!(
        client_handshake(&mut reader, &mut writer).unwrap_err(),
        HandshakeError::UnexpectedMessage {
            type_name: "attached"
        }
    ));
}

#[test]
fn handshake_on_closed_stream_is_closed() {
    let mut writer = Vec::new();
    assert!(matches!(
        daemon_handshake(&mut &[][..], &mut writer, &host()).unwrap_err(),
        HandshakeError::Closed
    ));
    assert!(writer.is_empty());
    assert!(matches!(
        client_handshake(&mut &[][..], &mut writer).unwrap_err(),
        HandshakeError::Closed
    ));
}
