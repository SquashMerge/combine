//! Protocol version 7: the version constant and the mismatch text a version 7
//! peer reads and writes.

use combine_protocol::*;

const MISMATCH_7_6: &str = "protocol version mismatch: client 7, daemon 6";
const MISMATCH_6_7: &str = "protocol version mismatch: client 6, daemon 7";
const REFUSAL_7_6: &str = r#"{"type":"error","code":"version_mismatch","message":"protocol version mismatch: client 7, daemon 6","request":null,"attachment":null}"#;
const REFUSAL_6_7: &str = r#"{"type":"error","code":"version_mismatch","message":"protocol version mismatch: client 6, daemon 7","request":null,"attachment":null}"#;
const HELLO_7: &str = r#"{"type":"hello","protocol_version":7}"#;
const HELLO_6: &str = r#"{"type":"hello","protocol_version":6}"#;
const HOST: &str = "3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b";

/// `be32(1 + json.len()) ++ 0x01 ++ json`.
fn frame(json: &str) -> Vec<u8> {
    let mut bytes = ((json.len() + 1) as u32).to_be_bytes().to_vec();
    bytes.push(1);
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

#[test]
fn protocol_version_is_7() {
    assert_eq!(PROTOCOL_VERSION, 7);
}

#[test]
fn mismatch_text_reads_client_7_daemon_6() {
    let mismatch = VersionMismatch {
        client: 7,
        daemon: 6,
    };
    assert_eq!(mismatch.to_string(), MISMATCH_7_6);
    let error = ErrorMessage::from(mismatch);
    assert_eq!(error.message, MISMATCH_7_6);
    assert_eq!(error.code, ErrorCode::VersionMismatch);
    assert_eq!(
        answer_hello(
            &Hello {
                protocol_version: 6
            },
            &HostId(HOST.into())
        ),
        Err(VersionMismatch {
            client: 6,
            daemon: 7
        })
    );
}

#[test]
fn a_v7_client_meeting_a_v6_welcome_reads_client_7_daemon_6() {
    let welcome = DaemonMessage::Welcome(Welcome {
        protocol_version: 6,
        host_id: HostId(HOST.into()),
    })
    .encode()
    .unwrap();
    let mut reader = &welcome[..];
    let mut writer = Vec::new();
    let error = client_handshake(&mut reader, &mut writer).unwrap_err();
    assert!(matches!(
        error,
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 7,
            daemon: 6
        })
    ));
    assert_eq!(error.to_string(), MISMATCH_7_6);
}

#[test]
fn a_v7_client_shows_a_v6_daemons_refusal_as_client_7_daemon_6() {
    let refusal = frame(REFUSAL_7_6);
    let mut reader = &refusal[..];
    let mut writer = Vec::new();
    let error = client_handshake(&mut reader, &mut writer).unwrap_err();
    assert_eq!(writer, frame(HELLO_7));
    match &error {
        HandshakeError::Refused(refused) => {
            assert_eq!(refused.code, ErrorCode::VersionMismatch)
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(error.to_string(), MISMATCH_7_6);
}

#[test]
fn a_v7_daemon_refuses_a_v6_hello_as_client_6_daemon_7() {
    let hello = frame(HELLO_6);
    let mut reader = &hello[..];
    let mut writer = Vec::new();
    let error = daemon_handshake(&mut reader, &mut writer, &HostId(HOST.into())).unwrap_err();
    assert!(matches!(
        error,
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 6,
            daemon: 7
        })
    ));
    assert_eq!(error.to_string(), MISMATCH_6_7);
    assert_eq!(writer, frame(REFUSAL_6_7));
}
