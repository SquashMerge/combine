//! Protocol version 6: `request_ownership`'s optional `if_vacant`, which makes a claim
//! conditional on the terminal still having no owner under the generation the client saw.

mod common;

use combine_protocol::*;
use common::*;

fn request(if_vacant: Option<u64>) -> ClientMessage {
    ClientMessage::RequestOwnership(RequestOwnership {
        attachment: AttachmentId(7),
        if_vacant: if_vacant.map(OwnershipGeneration),
    })
}

#[test]
fn a_conditional_claim_round_trips() {
    let golden = control_frame(r#"{"type":"request_ownership","attachment":7,"if_vacant":12}"#);
    let message = request(Some(12));
    assert_eq!(message.encode().unwrap(), golden);
    assert_eq!(
        ClientMessage::decode(&golden).unwrap(),
        (message, golden.len())
    );
}

#[test]
fn a_manual_request_leaves_if_vacant_out_and_a_null_reads_as_manual() {
    assert_eq!(
        request(None).encode().unwrap(),
        control_frame(r#"{"type":"request_ownership","attachment":7}"#)
    );
    let frame = control_frame(r#"{"type":"request_ownership","attachment":7,"if_vacant":null}"#);
    assert_eq!(ClientMessage::decode(&frame).unwrap().0, request(None));
}

#[test]
fn a_mistyped_if_vacant_is_bad_json() {
    for value in [
        r#""12""#,
        "-1",
        "1.5",
        "true",
        "[12]",
        "18446744073709551616",
    ] {
        let frame = control_frame(&format!(
            r#"{{"type":"request_ownership","attachment":7,"if_vacant":{value}}}"#
        ));
        assert!(
            matches!(
                ClientMessage::decode(&frame),
                Err(DecodeError::BadJson { .. })
            ),
            "{value}"
        );
    }
}
