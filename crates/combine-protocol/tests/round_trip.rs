mod common;

use std::io::Cursor as IoCursor;

use combine_protocol::*;
use common::*;

/// Asserts the golden bytes, both decoders, the stream path and `type_name`.
macro_rules! round_trip {
    ($goldens:expr, $key:expr, $tag:expr, $message:ty) => {{
        let (_, msg, golden) = $goldens
            .into_iter()
            .find(|(key, _, _)| *key == $key)
            .expect("golden exists");
        assert_eq!(msg.encode().unwrap(), golden);
        assert_eq!(
            <$message>::decode(&golden).unwrap(),
            (msg.clone(), golden.len())
        );
        let mut wire = Vec::new();
        write_message(&mut wire, &msg).unwrap();
        assert_eq!(wire, golden);
        let back: Option<$message> = read_message(&mut IoCursor::new(wire)).unwrap();
        assert_eq!(back, Some(msg.clone()));
        assert_eq!(msg.type_name(), $tag);
    }};
}

#[test]
fn hello_round_trips() {
    round_trip!(client_goldens(), "hello", "hello", ClientMessage);
}

#[test]
fn attach_round_trips() {
    round_trip!(client_goldens(), "attach", "attach", ClientMessage);
}

#[test]
fn resume_round_trips() {
    round_trip!(client_goldens(), "resume", "resume", ClientMessage);
}

#[test]
fn detach_round_trips() {
    round_trip!(client_goldens(), "detach", "detach", ClientMessage);
}

#[test]
fn ack_round_trips() {
    round_trip!(client_goldens(), "ack", "ack", ClientMessage);
}

#[test]
fn resize_round_trips() {
    round_trip!(client_goldens(), "resize", "resize", ClientMessage);
}

#[test]
fn focus_round_trips() {
    round_trip!(client_goldens(), "focus", "focus", ClientMessage);
}

#[test]
fn set_display_round_trips() {
    round_trip!(
        client_goldens(),
        "set_display",
        "set_display",
        ClientMessage
    );
}

#[test]
fn request_ownership_round_trips() {
    round_trip!(
        client_goldens(),
        "request_ownership",
        "request_ownership",
        ClientMessage
    );
}

#[test]
fn input_round_trips() {
    round_trip!(client_goldens(), "input", "input", ClientMessage);
}

#[test]
fn automatic_reply_round_trips() {
    round_trip!(
        client_goldens(),
        "automatic_reply",
        "automatic_reply",
        ClientMessage
    );
}

#[test]
fn welcome_round_trips() {
    round_trip!(daemon_goldens(), "welcome", "welcome", DaemonMessage);
}

#[test]
fn attached_round_trips() {
    round_trip!(daemon_goldens(), "attached", "attached", DaemonMessage);
}

#[test]
fn resumed_round_trips() {
    round_trip!(daemon_goldens(), "resumed", "resumed", DaemonMessage);
}

#[test]
fn resumed_with_nothing_to_replay_round_trips() {
    round_trip!(daemon_goldens(), "resumed_none", "resumed", DaemonMessage);
}

#[test]
fn gap_round_trips() {
    round_trip!(daemon_goldens(), "gap", "gap", DaemonMessage);
}

#[test]
fn gap_answering_resume_round_trips() {
    round_trip!(
        daemon_goldens(),
        "gap_answering_resume",
        "gap",
        DaemonMessage
    );
}

#[test]
fn ownership_round_trips() {
    round_trip!(daemon_goldens(), "ownership", "ownership", DaemonMessage);
}

#[test]
fn ownership_with_no_owner_round_trips() {
    round_trip!(
        daemon_goldens(),
        "ownership_none",
        "ownership",
        DaemonMessage
    );
}

#[test]
fn error_round_trips() {
    round_trip!(daemon_goldens(), "error", "error", DaemonMessage);
}

#[test]
fn started_round_trips() {
    round_trip!(daemon_goldens(), "started", "started", DaemonMessage);
}

#[test]
fn data_round_trips() {
    round_trip!(daemon_goldens(), "data", "data", DaemonMessage);
}

#[test]
fn resized_round_trips() {
    round_trip!(daemon_goldens(), "resized", "resized", DaemonMessage);
}

#[test]
fn facts_round_trips() {
    round_trip!(daemon_goldens(), "facts", "facts", DaemonMessage);
}

#[test]
fn resynced_round_trips() {
    round_trip!(daemon_goldens(), "resynced", "resynced", DaemonMessage);
}

#[test]
fn closed_round_trips() {
    round_trip!(daemon_goldens(), "closed", "closed", DaemonMessage);
}

#[test]
fn lost_round_trips() {
    round_trip!(daemon_goldens(), "lost", "lost", DaemonMessage);
}

#[test]
fn frame_header_is_big_endian_length_counting_kind_and_payload() {
    let json = format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION}}}"#);
    let mut golden = vec![0x00, 0x00, 0x00, 0x26, 0x01];
    golden.extend_from_slice(json.as_bytes());
    let hello = ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
    });
    assert_eq!(hello.encode().unwrap(), golden);
    assert_eq!(json.len() + 1, 0x26);
}

#[test]
fn largest_data_frame_fits_the_cap_exactly() {
    let msg = DaemonMessage::Data(Data {
        cursor: cursor(1, 1),
        bytes: vec![0xab; MAX_DATA_CHUNK],
    });
    let frame = msg.encode().unwrap();
    assert_eq!(frame.len(), 4 + (1 << 20));
    assert_eq!(frame[..4], [0x00, 0x10, 0x00, 0x00]);
    assert_eq!(DaemonMessage::decode(&frame).unwrap(), (msg, frame.len()));
}

#[test]
fn data_one_byte_over_the_chunk_is_an_encode_error() {
    let msg = DaemonMessage::Data(Data {
        cursor: cursor(1, 1),
        bytes: vec![0; MAX_DATA_CHUNK + 1],
    });
    assert_eq!(
        msg.encode(),
        Err(EncodeError::FrameTooLarge {
            length: (1 << 20) + 1,
            max: MAX_FRAME_LEN
        })
    );
}

#[test]
fn split_data_uses_one_seq_per_frame() {
    let input: Vec<u8> = (0..2 * MAX_DATA_CHUNK + 5).map(|i| i as u8).collect();
    let events = Data::split(cursor(4, 10), &input).unwrap();
    let seqs: Vec<u64> = events.iter().map(|d| d.cursor.seq.0).collect();
    assert_eq!(seqs, [10, 11, 12]);
    assert!(events
        .iter()
        .all(|d| d.cursor.attachment == AttachmentId(7) && d.cursor.epoch == Epoch(4)));
    let sizes: Vec<usize> = events.iter().map(|d| d.bytes.len()).collect();
    assert_eq!(sizes, [MAX_DATA_CHUNK, MAX_DATA_CHUNK, 5]);
    let joined: Vec<u8> = events.iter().flat_map(|d| d.bytes.clone()).collect();
    assert_eq!(joined, input);
    for event in events {
        let frame = DaemonMessage::Data(event).encode().unwrap();
        assert!(frame.len() <= 4 + (1 << 20));
    }
}

#[test]
fn split_data_of_nothing_is_no_frames() {
    assert_eq!(Data::split(cursor(4, 10), &[]).unwrap(), Vec::new());
}

#[test]
fn split_data_rejects_seq_overflow() {
    let last = cursor(4, u64::MAX);
    assert_eq!(
        Data::split(last, &vec![0; MAX_DATA_CHUNK + 1]),
        Err(EncodeError::SeqOverflow)
    );
    assert_eq!(Data::split(last, b"x").unwrap().len(), 1);
}

#[test]
fn unknown_fields_are_ignored() {
    let hello = control_frame(&format!(
        r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"extra":{{"a":[1,{{"b":null}}]}}}}"#
    ));
    assert_eq!(
        ClientMessage::decode(&hello).unwrap().0,
        ClientMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION
        })
    );
    let started = control_frame(
        r#"{"type":"started","cursor":{"attachment":7,"epoch":4,"seq":10,"x":1},"backend_generation":2,"cols":80,"rows":24,"fidelity":"exact","ordered_resizes":true,"later":{"n":[1]}}"#,
    );
    let (msg, _) = DaemonMessage::decode(&started).unwrap();
    assert_eq!(msg.cursor(), Some(cursor(4, 10)));
}

#[test]
fn missing_optional_fields_decode_as_none() {
    let gap = control_frame(r#"{"type":"gap","attachment":7}"#);
    assert_eq!(
        DaemonMessage::decode(&gap).unwrap().0,
        DaemonMessage::Gap(Gap {
            attachment: AttachmentId(7),
            request: None
        })
    );
    let error = control_frame(r#"{"type":"error","code":"internal","message":"boom"}"#);
    assert_eq!(
        DaemonMessage::decode(&error).unwrap().0,
        DaemonMessage::Error(ErrorMessage {
            code: ErrorCode::Internal,
            message: "boom".into(),
            request: None,
            attachment: None
        })
    );
}

#[test]
fn get_panel_round_trips() {
    round_trip!(client_goldens(), "get_panel", "get_panel", ClientMessage);
}

#[test]
fn report_render_round_trips() {
    round_trip!(
        client_goldens(),
        "report_render",
        "report_render",
        ClientMessage
    );
}

#[test]
fn report_render_failed_round_trips() {
    round_trip!(
        client_goldens(),
        "report_render_failed",
        "report_render",
        ClientMessage
    );
}

#[test]
fn close_panel_round_trips() {
    round_trip!(
        client_goldens(),
        "close_panel",
        "close_panel",
        ClientMessage
    );
}

#[test]
fn pin_panel_round_trips() {
    round_trip!(client_goldens(), "pin_panel", "pin_panel", ClientMessage);
}

#[test]
fn mutate_layout_with_panel_ops_round_trips() {
    round_trip!(
        client_goldens(),
        "mutate_layout_panel_ops",
        "mutate_layout",
        ClientMessage
    );
}

#[test]
fn mcp_open_round_trips() {
    round_trip!(client_goldens(), "mcp_open", "mcp_open", ClientMessage);
}

#[test]
fn mcp_open_cwd_only_round_trips() {
    round_trip!(
        client_goldens(),
        "mcp_open_cwd_only",
        "mcp_open",
        ClientMessage
    );
}

#[test]
fn mcp_client_round_trips() {
    round_trip!(client_goldens(), "mcp_client", "mcp", ClientMessage);
}

#[test]
fn terminals_round_trips() {
    round_trip!(daemon_goldens(), "terminals", "terminals", DaemonMessage);
}

#[test]
fn terminals_without_a_backend_round_trips() {
    round_trip!(
        daemon_goldens(),
        "terminals_no_backend",
        "terminals",
        DaemonMessage
    );
}

#[test]
fn terminals_from_an_older_daemon_read_no_backend() {
    let json = golden_json("terminals_no_backend");
    let older = patched(&json, "/backend", None);
    assert_eq!(
        decode_daemon(&older).unwrap(),
        decode_daemon(&json).unwrap()
    );
    let nameless = patched(
        &golden_json("terminals"),
        "/backend",
        Some(serde_json::json!({"id": "backend-a"})),
    );
    match decode_daemon(&nameless).unwrap() {
        DaemonMessage::Terminals(list) => assert_eq!(
            list.backend,
            Some(BackendInfo {
                id: s("backend-a"),
                instance: None
            })
        ),
        other => panic!("expected terminals, got {other:?}"),
    }
}

#[test]
fn panels_round_trips() {
    round_trip!(daemon_goldens(), "panels", "panels", DaemonMessage);
}

#[test]
fn panels_push_round_trips() {
    round_trip!(daemon_goldens(), "panels_push", "panels", DaemonMessage);
}

#[test]
fn panel_round_trips() {
    round_trip!(daemon_goldens(), "panel", "panel", DaemonMessage);
}

#[test]
fn panel_closed_round_trips() {
    round_trip!(
        daemon_goldens(),
        "panel_closed",
        "panel_closed",
        DaemonMessage
    );
}

#[test]
fn panel_pinned_round_trips() {
    round_trip!(
        daemon_goldens(),
        "panel_pinned",
        "panel_pinned",
        DaemonMessage
    );
}

#[test]
fn mcp_opened_round_trips() {
    round_trip!(daemon_goldens(), "mcp_opened", "mcp_opened", DaemonMessage);
}

#[test]
fn mcp_daemon_round_trips() {
    round_trip!(daemon_goldens(), "mcp_daemon", "mcp", DaemonMessage);
}

#[test]
fn error_unknown_panel_round_trips() {
    round_trip!(
        daemon_goldens(),
        "error_unknown_panel",
        "error",
        DaemonMessage
    );
}

#[test]
fn error_unknown_caller_round_trips() {
    round_trip!(
        daemon_goldens(),
        "error_unknown_caller",
        "error",
        DaemonMessage
    );
}

#[test]
fn error_panel_limit_round_trips() {
    round_trip!(
        daemon_goldens(),
        "error_panel_limit",
        "error",
        DaemonMessage
    );
}

#[test]
fn error_codes_encode_as_snake_case() {
    let codes = [
        (ErrorCode::VersionMismatch, "version_mismatch"),
        (ErrorCode::Malformed, "malformed"),
        (ErrorCode::UnknownTerminal, "unknown_terminal"),
        (ErrorCode::AttachFailed, "attach_failed"),
        (ErrorCode::UnknownAttachment, "unknown_attachment"),
        (ErrorCode::StaleGeneration, "stale_generation"),
        (ErrorCode::WatchOnly, "watch_only"),
        (ErrorCode::InvalidRequest, "invalid_request"),
        (ErrorCode::UnknownPanel, "unknown_panel"),
        (ErrorCode::UnknownCaller, "unknown_caller"),
        (ErrorCode::PanelLimit, "panel_limit"),
        (ErrorCode::Internal, "internal"),
    ];
    for (code, name) in codes {
        let msg = DaemonMessage::Error(ErrorMessage {
            code,
            message: String::new(),
            request: None,
            attachment: None,
        });
        let golden = control_frame(&format!(
            r#"{{"type":"error","code":"{name}","message":"","request":null,"attachment":null}}"#
        ));
        assert_eq!(msg.encode().unwrap(), golden, "{name}");
        assert_eq!(DaemonMessage::decode(&golden).unwrap().0, msg, "{name}");
    }
}

fn started_with(fidelity: Fidelity) -> DaemonMessage {
    DaemonMessage::Started(Started {
        cursor: cursor(1, 1),
        backend_generation: BackendGeneration(1),
        cols: 1,
        rows: 1,
        fidelity,
        ordered_resizes: false,
    })
}

#[test]
fn fidelity_values_encode_as_snake_case() {
    for (fidelity, name) in [(Fidelity::Exact, "exact"), (Fidelity::Inexact, "inexact")] {
        let frame = started_with(fidelity).encode().unwrap();
        let text = String::from_utf8(frame[5..].to_vec()).unwrap();
        assert!(text.contains(&format!(r#""fidelity":"{name}""#)), "{text}");
        assert_eq!(
            DaemonMessage::decode(&frame).unwrap().0,
            started_with(fidelity)
        );
    }
}

#[test]
fn close_reasons_encode_as_snake_case() {
    for (reason, name) in [
        (CloseReason::Detached, "detached"),
        (CloseReason::Exited, "exited"),
    ] {
        let msg = DaemonMessage::Closed(Closed {
            cursor: cursor(1, 1),
            reason,
        });
        let frame = msg.encode().unwrap();
        let text = String::from_utf8(frame[5..].to_vec()).unwrap();
        assert!(text.ends_with(&format!(r#""reason":"{name}"}}"#)), "{text}");
        assert_eq!(DaemonMessage::decode(&frame).unwrap().0, msg);
    }
}

#[test]
fn cursor_helper_names_each_event_cursor() {
    let with_cursor = [
        ("started", Some(cursor(4, 10))),
        ("data", Some(cursor(4, 11))),
        ("resized", Some(cursor(4, 12))),
        ("facts", Some(cursor(4, 13))),
        ("resynced", Some(cursor(4, 14))),
        ("closed", Some(cursor(4, 15))),
        ("lost", Some(cursor(4, 16))),
        ("welcome", None),
        ("attached", None),
        ("resumed", None),
        ("resumed_none", None),
        ("gap", None),
        ("gap_answering_resume", None),
        ("ownership", None),
        ("ownership_none", None),
        ("error", None),
        ("terminals", None),
        ("terminals_no_backend", None),
        ("panels", None),
        ("panels_push", None),
        ("panel", None),
        ("panel_closed", None),
        ("panel_pinned", None),
        ("mcp_opened", None),
        ("mcp_daemon", None),
        ("error_unknown_panel", None),
        ("error_unknown_caller", None),
        ("error_panel_limit", None),
    ];
    let goldens = daemon_goldens();
    for (key, expected) in with_cursor {
        let (_, msg, _) = goldens.iter().find(|(k, _, _)| *k == key).unwrap();
        assert_eq!(msg.cursor(), expected, "{key}");
    }
}

#[test]
fn stream_attachment_marks_each_stream_message() {
    let in_stream = [
        "started", "data", "resized", "facts", "resynced", "closed", "lost", "gap",
    ];
    for (key, msg, _) in daemon_goldens() {
        let expected = in_stream.contains(&key).then_some(AttachmentId(7));
        assert_eq!(msg.stream_attachment(), expected, "{key}");
    }
}

#[test]
fn json_whitespace_is_accepted() {
    let frame = control_frame(&format!(
        " \n{{ \"type\" : \"hello\" , \"protocol_version\" : {PROTOCOL_VERSION} }}\r\n\t"
    ));
    assert_eq!(
        ClientMessage::decode(&frame).unwrap().0,
        ClientMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION
        })
    );
}

#[test]
fn several_frames_in_one_buffer_decode_in_order() {
    let goldens = daemon_goldens();
    let buf: Vec<u8> = goldens.iter().flat_map(|(_, _, g)| g.clone()).collect();
    let mut rest = buf.as_slice();
    for (key, msg, _) in &goldens {
        let (decoded, used) = DaemonMessage::decode(rest).unwrap();
        assert_eq!(&decoded, msg, "{key}");
        rest = &rest[used..];
    }
    assert!(rest.is_empty());
}

#[test]
fn stream_reader_returns_none_at_clean_end() {
    let golden = &client_goldens()[0].2;
    let mut reader = IoCursor::new(golden.clone());
    assert!(read_message::<ClientMessage, _>(&mut reader)
        .unwrap()
        .is_some());
    assert!(read_message::<ClientMessage, _>(&mut reader)
        .unwrap()
        .is_none());
    assert!(
        read_message::<ClientMessage, _>(&mut IoCursor::new(Vec::new()))
            .unwrap()
            .is_none()
    );
}
