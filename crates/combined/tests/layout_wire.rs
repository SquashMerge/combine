//! The conversions between the layout sync's types and the wire protocol's.

mod layout_support;

use combine_protocol as wire;
use combined::core::{
    layout_snapshot, ops_from_wire, DroppedBatch, LayoutOp, SpaceLayoutStatus, MAX_WIRE_OPS,
};
use combined::model::{LayoutRevision, SplitDir};
use layout_support::*;

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
    let snapshot = layout_snapshot(&space("/work/trellis"), &status());
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
            ratio: wire::Ratio(0.4),
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
    let snapshot = layout_snapshot(&space("s"), &status);
    assert_eq!(snapshot.dropped, None);
    assert_eq!(snapshot.error, None);
}

#[test]
fn every_wire_op_converts() {
    let ops = ops_from_wire(vec![
        set_active_op(),
        wire::LayoutOp::SetRatio {
            leaves: vec!["b".to_string(), "a".to_string()],
            ratio: wire::Ratio(0.3),
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
            ratio: wire::Ratio(0.5),
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
        ratio: wire::Ratio(0.5),
    };
    assert!(ops_from_wire(vec![bad_leaf]).is_err());
}
