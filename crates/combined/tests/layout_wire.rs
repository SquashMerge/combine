//! The conversions between the layout sync's types and the wire protocol's, and core's refusal of
//! layout messages.

mod layout_support;

use combine_protocol as wire;
use combined::core::{
    layout_snapshot, ops_from_wire, Core, CoreConfig, DroppedBatch, LayoutOp, SpaceLayoutStatus,
    MAX_WIRE_OPS,
};
use combined::model::{LayoutRevision, Millis, SplitDir};
use combined::ports::Capabilities;
use layout_support::*;

fn ratio(value: f64) -> wire::Ratio {
    wire::Ratio::new(value).unwrap()
}

fn status() -> SpaceLayoutStatus {
    SpaceLayoutStatus {
        layout: layout(split(
            0.4,
            tile("a", &["p1", "p2"], Some("p2")),
            tile("b", &[], None),
        )),
        layout_revision: LayoutRevision(7),
        read_only: true,
        error: Some("broken".to_string()),
        dropped: Some(DroppedBatch {
            op_count: 3,
            fields: Default::default(),
        }),
    }
}

fn set_active_op() -> wire::LayoutOp {
    wire::LayoutOp::SetActive {
        tile: "a".to_string(),
        panel: "p1".to_string(),
    }
}

#[test]
fn status_becomes_a_snapshot() {
    let snapshot = layout_snapshot(&space("/work/trellis"), &status()).unwrap();
    assert_eq!(snapshot.space, "/work/trellis");
    assert_eq!(snapshot.layout_revision, 7);
    assert!(snapshot.read_only);
    assert_eq!(snapshot.error.as_deref(), Some("broken"));
    let dropped = snapshot.dropped.expect("a dropped notice");
    assert_eq!(dropped.op_count, 3);
    assert!(dropped.message.contains('3'), "{}", dropped.message);
    assert_eq!(
        snapshot.root,
        wire::LayoutNode::Split {
            dir: wire::SplitDirection::H,
            ratio: ratio(0.4),
            a: Box::new(wire::LayoutNode::Tile {
                id: "a".to_string(),
                panels: vec!["p1".to_string(), "p2".to_string()],
                active: Some("p2".to_string()),
            }),
            b: Box::new(wire::LayoutNode::Tile {
                id: "b".to_string(),
                panels: vec![],
                active: None,
            }),
        }
    );
}

#[test]
fn status_without_a_notice_has_none() {
    let mut status = status();
    status.dropped = None;
    status.error = None;
    let snapshot = layout_snapshot(&space("s"), &status).unwrap();
    assert_eq!(snapshot.dropped, None);
    assert_eq!(snapshot.error, None);
}

#[test]
fn snapshot_refuses_a_ratio_the_wire_cannot_carry() {
    let mut status = status();
    status.layout = layout(split(
        f64::NAN,
        tile("a", &["p1"], Some("p1")),
        tile("b", &[], None),
    ));
    assert!(layout_snapshot(&space("s"), &status).is_err());
}

#[test]
fn every_wire_op_converts() {
    let ops = ops_from_wire(vec![
        set_active_op(),
        wire::LayoutOp::SetRatio {
            leaves: vec!["b".to_string(), "a".to_string()],
            ratio: ratio(0.3),
        },
        wire::LayoutOp::MovePanel {
            panel: "p1".to_string(),
            to_tile: "b".to_string(),
            index: 2,
        },
        wire::LayoutOp::SplitTile {
            tile: "a".to_string(),
            dir: wire::SplitDirection::V,
            new_tile: "c".to_string(),
            ratio: ratio(0.5),
            panel: Some("p2".to_string()),
        },
        wire::LayoutOp::CloseTile {
            tile: "c".to_string(),
        },
    ])
    .unwrap();
    assert_eq!(ops.len(), 5);
    assert_eq!(ops[0], set_active("a", "p1"));
    assert_eq!(ops[1], set_ratio(&["a", "b"], 0.3));
    assert_eq!(
        ops[2],
        LayoutOp::MovePanel {
            panel: p("p1"),
            to_tile: t("b"),
            index: 2,
        }
    );
    assert_eq!(
        ops[3],
        LayoutOp::SplitTile {
            tile: t("a"),
            dir: SplitDir::V,
            new_tile: t("c"),
            ratio: 0.5,
            panel: Some(p("p2")),
        }
    );
    assert_eq!(ops[4], close("c"));
}

#[test]
fn a_batch_holds_one_to_thirty_two_ops() {
    assert!(ops_from_wire(vec![]).is_err());
    assert_eq!(MAX_WIRE_OPS, 32);
    assert!(ops_from_wire(vec![set_active_op(); 32]).is_ok());
    assert!(ops_from_wire(vec![set_active_op(); 33]).is_err());
}

#[test]
fn an_invalid_id_refuses_the_batch() {
    let bad = wire::LayoutOp::CloseTile {
        tile: String::new(),
    };
    assert!(ops_from_wire(vec![set_active_op(), bad]).is_err());
    let bad_leaf = wire::LayoutOp::SetRatio {
        leaves: vec!["a\nb".to_string()],
        ratio: ratio(0.5),
    };
    assert!(ops_from_wire(vec![bad_leaf]).is_err());
}

#[test]
fn core_refuses_every_layout_message_with_invalid_request() {
    let capabilities = Capabilities {
        ordered_attach: true,
        byte_credit: false,
        initial_credit: 0,
        display_queries: true,
        automatic_replies: true,
        resync_signal: true,
        agent_state: false,
    };
    let messages = [
        (
            wire::ClientMessage::WatchLayout(wire::WatchLayout {
                request: wire::RequestId(5),
                space: "/work/repo".to_string(),
            }),
            Some(wire::RequestId(5)),
        ),
        (
            wire::ClientMessage::MutateLayout(wire::MutateLayout {
                request: wire::RequestId(6),
                space: "/work/repo".to_string(),
                base_layout_revision: 0,
                ops: vec![set_active_op()],
            }),
            Some(wire::RequestId(6)),
        ),
        (
            wire::ClientMessage::UnwatchLayout(wire::UnwatchLayout {}),
            None,
        ),
    ];
    for (message, request) in messages {
        let mut core = Core::new(CoreConfig::default(), capabilities).unwrap();
        let connection = core.connect();
        core.client_message(connection, message, Millis(0));
        let replies = core.pull(connection, usize::MAX, Millis(0));
        assert_eq!(replies.len(), 1);
        match &replies[0] {
            wire::DaemonMessage::Error(error) => {
                assert_eq!(error.code, wire::ErrorCode::InvalidRequest);
                assert_eq!(error.message, "unexpected layout message");
                assert_eq!(error.request, request);
                assert_eq!(error.attachment, None);
            }
            other => panic!("not an error: {other:?}"),
        }
    }
}
