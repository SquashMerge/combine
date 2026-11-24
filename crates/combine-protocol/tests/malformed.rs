mod common;

use std::io::{self, Read};

use combine_protocol::*;
use common::*;

fn bad_json(detail: &str) -> DecodeError {
    DecodeError::BadJson {
        detail: detail.to_owned(),
    }
}

fn is_bad_json<M: std::fmt::Debug>(result: Result<(M, usize), DecodeError>) -> bool {
    matches!(result, Err(DecodeError::BadJson { .. }))
}

#[test]
fn empty_buffer_is_truncated() {
    let truncated = Err(DecodeError::Truncated {
        needed: 4,
        available: 0,
    });
    assert_eq!(ClientMessage::decode(&[]).map(|_| ()), truncated);
    assert_eq!(DaemonMessage::decode(&[]).map(|_| ()), truncated);
}

#[test]
fn partial_length_prefix_is_truncated() {
    for available in 1..4 {
        let buf = vec![0; available];
        let expected = Err(DecodeError::Truncated {
            needed: 4,
            available,
        });
        assert_eq!(ClientMessage::decode(&buf).map(|_| ()), expected);
        assert_eq!(DaemonMessage::decode(&buf).map(|_| ()), expected);
    }
}

#[test]
fn partial_payload_is_truncated() {
    let buf = [0, 0, 0, 10, 1, 2, 3, 4, 5];
    let expected = Err(DecodeError::Truncated {
        needed: 14,
        available: 9,
    });
    assert_eq!(ClientMessage::decode(&buf).map(|_| ()), expected);
    assert_eq!(DaemonMessage::decode(&buf).map(|_| ()), expected);
}

#[test]
fn every_prefix_of_every_valid_frame_is_truncated() {
    let mut frames: Vec<Vec<u8>> = Vec::new();
    frames.extend(client_goldens().into_iter().map(|(_, _, g)| g));
    frames.extend(daemon_goldens().into_iter().map(|(_, _, g)| g));
    for golden in frames {
        for cut in 0..golden.len() {
            let expected = Err(DecodeError::Truncated {
                needed: if cut < 4 { 4 } else { golden.len() },
                available: cut,
            });
            assert_eq!(
                ClientMessage::decode(&golden[..cut]).map(|_| ()),
                expected,
                "cut {cut}"
            );
            assert_eq!(
                DaemonMessage::decode(&golden[..cut]).map(|_| ()),
                expected,
                "cut {cut}"
            );
        }
    }
}

#[test]
fn zero_length_is_an_empty_frame() {
    let buf = [0, 0, 0, 0, 1];
    assert_eq!(ClientMessage::decode(&buf), Err(DecodeError::EmptyFrame));
    assert_eq!(DaemonMessage::decode(&buf), Err(DecodeError::EmptyFrame));
}

#[test]
fn length_over_the_cap_is_rejected() {
    for length in [(1u32 << 20) + 1, u32::MAX] {
        let buf = length.to_be_bytes();
        let expected = Err(DecodeError::FrameTooLarge {
            length,
            max: MAX_FRAME_LEN,
        });
        assert_eq!(ClientMessage::decode(&buf).map(|_| ()), expected);
        assert_eq!(DaemonMessage::decode(&buf).map(|_| ()), expected);
    }
}

#[test]
fn length_at_the_cap_is_accepted_from_the_header() {
    let buf = MAX_FRAME_LEN.to_be_bytes();
    let expected = Err(DecodeError::Truncated {
        needed: 4 + (1 << 20),
        available: 4,
    });
    assert_eq!(ClientMessage::decode(&buf).map(|_| ()), expected);
    assert_eq!(DaemonMessage::decode(&buf).map(|_| ()), expected);
}

struct Counting<R> {
    inner: R,
    consumed: usize,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.consumed += n;
        Ok(n)
    }
}

#[test]
fn reader_rejects_oversize_length_before_reading_payload() {
    let header = ((1u32 << 20) + 1).to_be_bytes();
    let mut reader = Counting {
        inner: (&header[..]).chain(io::repeat(0)),
        consumed: 0,
    };
    let result = read_message::<ClientMessage, _>(&mut reader);
    assert!(matches!(
        result,
        Err(ReadError::Decode(DecodeError::FrameTooLarge { .. }))
    ));
    assert_eq!(reader.consumed, 4);
}

#[test]
fn unknown_kinds_are_typed_errors() {
    for kind in [0u8, 5, 6, 127, 255] {
        let buf = frame(kind, &[0; 24]);
        let expected = Err(DecodeError::UnknownKind { kind });
        assert_eq!(
            ClientMessage::decode(&buf).map(|_| ()),
            expected,
            "kind {kind}"
        );
        assert_eq!(
            DaemonMessage::decode(&buf).map(|_| ()),
            expected,
            "kind {kind}"
        );
    }
}

#[test]
fn kinds_from_the_other_direction_are_unknown() {
    assert_eq!(
        ClientMessage::decode(&frame(2, &[0; 24])),
        Err(DecodeError::UnknownKind { kind: 2 })
    );
    for kind in [3, 4] {
        assert_eq!(
            DaemonMessage::decode(&frame(kind, &[0; 24])),
            Err(DecodeError::UnknownKind { kind })
        );
    }
}

#[test]
fn every_kind_byte_fails_cleanly() {
    for kind in 0..=255u8 {
        let buf = frame(kind, &[0; 24]);
        let client = ClientMessage::decode(&buf);
        match kind {
            1 => assert!(is_bad_json(client), "client kind {kind}"),
            3 | 4 => assert!(client.is_ok(), "client kind {kind}"),
            _ => assert_eq!(client, Err(DecodeError::UnknownKind { kind })),
        }
        let daemon = DaemonMessage::decode(&buf);
        match kind {
            1 => assert!(is_bad_json(daemon), "daemon kind {kind}"),
            2 => assert!(daemon.is_ok(), "daemon kind {kind}"),
            _ => assert_eq!(daemon, Err(DecodeError::UnknownKind { kind })),
        }
    }
}

#[test]
fn short_binary_payloads_are_typed_errors() {
    for (len, kind, needed) in [(23, 2u8, 24usize), (0, 2, 24)] {
        assert_eq!(
            DaemonMessage::decode(&frame(kind, &vec![0; len])),
            Err(DecodeError::ShortPayload {
                kind,
                needed,
                actual: len
            })
        );
    }
    assert_eq!(
        ClientMessage::decode(&frame(3, &[0; 7])),
        Err(DecodeError::ShortPayload {
            kind: 3,
            needed: 8,
            actual: 7
        })
    );
    assert_eq!(
        ClientMessage::decode(&frame(4, &[0; 23])),
        Err(DecodeError::ShortPayload {
            kind: 4,
            needed: 24,
            actual: 23
        })
    );
}

fn unknown_type(name: &str) -> DecodeError {
    DecodeError::UnknownMessageType {
        type_name: name.to_owned(),
    }
}

#[test]
fn unknown_message_types_are_typed_errors() {
    let buf = control_frame(r#"{"type":"teleport","where":"mars"}"#);
    assert_eq!(ClientMessage::decode(&buf), Err(unknown_type("teleport")));
    assert_eq!(DaemonMessage::decode(&buf), Err(unknown_type("teleport")));
}

#[test]
fn message_types_from_the_other_direction_are_unknown() {
    let started = control_frame(
        r#"{"type":"started","cursor":{"attachment":7,"epoch":4,"seq":10},"backend_generation":2,"cols":80,"rows":24,"fidelity":"exact","ordered_resizes":true}"#,
    );
    assert_eq!(
        ClientMessage::decode(&started),
        Err(unknown_type("started"))
    );
    let hello = control_frame(&format!(
        r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION}}}"#
    ));
    assert_eq!(DaemonMessage::decode(&hello), Err(unknown_type("hello")));
}

#[test]
fn v5_message_types_keep_their_direction() {
    for key in [
        "get_panel",
        "report_render",
        "close_panel",
        "pin_panel",
        "mcp_open",
    ] {
        let frame = client_goldens()
            .into_iter()
            .find(|(k, _, _)| *k == key)
            .unwrap()
            .2;
        assert_eq!(
            DaemonMessage::decode(&frame),
            Err(unknown_type(key)),
            "{key}"
        );
    }
    for key in [
        "panels",
        "panel",
        "panel_closed",
        "panel_pinned",
        "mcp_opened",
    ] {
        let frame = daemon_goldens()
            .into_iter()
            .find(|(k, _, _)| *k == key)
            .unwrap()
            .2;
        assert_eq!(
            ClientMessage::decode(&frame),
            Err(unknown_type(key)),
            "{key}"
        );
    }
    let frame = control_frame(&golden_json("mcp_client"));
    assert!(ClientMessage::decode(&frame).is_ok());
    assert!(DaemonMessage::decode(&frame).is_ok());
}

#[test]
fn binary_message_names_are_not_json_types() {
    for name in ["data", "input", "automatic_reply"] {
        let buf = control_frame(&format!(r#"{{"type":"{name}","attachment":7}}"#));
        assert_eq!(ClientMessage::decode(&buf), Err(unknown_type(name)));
        assert_eq!(DaemonMessage::decode(&buf), Err(unknown_type(name)));
    }
}

#[test]
fn bad_json_is_a_typed_error() {
    let client_cases: Vec<Vec<u8>> = vec![
        control_frame("{"),
        control_frame("not json"),
        control_frame("[1,2]"),
        control_frame(r#"["hello"]"#),
        control_frame(r#""hello""#),
        control_frame("{}"),
        control_frame(r#"{"type":7}"#),
        control_frame(r#"{"type":"hello"}"#),
        control_frame(
            r#"{"type":"resize","attachment":7,"cols":-1,"rows":24,"ownership_generation":1}"#,
        ),
        control_frame(
            r#"{"type":"resize","attachment":7,"cols":70000,"rows":24,"ownership_generation":1}"#,
        ),
        control_frame(&format!(
            r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION}}} trailing"#
        )),
        frame(1, &[0xff, 0xfe]),
        frame(
            1,
            &[
                format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"x":""#)
                    .as_bytes(),
                b"\xff\xfe",
                b"\"}",
            ]
            .concat(),
        ),
    ];
    for buf in client_cases {
        assert!(is_bad_json(ClientMessage::decode(&buf)), "{buf:?}");
    }
    let daemon_cases = [
        r#"{"type":"started","cursor":{"attachment":7,"epoch":4,"seq":10},"backend_generation":2,"cols":80,"rows":24,"fidelity":"perfect","ordered_resizes":true}"#,
        r#"{"type":"error","code":"teapot","message":"","request":null,"attachment":null}"#,
        r#"{"type":"closed","cursor":{"attachment":7,"epoch":4,"seq":10},"reason":"vanished"}"#,
    ];
    for json in daemon_cases {
        assert!(
            is_bad_json(DaemonMessage::decode(&control_frame(json))),
            "{json}"
        );
    }
}

#[test]
fn control_payload_that_is_not_an_object_has_a_fixed_detail() {
    assert_eq!(
        ClientMessage::decode(&control_frame("  [1]")),
        Err(bad_json("control payload is not a JSON object"))
    );
    assert_eq!(
        ClientMessage::decode(&frame(1, &[])),
        Err(bad_json("control payload is not a JSON object"))
    );
}

#[test]
fn nested_structs_accept_array_form() {
    let buf = control_frame(r#"{"type":"ack","cursor":[7,4,9]}"#);
    assert_eq!(
        ClientMessage::decode(&buf).unwrap().0,
        ClientMessage::Ack(Ack {
            cursor: cursor(4, 9)
        })
    );
    let top_level = control_frame(r#"[7,4,9]"#);
    assert!(is_bad_json(ClientMessage::decode(&top_level)));
    assert!(is_bad_json(DaemonMessage::decode(&top_level)));
}

#[test]
fn deeply_nested_json_is_bad_json() {
    let json = format!(
        r#"{{"type":"hello","protocol_version":{}"#,
        "[".repeat(100_000)
    );
    assert!(is_bad_json(ClientMessage::decode(&control_frame(&json))));
}

#[test]
fn deep_nesting_in_an_unknown_field_is_skipped() {
    let json = format!(
        r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"x":{}{}}}"#,
        "[".repeat(100_000),
        "]".repeat(100_000)
    );
    assert_eq!(
        ClientMessage::decode(&control_frame(&json)).unwrap().0,
        ClientMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION
        })
    );
}

#[test]
fn long_error_text_is_cut() {
    let long = "x".repeat(1_000_000);
    let json = format!(r#"{{"type":"hello","protocol_version":"{long}"}}"#);
    let error = ClientMessage::decode(&control_frame(&json)).unwrap_err();
    match &error {
        DecodeError::BadJson { detail } => assert!(detail.len() <= MAX_ERROR_DETAIL),
        other => panic!("expected BadJson, got {other:?}"),
    }
    assert!(error.to_string().len() < 2048);

    let json = format!(r#"{{"type":"{}"}}"#, "t".repeat(1_000_000));
    let error = ClientMessage::decode(&control_frame(&json)).unwrap_err();
    assert_eq!(error, unknown_type(&"t".repeat(MAX_ERROR_DETAIL)));
    assert!(error.to_string().len() < 2048);

    let json = format!(r#"{{"type":"{}"}}"#, "€".repeat(100));
    let error = DaemonMessage::decode(&control_frame(&json)).unwrap_err();
    assert_eq!(error, unknown_type(&"€".repeat(85)));
    assert!(error.to_string().len() < 2048);
}

#[test]
fn stream_errors_at_message_level_leave_the_next_frame_readable() {
    let hello = ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
    });
    let mut wire = Vec::new();
    wire.extend(control_frame("not json"));
    wire.extend(frame(9, b"abc"));
    wire.extend(frame(3, &[0; 3]));
    wire.extend(control_frame(r#"{"type":"teleport"}"#));
    wire.extend(hello.encode().unwrap());
    let mut reader = &wire[..];
    let expected: [fn(&DecodeError) -> bool; 4] = [
        |e| matches!(e, DecodeError::BadJson { .. }),
        |e| matches!(e, DecodeError::UnknownKind { kind: 9 }),
        |e| matches!(e, DecodeError::ShortPayload { kind: 3, .. }),
        |e| matches!(e, DecodeError::UnknownMessageType { .. }),
    ];
    for check in expected {
        match read_message::<ClientMessage, _>(&mut reader) {
            Err(ReadError::Decode(e)) => assert!(check(&e), "{e:?}"),
            other => panic!("expected a decode error, got {other:?}"),
        }
    }
    assert_eq!(read_message(&mut reader).unwrap(), Some(hello));
}

#[test]
fn reader_reports_truncation_at_end_of_stream() {
    let mut reader: &[u8] = &[0, 0];
    assert!(matches!(
        read_message::<ClientMessage, _>(&mut reader),
        Err(ReadError::Decode(DecodeError::Truncated {
            needed: 4,
            available: 2
        }))
    ));
    let mut reader: &[u8] = &[0, 0, 0, 10, 1, 2, 3, 4, 5];
    assert!(matches!(
        read_message::<ClientMessage, _>(&mut reader),
        Err(ReadError::Decode(DecodeError::Truncated {
            needed: 14,
            available: 9
        }))
    ));
}

struct Failing {
    errors: Vec<io::ErrorKind>,
}

impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from(self.errors.remove(0)))
    }
}

#[test]
fn reader_reports_io_errors() {
    let mut reader = Failing {
        errors: vec![io::ErrorKind::ConnectionReset],
    };
    match read_message::<ClientMessage, _>(&mut reader) {
        Err(ReadError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::ConnectionReset),
        other => panic!("expected an I/O error, got {other:?}"),
    }
}

#[test]
fn reader_retries_interrupted_reads() {
    struct Flaky<'a> {
        interrupted: bool,
        rest: &'a [u8],
    }
    impl Read for Flaky<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            self.rest.read(buf)
        }
    }
    let hello = ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
    });
    let bytes = hello.encode().unwrap();
    let mut reader = Flaky {
        interrupted: false,
        rest: &bytes,
    };
    assert_eq!(read_message(&mut reader).unwrap(), Some(hello));
}

#[test]
fn oversize_messages_are_encode_errors() {
    let input = |len| {
        ClientMessage::Input(Input {
            attachment: AttachmentId(7),
            bytes: vec![0; len],
        })
    };
    assert_eq!(
        input(MAX_INPUT_CHUNK).encode().unwrap().len(),
        4 + (1 << 20)
    );
    assert_eq!(
        input(MAX_INPUT_CHUNK + 1).encode(),
        Err(EncodeError::FrameTooLarge {
            length: (1 << 20) + 1,
            max: MAX_FRAME_LEN
        })
    );
    let reply = |len| {
        ClientMessage::AutomaticReply(AutomaticReply {
            attachment: AttachmentId(7),
            epoch: Epoch(1),
            ownership_generation: OwnershipGeneration(1),
            bytes: vec![0; len],
        })
    };
    assert_eq!(
        reply(MAX_AUTOMATIC_REPLY).encode().unwrap().len(),
        4 + (1 << 20)
    );
    assert!(matches!(
        reply(MAX_AUTOMATIC_REPLY + 1).encode(),
        Err(EncodeError::FrameTooLarge { .. })
    ));
    let error = DaemonMessage::Error(ErrorMessage {
        code: ErrorCode::Internal,
        message: "x".repeat(1 << 20),
        request: None,
        attachment: None,
    });
    assert!(matches!(
        error.encode(),
        Err(EncodeError::FrameTooLarge { .. })
    ));
    let mut sink = Vec::new();
    assert!(matches!(
        write_message(&mut sink, &error),
        Err(WriteError::Encode(EncodeError::FrameTooLarge { .. }))
    ));
    assert!(sink.is_empty());
}
