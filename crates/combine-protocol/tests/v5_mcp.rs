//! Protocol version 5: the MCP channel.

mod common;

use combine_protocol::*;
use common::*;
use serde_json::{json, Value};

const SOURCE_LIMIT: usize = 204_800;
const NOT_AN_OBJECT: &str = "a JSON-RPC message must be a JSON object";

fn mcp_text(message: &str) -> String {
    format!(r#"{{"type":"mcp","message":{message}}}"#)
}

fn client_message(json: &str) -> Mcp {
    match decode_client(json) {
        Ok(ClientMessage::Mcp(m)) => m,
        other => panic!("{other:?}: {json}"),
    }
}

fn open_caller(json: &str) -> McpCaller {
    match decode_client(json) {
        Ok(ClientMessage::McpOpen(m)) => m.caller,
        other => panic!("{other:?}: {json}"),
    }
}

#[test]
fn mcp_open_is_answered_by_mcp_opened_with_its_request() {
    let open = match decode_client(&golden_json("mcp_open")) {
        Ok(ClientMessage::McpOpen(m)) => m,
        other => panic!("{other:?}"),
    };
    let opened = match decode_daemon(&golden_json("mcp_opened")) {
        Ok(DaemonMessage::McpOpened(m)) => m,
        other => panic!("{other:?}"),
    };
    assert_eq!(open.request, opened.request);
    assert_eq!(open.request, RequestId(1));
}

#[test]
fn caller_fields_are_each_optional() {
    let base = golden_json("mcp_open");
    for field in ["terminal_id", "terminal_socket", "cwd"] {
        for how in [None, Some(json!(null))] {
            let caller = open_caller(&patched(&base, &format!("/caller/{field}"), how));
            let full = open_caller(&base);
            let expected = McpCaller {
                terminal_id: (field != "terminal_id")
                    .then(|| full.terminal_id.clone())
                    .flatten(),
                terminal_socket: (field != "terminal_socket")
                    .then(|| full.terminal_socket.clone())
                    .flatten(),
                cwd: (field != "cwd").then(|| full.cwd.clone()).flatten(),
            };
            assert_eq!(caller, expected, "{field}");
        }
    }
    let empty = patched(&base, "/caller", Some(json!({})));
    assert_eq!(
        open_caller(&empty),
        McpCaller {
            terminal_id: None,
            terminal_socket: None,
            cwd: None,
        }
    );
}

#[test]
fn mcp_open_with_a_missing_or_mistyped_field_is_malformed() {
    let base = golden_json("mcp_open");
    for field in ["request", "caller"] {
        client_detail(&patched(&base, &format!("/{field}"), None));
    }
    for (pointer, value) in [
        ("/caller", json!("x")),
        ("/caller", json!(null)),
        ("/caller/cwd", json!(7)),
        ("/caller/terminal_id", json!([])),
        ("/caller/terminal_socket", json!({})),
        ("/request", json!("1")),
    ] {
        client_detail(&patched(&base, pointer, Some(value)));
    }
    for key in ["mcp_open", "mcp_open_cwd_only"] {
        let text = golden_json(key);
        client_detail(&text[..text.len() - 1]);
        client_detail(&format!("{text} x"));
    }
}

#[test]
fn mcp_decodes_in_both_directions() {
    let frame = control_frame(&golden_json("mcp_client"));
    let (client, _) = ClientMessage::decode(&frame).unwrap();
    let (daemon, _) = DaemonMessage::decode(&frame).unwrap();
    let (ClientMessage::Mcp(m1), DaemonMessage::Mcp(m2)) = (&client, &daemon) else {
        panic!("{client:?} {daemon:?}");
    };
    assert_eq!(m1, m2);
    assert_eq!(client.encode().unwrap(), frame);
    assert_eq!(daemon.encode().unwrap(), frame);
}

#[test]
fn mcp_message_must_be_one_json_object() {
    for message in ["[1]", "[]", "null", "7", r#""x""#, "true", r#""{\"a\":1}""#] {
        let text = mcp_text(message);
        for detail in [client_detail(&text), daemon_detail(&text)] {
            assert!(detail.contains(NOT_AN_OBJECT), "{message}: {detail}");
        }
    }
    for text in [
        r#"{"type":"mcp","message":{"a":}}"#,
        r#"{"type":"mcp","message":{"a":1}{"b":2}}"#,
    ] {
        client_detail(text);
        daemon_detail(text);
    }
    let missing = r#"{"type":"mcp"}"#;
    client_detail(missing);
    daemon_detail(missing);
    for key in ["mcp_client", "mcp_daemon"] {
        let text = golden_json(key);
        client_detail(&text[..text.len() - 1]);
        daemon_detail(&format!("{text} x"));
    }
    assert_eq!(client_message(&mcp_text("{}")).message.as_str(), "{}");
}

#[test]
fn json_rpc_message_new_takes_one_object() {
    for text in ["[1]", "null", "7"] {
        assert_eq!(
            JsonRpcMessage::new(s(text)),
            Err(DecodeError::BadJson {
                detail: s(NOT_AN_OBJECT)
            })
        );
    }
    assert_eq!(
        JsonRpcMessage::new(s(" {\"a\":1} ")).unwrap().as_str(),
        "{\"a\":1}"
    );
    assert!(matches!(
        JsonRpcMessage::new(s("not json")),
        Err(DecodeError::BadJson { .. })
    ));
    match JsonRpcMessage::new("x".repeat(1 << 20)) {
        Err(DecodeError::BadJson { detail }) => assert!(detail.len() <= MAX_ERROR_DETAIL),
        other => panic!("{other:?}"),
    }
}

#[test]
fn mcp_equality_is_by_text() {
    assert_ne!(json_rpc(r#"{"a":1,"b":2}"#), json_rpc(r#"{"b":2,"a":1}"#));
    assert_eq!(json_rpc(r#"{"a":1,"b":2}"#), json_rpc(r#"{"a":1,"b":2}"#));
}

const AC9_MESSAGE: &str = r#"{"jsonrpc":"2.0","id":"req-7","method":"tools/call","params":{"name":"create_panel","arguments":{"title":"Flow","source":"graph TD\n  A --> B\n","z_last":true,"a_first":[1,{"nested":{"deeper":[null,false,"é","é"]}}]},"_meta":{"progressToken":42}},"x-unknown":{"big":123456789012345678901234567890,"precise":0.1000000000000000055511151231257827}}"#;

#[test]
fn mcp_message_keeps_unknown_and_nested_fields_byte_for_byte() {
    let frame = control_frame(&mcp_text(AC9_MESSAGE));
    for message in [
        ClientMessage::decode(&frame).unwrap().0.encode().unwrap(),
        DaemonMessage::decode(&frame).unwrap().0.encode().unwrap(),
    ] {
        assert_eq!(message, frame);
    }
    let Ok(ClientMessage::Mcp(mcp)) = decode_client(&mcp_text(AC9_MESSAGE)) else {
        panic!("not an mcp message");
    };
    assert_eq!(mcp.message.as_str(), AC9_MESSAGE);
    assert!(mcp.message.as_str().contains(r"é"));
    let kept: Value = serde_json::from_str(mcp.message.as_str()).unwrap();
    let original: Value = serde_json::from_str(AC9_MESSAGE).unwrap();
    assert_eq!(kept, original);
}

#[test]
fn mcp_message_keeps_its_own_whitespace() {
    let text = "{\"type\":\"mcp\",\"message\": { \"a\" : 1 } }";
    let first = client_message(text);
    assert_eq!(first.message.as_str(), "{ \"a\" : 1 }");
    let encoded = ClientMessage::Mcp(first.clone()).encode().unwrap();
    let second = match ClientMessage::decode(&encoded).unwrap().0 {
        ClientMessage::Mcp(m) => m,
        other => panic!("{other:?}"),
    };
    assert_eq!(first, second);
}

#[test]
fn deep_nesting_inside_an_mcp_message_is_kept() {
    let deep = format!(
        r#"{{"type":"mcp","message":{{"a":{}{}}}}}"#,
        "[".repeat(100_000),
        "]".repeat(100_000)
    );
    let frame = control_frame(&deep);
    let (client, _) = ClientMessage::decode(&frame).unwrap();
    let (daemon, _) = DaemonMessage::decode(&frame).unwrap();
    assert_eq!(client.encode().unwrap(), frame);
    assert_eq!(daemon.encode().unwrap(), frame);

    let unbalanced = format!(
        r#"{{"type":"mcp","message":{{"a":{}}}}}"#,
        "[".repeat(100_000)
    );
    client_detail(&unbalanced);
    daemon_detail(&unbalanced);
}

#[test]
fn mcp_frames_in_one_stream_decode_one_message_each() {
    let texts = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    ];
    let mut wire = Vec::new();
    for text in texts {
        wire.extend(control_frame(&mcp_text(text)));
    }
    let mut reader = wire.as_slice();
    for text in texts {
        match read_message::<ClientMessage, _>(&mut reader).unwrap() {
            Some(ClientMessage::Mcp(m)) => assert_eq!(m.message.as_str(), text),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(read_message::<ClientMessage, _>(&mut reader).unwrap(), None);
}

fn tools_call(escaped_source_unit: &str) -> ClientMessage {
    let source = escaped_source_unit.repeat(SOURCE_LIMIT);
    let text = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"create_panel","arguments":{{"source":"{source}"}}}}}}"#
    );
    ClientMessage::Mcp(Mcp {
        message: json_rpc(&text),
    })
}

#[test]
fn a_200_kib_source_fits_an_mcp_frame_and_an_oversize_message_is_frame_too_large() {
    let fits = tools_call(r#"\""#);
    let frame = fits.encode().unwrap();
    assert_eq!(ClientMessage::decode(&frame).unwrap().0, fits);

    let too_big = tools_call(r"\u0001");
    assert!(matches!(
        too_big.encode(),
        Err(EncodeError::FrameTooLarge { .. })
    ));
    let mut sink = Vec::new();
    assert!(matches!(
        write_message(&mut sink, &too_big),
        Err(WriteError::Encode(EncodeError::FrameTooLarge { .. }))
    ));
    assert!(sink.is_empty());
}
