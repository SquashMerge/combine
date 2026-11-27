//! Protocol version 6: the version constant and the mismatch text a version 6
//! peer reads and writes.

use combine_protocol::*;

const MISMATCH_6_5: &str = "protocol version mismatch: client 6, daemon 5";
const MISMATCH_5_6: &str = "protocol version mismatch: client 5, daemon 6";
const REFUSAL_6_5: &str = r#"{"type":"error","code":"version_mismatch","message":"protocol version mismatch: client 6, daemon 5","request":null,"attachment":null}"#;
const REFUSAL_5_6: &str = r#"{"type":"error","code":"version_mismatch","message":"protocol version mismatch: client 5, daemon 6","request":null,"attachment":null}"#;
const HELLO_6: &str = r#"{"type":"hello","protocol_version":6}"#;
const HELLO_5: &str = r#"{"type":"hello","protocol_version":5}"#;
const HOST: &str = "3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b";

/// `be32(1 + json.len()) ++ 0x01 ++ json`.
fn frame(json: &str) -> Vec<u8> {
    let mut bytes = ((json.len() + 1) as u32).to_be_bytes().to_vec();
    bytes.push(1);
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

#[test]
fn protocol_version_is_6() {
    assert_eq!(PROTOCOL_VERSION, 6);
}

#[test]
fn mismatch_text_reads_client_6_daemon_5() {
    let mismatch = VersionMismatch {
        client: 6,
        daemon: 5,
    };
    assert_eq!(mismatch.to_string(), MISMATCH_6_5);
    let error = ErrorMessage::from(mismatch);
    assert_eq!(error.message, MISMATCH_6_5);
    assert_eq!(error.code, ErrorCode::VersionMismatch);
    assert_eq!(
        answer_hello(
            &Hello {
                protocol_version: 5
            },
            &HostId(HOST.into())
        ),
        Err(VersionMismatch {
            client: 5,
            daemon: 6
        })
    );
}

#[test]
fn a_v6_client_meeting_a_v5_welcome_reads_client_6_daemon_5() {
    let welcome = DaemonMessage::Welcome(Welcome {
        protocol_version: 5,
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
            client: 6,
            daemon: 5
        })
    ));
    assert_eq!(error.to_string(), MISMATCH_6_5);
}

#[test]
fn a_v6_client_shows_a_v5_daemons_refusal_as_client_6_daemon_5() {
    let refusal = frame(REFUSAL_6_5);
    let mut reader = &refusal[..];
    let mut writer = Vec::new();
    let error = client_handshake(&mut reader, &mut writer).unwrap_err();
    assert_eq!(writer, frame(HELLO_6));
    match &error {
        HandshakeError::Refused(refused) => {
            assert_eq!(refused.code, ErrorCode::VersionMismatch)
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(error.to_string(), MISMATCH_6_5);
}

#[test]
fn a_v6_daemon_refuses_a_v5_hello_as_client_5_daemon_6() {
    let hello = frame(HELLO_5);
    let mut reader = &hello[..];
    let mut writer = Vec::new();
    let error = daemon_handshake(&mut reader, &mut writer, &HostId(HOST.into())).unwrap_err();
    assert!(matches!(
        error,
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 5,
            daemon: 6
        })
    ));
    assert_eq!(error.to_string(), MISMATCH_5_6);
    assert_eq!(writer, frame(REFUSAL_5_6));
}
