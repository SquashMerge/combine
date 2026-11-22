//! Protocol version 3: the layout messages and their error codes.

mod common;

use combine_protocol::*;
use common::control_frame;

const WATCH_JSON: &str = r#"{"type":"watch_layout","request":5,"space":"/Users/me/src/trellis"}"#;
const UNWATCH_JSON: &str = r#"{"type":"unwatch_layout"}"#;
const MUTATE_JSON: &str = r#"{"type":"mutate_layout","request":6,"space":"/Users/me/src/trellis","base_layout_revision":3,"ops":[{"op":"set_active","tile":"main","panel":"p1"},{"op":"set_ratio","leaves":["a","b"],"ratio":0.25},{"op":"move_panel","panel":"p1","to_tile":"b","index":0},{"op":"split_tile","tile":"a","dir":"h","new_tile":"c","ratio":0.5,"panel":null},{"op":"split_tile","tile":"a","dir":"v","new_tile":"d","ratio":0.5,"panel":"p2"},{"op":"close_tile","tile":"c"}]}"#;
const LAYOUT_JSON: &str = r#"{"type":"layout","request":null,"layout":{"space":"/Users/me/src/trellis","layout_revision":4,"root":{"type":"split","dir":"h","ratio":0.5,"a":{"type":"tile","id":"a","panels":["p1","p2"],"active":"p2"},"b":{"type":"tile","id":"b","panels":[],"active":null}},"read_only":false,"error":null,"dropped":{"op_count":2,"message":"dropped"}}}"#;

fn ratio(value: f64) -> Ratio {
    Ratio::new(value).unwrap()
}

fn s(text: &str) -> String {
    text.to_owned()
}

fn ops() -> Vec<LayoutOp> {
    vec![
        LayoutOp::SetActive {
            tile: s("main"),
            panel: s("p1"),
        },
        LayoutOp::SetRatio {
            leaves: vec![s("a"), s("b")],
            ratio: ratio(0.25),
        },
        LayoutOp::MovePanel {
            panel: s("p1"),
            to_tile: s("b"),
            index: 0,
        },
        LayoutOp::SplitTile {
            tile: s("a"),
            dir: SplitDirection::H,
            new_tile: s("c"),
            ratio: ratio(0.5),
            panel: None,
        },
        LayoutOp::SplitTile {
            tile: s("a"),
            dir: SplitDirection::V,
            new_tile: s("d"),
            ratio: ratio(0.5),
            panel: Some(s("p2")),
        },
        LayoutOp::CloseTile { tile: s("c") },
    ]
}

fn snapshot() -> LayoutSnapshot {
    LayoutSnapshot {
        space: s("/Users/me/src/trellis"),
        layout_revision: 4,
        root: LayoutNode::Split {
            dir: SplitDirection::H,
            ratio: ratio(0.5),
            a: Box::new(LayoutNode::Tile {
                id: s("a"),
                panels: vec![s("p1"), s("p2")],
                active: Some(s("p2")),
            }),
            b: Box::new(LayoutNode::Tile {
                id: s("b"),
                panels: vec![],
                active: None,
            }),
        },
        read_only: false,
        error: None,
        dropped: Some(DroppedNotice {
            op_count: 2,
            message: s("dropped"),
        }),
    }
}

#[test]
fn protocol_version_is_3() {
    assert_eq!(PROTOCOL_VERSION, 3);
}

#[test]
fn watch_layout_round_trips() {
    let frame = control_frame(WATCH_JSON);
    let (message, used) = ClientMessage::decode(&frame).unwrap();
    assert_eq!(
        message,
        ClientMessage::WatchLayout(WatchLayout {
            request: RequestId(5),
            space: s("/Users/me/src/trellis"),
        })
    );
    assert_eq!(used, frame.len());
    assert_eq!(message.type_name(), "watch_layout");
    assert_eq!(message.encode().unwrap(), frame);
}

#[test]
fn unwatch_layout_has_no_request() {
    let frame = control_frame(UNWATCH_JSON);
    let (message, _) = ClientMessage::decode(&frame).unwrap();
    assert_eq!(message, ClientMessage::UnwatchLayout(UnwatchLayout {}));
    assert_eq!(message.type_name(), "unwatch_layout");
    assert_eq!(message.encode().unwrap(), frame);
}

#[test]
fn mutate_layout_round_trips_every_op() {
    let frame = control_frame(MUTATE_JSON);
    let (message, _) = ClientMessage::decode(&frame).unwrap();
    assert_eq!(
        message,
        ClientMessage::MutateLayout(MutateLayout {
            request: RequestId(6),
            space: s("/Users/me/src/trellis"),
            base_layout_revision: 3,
            ops: ops(),
        })
    );
    assert_eq!(message.type_name(), "mutate_layout");
    assert_eq!(message.encode().unwrap(), frame);
}

#[test]
fn layout_push_has_null_request() {
    let frame = control_frame(LAYOUT_JSON);
    let (message, _) = DaemonMessage::decode(&frame).unwrap();
    assert_eq!(
        message,
        DaemonMessage::Layout(Layout {
            request: None,
            layout: snapshot(),
        })
    );
    assert_eq!(message.type_name(), "layout");
    assert_eq!(message.encode().unwrap(), frame);
    assert_eq!(message.stream_attachment(), None);

    let reply = DaemonMessage::Layout(Layout {
        request: Some(RequestId(5)),
        layout: snapshot(),
    });
    let json = String::from_utf8(reply.encode().unwrap()[5..].to_vec()).unwrap();
    assert!(
        json.starts_with(r#"{"type":"layout","request":5,"#),
        "{json}"
    );
    assert_eq!(
        DaemonMessage::decode(&reply.encode().unwrap()).unwrap().0,
        reply
    );
}

#[test]
fn unknown_op_is_malformed() {
    let json = r#"{"type":"mutate_layout","request":1,"space":"s","base_layout_revision":0,"ops":[{"op":"explode","tile":"a"}]}"#;
    assert!(matches!(
        ClientMessage::decode(&control_frame(json)),
        Err(DecodeError::BadJson { .. })
    ));
}

#[test]
fn new_error_codes_round_trip() {
    for (code, name) in [
        (ErrorCode::UnknownSpace, "unknown_space"),
        (ErrorCode::StaleLayout, "stale_layout"),
    ] {
        let message = DaemonMessage::Error(ErrorMessage {
            code,
            message: s("no"),
            request: Some(RequestId(9)),
            attachment: None,
        });
        let frame = message.encode().unwrap();
        let json = std::str::from_utf8(&frame[5..]).unwrap();
        assert!(json.contains(&format!(r#""code":"{name}""#)), "{json}");
        assert_eq!(DaemonMessage::decode(&frame).unwrap().0, message);
    }
}
