//! Protocol version 5: the version constant and the mismatch text a version 5
//! peer reads and writes.

use combine_protocol::*;

const MISMATCH_5_4: &str = "protocol version mismatch: client 5, daemon 4";
const MISMATCH_4_5: &str = "protocol version mismatch: client 4, daemon 5";
const REFUSAL_5_4: &str = r#"{"type":"error","code":"version_mismatch","message":"protocol version mismatch: client 5, daemon 4","request":null,"attachment":null}"#;
const REFUSAL_4_5: &str = r#"{"type":"error","code":"version_mismatch","message":"protocol version mismatch: client 4, daemon 5","request":null,"attachment":null}"#;
const HELLO_5: &str = r#"{"type":"hello","protocol_version":5}"#;
const HELLO_4: &str = r#"{"type":"hello","protocol_version":4}"#;
const HOST: &str = "3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b";

/// `be32(1 + json.len()) ++ 0x01 ++ json`.
fn frame(json: &str) -> Vec<u8> {
    let mut bytes = ((json.len() + 1) as u32).to_be_bytes().to_vec();
    bytes.push(1);
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

#[test]
fn protocol_version_is_5() {
    assert_eq!(PROTOCOL_VERSION, 5);
}

#[test]
fn mismatch_text_reads_client_5_daemon_4() {
    let mismatch = VersionMismatch {
        client: 5,
        daemon: 4,
    };
    assert_eq!(mismatch.to_string(), MISMATCH_5_4);
    let error = ErrorMessage::from(mismatch);
    assert_eq!(error.message, MISMATCH_5_4);
    assert_eq!(error.code, ErrorCode::VersionMismatch);
    assert_eq!(
        answer_hello(
            &Hello {
                protocol_version: 4
            },
            &HostId(HOST.into())
        ),
        Err(VersionMismatch {
            client: 4,
            daemon: 5
        })
    );
}

#[test]
fn a_v5_client_meeting_a_v4_welcome_reads_client_5_daemon_4() {
    let welcome = DaemonMessage::Welcome(Welcome {
        protocol_version: 4,
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
            client: 5,
            daemon: 4
        })
    ));
    assert_eq!(error.to_string(), MISMATCH_5_4);
}

#[test]
fn a_v5_client_shows_a_v4_daemons_refusal_as_client_5_daemon_4() {
    let refusal = frame(REFUSAL_5_4);
    let mut reader = &refusal[..];
    let mut writer = Vec::new();
    let error = client_handshake(&mut reader, &mut writer).unwrap_err();
    assert_eq!(writer, frame(HELLO_5));
    match &error {
        HandshakeError::Refused(refused) => {
            assert_eq!(refused.code, ErrorCode::VersionMismatch)
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(error.to_string(), MISMATCH_5_4);
}

#[test]
fn a_v5_daemon_refuses_a_v4_hello_as_client_4_daemon_5() {
    let hello = frame(HELLO_4);
    let mut reader = &hello[..];
    let mut writer = Vec::new();
    let error = daemon_handshake(&mut reader, &mut writer, &HostId(HOST.into())).unwrap_err();
    assert!(matches!(
        error,
        HandshakeError::VersionMismatch(VersionMismatch {
            client: 4,
            daemon: 5
        })
    ));
    assert_eq!(error.to_string(), MISMATCH_4_5);
    assert_eq!(writer, frame(REFUSAL_4_5));
}
